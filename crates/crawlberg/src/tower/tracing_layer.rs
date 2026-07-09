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

        let span = tracing::info_span!(
            "crawl.page.fetch",
            otel.kind = "client",
            { HTTP_REQUEST_METHOD } = "GET",
            { URL_FULL } = %url,
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
                            CrawlError::Timeout(_) | CrawlError::BrowserTimeout(_) => "timeout",
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
