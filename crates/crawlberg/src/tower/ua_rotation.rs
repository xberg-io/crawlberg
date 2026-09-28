//! User-Agent rotation layer for the Tower service stack.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};

use tower::{Layer, Service};

use super::types::{CrawlRequest, CrawlResponse};
use crate::error::CrawlError;

/// Tower layer that rotates User-Agent headers across requests.
#[derive(Clone)]
pub struct UaRotationLayer {
    user_agents: Arc<Vec<String>>,
    index: Arc<AtomicUsize>,
}

impl UaRotationLayer {
    pub fn new(user_agents: Vec<String>) -> Self {
        Self {
            user_agents: Arc::new(user_agents),
            index: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Advance the round-robin counter and return the next configured agent, or `None` when no
    /// rotation list is configured.
    ///
    /// ~keep The single place that decides which agent a rotating request gets. A caller that
    /// must judge robots rules before the request goes out (`RedirectPolicy::admits`) calls this
    /// once per request and pins the result onto the `CrawlRequest`, so the request this policy
    /// admits and the request the service below actually sends are the same one (crawlberg#423).
    pub(crate) fn choose_next(&self) -> Option<String> {
        if self.user_agents.is_empty() {
            return None;
        }
        let idx = self.index.fetch_add(1, Ordering::Relaxed) % self.user_agents.len();
        Some(self.user_agents[idx].clone())
    }
}

impl<S: Clone> Layer<S> for UaRotationLayer {
    type Service = UaRotationService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        UaRotationService {
            inner,
            rotation: self.clone(),
        }
    }
}

/// Tower service that injects a rotating User-Agent header into each request.
#[derive(Clone)]
pub struct UaRotationService<S> {
    inner: S,
    rotation: UaRotationLayer,
}

impl<S> Service<CrawlRequest> for UaRotationService<S>
where
    S: Service<CrawlRequest, Response = CrawlResponse, Error = CrawlError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = CrawlResponse;
    type Error = CrawlError;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: CrawlRequest) -> Self::Future {
        // ~keep A request that already names a `user-agent` chose it upstream (a robots
        // ~keep decision was made for that exact agent, or a caller wants a fixed one) and must
        // ~keep not be overwritten here, or the agent robots rules were checked against and the
        // ~keep agent this layer sends would diverge (crawlberg#423).
        if !req.headers.contains_key("user-agent")
            && let Some(ua) = self.rotation.choose_next()
        {
            req.headers.insert("user-agent".to_owned(), ua);
        }
        self.inner.call(req)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::Service;

    #[derive(Clone)]
    struct EchoService;
    impl Service<CrawlRequest> for EchoService {
        type Response = CrawlResponse;
        type Error = CrawlError;
        type Future = std::pin::Pin<Box<dyn std::future::Future<Output = Result<CrawlResponse, CrawlError>> + Send>>;
        fn poll_ready(&mut self, _: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn call(&mut self, req: CrawlRequest) -> Self::Future {
            let ua = req.headers.get("user-agent").cloned().unwrap_or_default();
            Box::pin(async move {
                Ok(CrawlResponse {
                    status: 200,
                    content_type: "text/html".into(),
                    body: ua,
                    body_bytes: vec![],
                    headers: std::collections::HashMap::new(),
                    landed_url: None,
                    sent_user_agent: None,
                })
            })
        }
    }

    #[tokio::test]
    async fn test_ua_rotation_injects_header() {
        let layer = UaRotationLayer::new(vec!["Bot/1.0".into(), "Bot/2.0".into()]);
        let mut svc = layer.layer(EchoService);

        let resp1 = svc.call(CrawlRequest::new("http://a.com")).await.unwrap();
        assert_eq!(resp1.body, "Bot/1.0");

        let resp2 = svc.call(CrawlRequest::new("http://b.com")).await.unwrap();
        assert_eq!(resp2.body, "Bot/2.0");

        let resp3 = svc.call(CrawlRequest::new("http://c.com")).await.unwrap();
        assert_eq!(resp3.body, "Bot/1.0");
    }

    #[tokio::test]
    async fn test_empty_ua_list_passes_through() {
        let layer = UaRotationLayer::new(vec![]);
        let mut svc = layer.layer(EchoService);
        let resp = svc.call(CrawlRequest::new("http://a.com")).await.unwrap();
        assert_eq!(resp.body, "");
    }
}
