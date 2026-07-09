//! Per-domain rate limiting layer for the Tower service stack.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tower::{Layer, Service};

use super::types::{CrawlRequest, CrawlResponse};
use crate::error::CrawlError;
use crate::traits::RateLimiter;

/// Tower layer that applies per-domain rate limiting using a [`RateLimiter`].
pub struct PerDomainRateLimitLayer {
    rate_limiter: Arc<dyn RateLimiter>,
}

impl PerDomainRateLimitLayer {
    pub fn new(rate_limiter: Arc<dyn RateLimiter>) -> Self {
        Self { rate_limiter }
    }
}

impl<S: Clone> Layer<S> for PerDomainRateLimitLayer {
    type Service = PerDomainRateLimitService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        PerDomainRateLimitService {
            inner,
            rate_limiter: self.rate_limiter.clone(),
        }
    }
}

/// Tower service that enforces per-domain rate limits before forwarding requests.
#[derive(Clone)]
pub struct PerDomainRateLimitService<S> {
    inner: S,
    rate_limiter: Arc<dyn RateLimiter>,
}

impl<S> Service<CrawlRequest> for PerDomainRateLimitService<S>
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
        let domain = req.domain().unwrap_or_default();
        let rate_limiter = self.rate_limiter.clone();
        let mut inner = self.inner.clone();
        std::mem::swap(&mut self.inner, &mut inner);

        Box::pin(async move {
            if !domain.is_empty() {
                rate_limiter.acquire(&domain).await?;
            }

            let resp = inner.call(req).await?;

            if !domain.is_empty() {
                rate_limiter.record_response(&domain, resp.status).await?;
            }

            Ok(resp)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::defaults::NoopRateLimiter;
    use tower::Service;

    #[derive(Clone)]
    struct OkService;
    impl Service<CrawlRequest> for OkService {
        type Response = CrawlResponse;
        type Error = CrawlError;
        type Future = std::pin::Pin<Box<dyn std::future::Future<Output = Result<CrawlResponse, CrawlError>> + Send>>;
        fn poll_ready(&mut self, _: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn call(&mut self, _: CrawlRequest) -> Self::Future {
            Box::pin(async {
                Ok(CrawlResponse {
                    status: 200,
                    content_type: "text/html".into(),
                    body: "ok".into(),
                    body_bytes: vec![],
                    headers: std::collections::HashMap::new(),
                })
            })
        }
    }

    #[tokio::test]
    async fn test_rate_limit_layer_passes_through() {
        let layer = PerDomainRateLimitLayer::new(std::sync::Arc::new(NoopRateLimiter));
        let mut svc = layer.layer(OkService);
        let resp = svc.call(CrawlRequest::new("http://example.com")).await.unwrap();
        assert_eq!(resp.status, 200);
    }
}
