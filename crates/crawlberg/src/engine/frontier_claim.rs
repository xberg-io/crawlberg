use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::error::CrawlError;
use crate::traits::{Frontier, FrontierEntry};

const CLAIM_SHARDS: usize = 64;

// ~keep Existing frontiers have no atomic claim operation. Shared, bounded lock shards protect
// ~keep their two-step fallback without retaining one lock for every URL the engine encounters.
pub(super) struct GuardedFrontier {
    inner: Arc<dyn Frontier>,
    claims: [Mutex<()>; CLAIM_SHARDS],
}

impl GuardedFrontier {
    pub(super) fn wrap(inner: Arc<dyn Frontier>) -> Arc<dyn Frontier> {
        Arc::new(Self {
            inner,
            claims: std::array::from_fn(|_| Mutex::new(())),
        })
    }

    fn shard(&self, url: &str) -> &Mutex<()> {
        let mut hasher = DefaultHasher::new();
        url.hash(&mut hasher);
        &self.claims[(hasher.finish() % CLAIM_SHARDS as u64) as usize]
    }
}

#[async_trait]
impl Frontier for GuardedFrontier {
    async fn push(&self, entry: FrontierEntry) -> Result<(), CrawlError> {
        self.inner.push(entry).await
    }

    async fn pop(&self) -> Result<Option<FrontierEntry>, CrawlError> {
        self.inner.pop().await
    }

    async fn pop_batch(&self, n: usize) -> Result<Vec<FrontierEntry>, CrawlError> {
        self.inner.pop_batch(n).await
    }

    async fn len(&self) -> Result<usize, CrawlError> {
        self.inner.len().await
    }

    async fn is_empty(&self) -> Result<bool, CrawlError> {
        self.inner.is_empty().await
    }

    async fn is_seen(&self, url: &str) -> Result<bool, CrawlError> {
        self.inner.is_seen(url).await
    }

    async fn mark_seen(&self, url: &str) -> Result<(), CrawlError> {
        let _claim = self.shard(url).lock().await;
        self.inner.mark_seen(url).await
    }

    async fn claim(&self, url: &str) -> Result<bool, CrawlError> {
        let _claim = self.shard(url).lock().await;
        self.inner.claim(url).await
    }

    fn isolated(&self) -> Option<Arc<dyn Frontier>> {
        self.inner.isolated().map(Self::wrap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CrawlEngine, InMemoryFrontier};

    struct YieldingFrontier(InMemoryFrontier);

    #[async_trait]
    impl Frontier for YieldingFrontier {
        async fn push(&self, entry: FrontierEntry) -> Result<(), CrawlError> {
            self.0.push(entry).await
        }

        async fn pop(&self) -> Result<Option<FrontierEntry>, CrawlError> {
            self.0.pop().await
        }

        async fn len(&self) -> Result<usize, CrawlError> {
            self.0.len().await
        }

        async fn is_seen(&self, url: &str) -> Result<bool, CrawlError> {
            let seen = self.0.is_seen(url).await?;
            tokio::task::yield_now().await;
            Ok(seen)
        }

        async fn mark_seen(&self, url: &str) -> Result<(), CrawlError> {
            self.0.mark_seen(url).await
        }
    }

    #[tokio::test]
    async fn should_share_legacy_claim_guards_across_engine_clones() {
        let engine = CrawlEngine::builder()
            .frontier(YieldingFrontier(InMemoryFrontier::new()))
            .build()
            .expect("engine");
        let claims = futures::future::join_all((0..8).map(|_| {
            let engine = engine.clone();
            async move { engine.frontier.claim("shared-key").await.expect("claim") }
        }))
        .await;
        assert_eq!(claims.into_iter().filter(|new| *new).count(), 1);
        assert!(engine.frontier.isolated().is_none());
        assert!(
            !engine
                .clone()
                .frontier
                .claim("shared-key")
                .await
                .expect("persistent claim")
        );
    }

    #[tokio::test]
    async fn should_give_an_isolated_frontier_fresh_claim_state() {
        let frontier = GuardedFrontier::wrap(Arc::new(InMemoryFrontier::new()));
        assert!(frontier.claim("shared-key").await.expect("first claim"));
        let isolated = frontier.isolated().expect("fresh frontier");
        assert!(isolated.claim("shared-key").await.expect("isolated claim"));
        assert!(!frontier.claim("shared-key").await.expect("original claim"));
    }
}
