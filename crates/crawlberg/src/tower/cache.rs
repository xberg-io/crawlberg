//! HTTP response cache layer for the Tower service stack.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{SystemTime, UNIX_EPOCH};

use tower::{Layer, Service};

use super::types::{CrawlRequest, CrawlResponse};
use crate::error::CrawlError;
use crate::traits::CrawlCache;
use crate::types::CachedPage;

/// Tower layer that caches HTTP responses using a [`CrawlCache`].
pub struct CrawlCacheLayer {
    cache: Arc<dyn CrawlCache>,
}

impl CrawlCacheLayer {
    pub fn new(cache: Arc<dyn CrawlCache>) -> Self {
        Self { cache }
    }
}

impl<S: Clone> Layer<S> for CrawlCacheLayer {
    type Service = CrawlCacheService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        CrawlCacheService {
            inner,
            cache: self.cache.clone(),
        }
    }
}

/// Tower service that checks the cache before forwarding requests and stores responses.
#[derive(Clone)]
pub struct CrawlCacheService<S> {
    inner: S,
    cache: Arc<dyn CrawlCache>,
}

impl<S> Service<CrawlRequest> for CrawlCacheService<S>
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
        let cache = self.cache.clone();
        let mut inner = self.inner.clone();
        std::mem::swap(&mut self.inner, &mut inner);
        let url = req.url.clone();

        Box::pin(async move {
            if let Ok(Some(cached)) = cache.get(&url).await {
                let mut headers = HashMap::new();
                if let Some(ref etag) = cached.etag {
                    headers.insert("etag".to_owned(), vec![etag.clone()]);
                }
                if let Some(ref lm) = cached.last_modified {
                    headers.insert("last-modified".to_owned(), vec![lm.clone()]);
                }
                let body_bytes = cached.body.as_bytes().to_vec();
                return Ok(CrawlResponse {
                    status: cached.status_code,
                    content_type: cached.content_type,
                    body: cached.body,
                    body_bytes,
                    headers,
                });
            }

            let resp = inner.call(req).await?;

            if resp.status >= 200 && resp.status < 300 {
                let _ = cache
                    .set(
                        &url,
                        &CachedPage {
                            url: url.clone(),
                            status_code: resp.status,
                            content_type: resp.content_type.clone(),
                            body: resp.body.clone(),
                            etag: resp.headers.get("etag").and_then(|v| v.first().cloned()),
                            last_modified: resp.headers.get("last-modified").and_then(|v| v.first().cloned()),
                            cached_at: SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs(),
                        },
                    )
                    .await;
            }

            Ok(resp)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::defaults::NoopCache;
    use tower::Service;

    #[derive(Clone)]
    struct CountingService(std::sync::Arc<std::sync::atomic::AtomicUsize>);
    impl Service<CrawlRequest> for CountingService {
        type Response = CrawlResponse;
        type Error = CrawlError;
        type Future = std::pin::Pin<Box<dyn std::future::Future<Output = Result<CrawlResponse, CrawlError>> + Send>>;
        fn poll_ready(&mut self, _: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn call(&mut self, _req: CrawlRequest) -> Self::Future {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Box::pin(async {
                Ok(CrawlResponse {
                    status: 200,
                    content_type: "text/html".into(),
                    body: "ok".into(),
                    body_bytes: vec![],
                    headers: HashMap::new(),
                })
            })
        }
    }

    #[tokio::test]
    async fn test_noop_cache_always_forwards() {
        let layer = CrawlCacheLayer::new(std::sync::Arc::new(NoopCache));
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut svc = layer.layer(CountingService(counter.clone()));

        svc.call(CrawlRequest::new("http://a.com")).await.unwrap();
        svc.call(CrawlRequest::new("http://a.com")).await.unwrap();
        assert_eq!(
            counter.load(std::sync::atomic::Ordering::Relaxed),
            2,
            "noop cache should forward all requests"
        );
    }
}
