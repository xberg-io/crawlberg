//! Tracing/telemetry layer for the Tower service stack.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Instant;

use opentelemetry::KeyValue;
use tower::{Layer, Service};
use tracing::Instrument;

use super::types::{CrawlRequest, CrawlResponse};
use crate::error::CrawlError;
use crate::telemetry::attributes::{
    CRAWL_TIER, HTTP_REQUEST_METHOD, HTTP_RESPONSE_BODY_SIZE, HTTP_RESPONSE_STATUS_CODE, SERVER_ADDRESS, URL_FULL,
};
use crate::telemetry::metrics::registry;

/// Tower layer that emits `tracing` spans for each crawl request.
pub struct CrawlTracingLayer;

impl CrawlTracingLayer {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CrawlTracingLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: Clone> Layer<S> for CrawlTracingLayer {
    type Service = CrawlTracingService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        CrawlTracingService { inner }
    }
}

/// Tower service that wraps each request in a `tracing` span with HTTP metadata.
#[derive(Clone)]
pub struct CrawlTracingService<S> {
    inner: S,
}

impl<S> Service<CrawlRequest> for CrawlTracingService<S>
where
    S: Service<CrawlRequest, Response = CrawlResponse, Error = CrawlError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = CrawlResponse;
    type Error = CrawlError;
    type Future = Pin<Box<dyn Future<Output = Result<CrawlResponse, CrawlError>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: CrawlRequest) -> Self::Future {
        let host = req.domain().unwrap_or_default();
        let url = req.url.clone();
        let tier = req.tier;

        // ~keep A userinfo-embedded credential (http://user:pass@host/) must never reach a
        // span field: spans are shipped to logs/OTLP by default, unlike an error message
        // that might be swallowed.
        let redacted_url = crate::net::redact_url_credentials(&url);
        let span = tracing::info_span!(
            "crawl.page.fetch",
            otel.kind = "client",
            { HTTP_REQUEST_METHOD } = "GET",
            { URL_FULL } = %redacted_url,
            { SERVER_ADDRESS } = %host,
            { HTTP_RESPONSE_STATUS_CODE } = tracing::field::Empty,
            { HTTP_RESPONSE_BODY_SIZE } = tracing::field::Empty,
            { CRAWL_TIER } = tracing::field::Empty,
        );

        let mut inner = self.inner.clone();
        std::mem::swap(&mut self.inner, &mut inner);

        Box::pin(
            async move {
                if let Some(t) = tier {
                    tracing::Span::current().record(CRAWL_TIER, t);
                }
                let started = Instant::now();
                let result = inner.call(req).await;
                let elapsed = started.elapsed();

                match result {
                    Ok(resp) => {
                        let span = tracing::Span::current();
                        span.record(HTTP_RESPONSE_STATUS_CODE, resp.status as i64);
                        span.record(HTTP_RESPONSE_BODY_SIZE, resp.body_bytes.len() as i64);

                        let status_label = if resp.status < 400 { "ok" } else { "http_error" };
                        registry().pages_total.add(1, &[KeyValue::new("status", status_label)]);
                        registry()
                            .pages_duration_seconds
                            .record(elapsed.as_secs_f64(), &[KeyValue::new("host", host)]);

                        tracing::info!(
                            status = resp.status,
                            body_size = resp.body_bytes.len(),
                            "fetch complete"
                        );
                        Ok(resp)
                    }
                    Err(ref e) => {
                        let status_label = match e {
                            CrawlError::Timeout { .. } | CrawlError::BrowserTimeout { .. } => "timeout",
                            _ => "http_error",
                        };
                        registry().pages_total.add(1, &[KeyValue::new("status", status_label)]);
                        registry()
                            .pages_duration_seconds
                            .record(elapsed.as_secs_f64(), &[KeyValue::new("host", host)]);
                        result
                    }
                }
            }
            .instrument(span),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tower::{Layer, ServiceExt, service_fn};
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Metadata};

    use super::*;

    /// `Visit` that records every field name/value pair it sees, formatted with `Debug`
    /// (which is how tracing dispatches both `%value` and plain `Display`/`Debug` fields).
    struct FieldVisitor<'a>(&'a mut Vec<(String, String)>);

    impl Visit for FieldVisitor<'_> {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.0.push((field.name().to_owned(), format!("{value:?}")));
        }
    }

    /// Minimal `tracing::Subscriber` that captures every span's fields into `sink`, so a
    /// test can assert on the exact value recorded for a given field name without pulling
    /// in a `tracing-subscriber` dev-dependency.
    struct CapturingSubscriber {
        sink: Arc<Mutex<Vec<(String, String)>>>,
    }

    impl tracing::Subscriber for CapturingSubscriber {
        fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, attrs: &Attributes<'_>) -> Id {
            let mut fields = self.sink.lock().expect("sink mutex must not be poisoned");
            attrs.record(&mut FieldVisitor(&mut fields));
            Id::from_u64(1)
        }

        fn record(&self, _span: &Id, _values: &Record<'_>) {}
        fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

        fn event(&self, event: &Event<'_>) {
            let mut fields = self.sink.lock().expect("sink mutex must not be poisoned");
            event.record(&mut FieldVisitor(&mut fields));
        }

        fn enter(&self, _span: &Id) {}
        fn exit(&self, _span: &Id) {}
    }

    #[tokio::test]
    async fn call_redacts_url_credentials_on_the_fetch_span() {
        // ~keep #[tokio::test] defaults to a current-thread runtime, so the whole
        // `.await` stays on the thread the subscriber guard was set on.
        let sink: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let _guard = tracing::subscriber::set_default(CapturingSubscriber { sink: sink.clone() });

        let inner = service_fn(|_req: CrawlRequest| async {
            Ok::<_, CrawlError>(CrawlResponse {
                status: 200,
                content_type: String::new(),
                body: String::new(),
                body_bytes: Vec::new(),
                headers: std::collections::HashMap::new(),
            })
        });
        let svc = CrawlTracingLayer::new().layer(inner);

        let req = CrawlRequest::new("http://user:hunter2@example.com/page");
        svc.oneshot(req).await.expect("stubbed inner service always succeeds");

        let recorded = sink.lock().expect("sink mutex must not be poisoned");
        let url_full = recorded
            .iter()
            .find(|(name, _)| name == URL_FULL)
            .map(|(_, value)| value.clone())
            .unwrap_or_else(|| panic!("expected a '{URL_FULL}' field on the crawl.page.fetch span, got {recorded:?}"));

        assert!(
            !url_full.contains("hunter2"),
            "span field '{URL_FULL}' must not contain the raw password, got '{url_full}'"
        );
        assert_eq!(
            url_full, "http://***:***@example.com/page",
            "unexpected redacted form in span field '{URL_FULL}'"
        );
    }
}
