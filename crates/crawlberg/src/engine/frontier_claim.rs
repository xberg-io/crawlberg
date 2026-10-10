use std::sync::{Arc, Mutex as SyncMutex, PoisonError};

use ahash::AHashMap;
use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::error::CrawlError;
use crate::traits::{Frontier, FrontierEntry};

type KeyLocks = SyncMutex<AHashMap<String, Arc<Mutex<()>>>>;

// ~keep Existing frontiers have no atomic claim operation, so the engine lets one call at a time
// ~keep run their two-step fallback for a key. The lock is for that key alone and lives only while
// ~keep a call for the key is in flight: a frontier call that never returns holds back its own key
// ~keep and no other, and the table does not grow with the URLs the engine encounters.
pub(super) struct GuardedFrontier {
    inner: Arc<dyn Frontier>,
    in_flight: KeyLocks,
}

impl GuardedFrontier {
    pub(super) fn wrap(inner: Arc<dyn Frontier>) -> Arc<dyn Frontier> {
        Arc::new(Self::new(inner))
    }

    fn new(inner: Arc<dyn Frontier>) -> Self {
        Self {
            inner,
            in_flight: KeyLocks::default(),
        }
    }
}

/// One call's place in the queue for a key. Dropping it removes the key's lock when no other
/// call holds or awaits it, so a cancelled call leaves nothing behind.
struct KeyTurn<'a> {
    table: &'a KeyLocks,
    key: &'a str,
    lock: Arc<Mutex<()>>,
}

impl<'a> KeyTurn<'a> {
    fn enter(table: &'a KeyLocks, key: &'a str) -> Self {
        let lock = Arc::clone(
            table
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(key.to_owned())
                .or_default(),
        );
        Self { table, key, lock }
    }
}

impl Drop for KeyTurn<'_> {
    fn drop(&mut self) {
        let mut table = self.table.lock().unwrap_or_else(PoisonError::into_inner);
        // ~keep A turn is entered only under the table lock, so the count cannot rise here. Two
        // ~keep owners are the table and this turn: no other call holds or awaits the key.
        if Arc::strong_count(&self.lock) == 2 {
            table.remove(self.key);
        }
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
        let turn = KeyTurn::enter(&self.in_flight, url);
        let _held = turn.lock.lock().await;
        self.inner.mark_seen(url).await
    }

    async fn claim(&self, url: &str) -> Result<bool, CrawlError> {
        let turn = KeyTurn::enter(&self.in_flight, url);
        let _held = turn.lock.lock().await;
        self.inner.claim(url).await
    }

    fn isolated(&self) -> Option<Arc<dyn Frontier>> {
        self.inner.isolated().map(Self::wrap)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::sync::Notify;

    use super::*;
    use crate::{CrawlEngine, InMemoryFrontier};

    /// A frontier written before `claim` existed: its check yields before it answers.
    struct YieldingFrontier {
        seen: InMemoryFrontier,
        fresh_for_each_crawl: bool,
    }

    impl YieldingFrontier {
        fn new(fresh_for_each_crawl: bool) -> Self {
            Self {
                seen: InMemoryFrontier::new(),
                fresh_for_each_crawl,
            }
        }
    }

    #[async_trait]
    impl Frontier for YieldingFrontier {
        async fn push(&self, entry: FrontierEntry) -> Result<(), CrawlError> {
            self.seen.push(entry).await
        }

        async fn pop(&self) -> Result<Option<FrontierEntry>, CrawlError> {
            self.seen.pop().await
        }

        async fn len(&self) -> Result<usize, CrawlError> {
            self.seen.len().await
        }

        async fn is_seen(&self, url: &str) -> Result<bool, CrawlError> {
            let seen = self.seen.is_seen(url).await?;
            tokio::task::yield_now().await;
            Ok(seen)
        }

        async fn mark_seen(&self, url: &str) -> Result<(), CrawlError> {
            self.seen.mark_seen(url).await
        }

        fn isolated(&self) -> Option<Arc<dyn Frontier>> {
            self.fresh_for_each_crawl
                .then(|| Arc::new(Self::new(true)) as Arc<dyn Frontier>)
        }
    }

    /// A frontier written before `claim` existed whose check of one key waits for `release`.
    /// A test that never releases it has a frontier call that never returns.
    struct GatedFrontier {
        seen: InMemoryFrontier,
        gated_key: &'static str,
        entered: Notify,
        release: Notify,
    }

    impl GatedFrontier {
        fn wrapped(gated_key: &'static str) -> (Arc<Self>, Arc<dyn Frontier>) {
            let gated = Arc::new(Self {
                seen: InMemoryFrontier::new(),
                gated_key,
                entered: Notify::new(),
                release: Notify::new(),
            });
            let guarded = GuardedFrontier::wrap(Arc::clone(&gated) as Arc<dyn Frontier>);
            (gated, guarded)
        }
    }

    #[async_trait]
    impl Frontier for GatedFrontier {
        async fn push(&self, entry: FrontierEntry) -> Result<(), CrawlError> {
            self.seen.push(entry).await
        }

        async fn pop(&self) -> Result<Option<FrontierEntry>, CrawlError> {
            self.seen.pop().await
        }

        async fn len(&self) -> Result<usize, CrawlError> {
            self.seen.len().await
        }

        async fn is_seen(&self, url: &str) -> Result<bool, CrawlError> {
            if url == self.gated_key {
                self.entered.notify_one();
                self.release.notified().await;
            }
            self.seen.is_seen(url).await
        }

        async fn mark_seen(&self, url: &str) -> Result<(), CrawlError> {
            self.seen.mark_seen(url).await
        }
    }

    // ~keep Time is paused: a timeout ends only when every task waits, so no result depends on speed.
    const NEVER: Duration = Duration::from_secs(3600);

    #[tokio::test(start_paused = true)]
    async fn should_claim_other_keys_while_one_frontier_call_never_returns() {
        let (gated, frontier) = GatedFrontier::wrapped("stuck-key");
        let stuck = tokio::spawn({
            let frontier = Arc::clone(&frontier);
            async move { frontier.claim("stuck-key").await }
        });
        gated.entered.notified().await;

        for index in 0..256 {
            let key = format!("key-{index}");
            let claim = tokio::time::timeout(NEVER, frontier.claim(&key)).await;
            assert!(
                matches!(claim, Ok(Ok(true))),
                "{key} waits for the call on stuck-key: {claim:?}"
            );
        }
        assert!(!stuck.is_finished());
        stuck.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn should_keep_no_lock_for_a_key_after_its_calls_end_or_are_cancelled() {
        let gated = Arc::new(GatedFrontier {
            seen: InMemoryFrontier::new(),
            gated_key: "stuck-key",
            entered: Notify::new(),
            release: Notify::new(),
        });
        let frontier = Arc::new(GuardedFrontier::new(Arc::clone(&gated) as Arc<dyn Frontier>));
        let locks = |frontier: &GuardedFrontier| frontier.in_flight.lock().expect("table").len();

        assert!(frontier.claim("plain-key").await.expect("claim"));
        frontier.mark_seen("plain-key").await.expect("mark");
        assert_eq!(locks(&frontier), 0, "a finished call left its lock");

        let stuck = tokio::spawn({
            let frontier = Arc::clone(&frontier);
            async move { frontier.claim("stuck-key").await }
        });
        gated.entered.notified().await;
        let mut waiting = tokio::spawn({
            let frontier = Arc::clone(&frontier);
            async move { frontier.claim("stuck-key").await }
        });
        assert!(tokio::time::timeout(NEVER, &mut waiting).await.is_err());
        assert_eq!(locks(&frontier), 1, "two calls for one key share one lock");

        stuck.abort();
        assert!(stuck.await.expect_err("cancelled").is_cancelled());
        gated.entered.notified().await;
        waiting.abort();
        assert!(waiting.await.expect_err("cancelled").is_cancelled());
        assert_eq!(locks(&frontier), 0, "a cancelled call left its lock");
    }

    #[tokio::test(start_paused = true)]
    async fn should_hold_a_mark_until_the_claim_in_flight_for_its_key_ends() {
        let (gated, frontier) = GatedFrontier::wrapped("shared-key");
        let claim = tokio::spawn({
            let frontier = Arc::clone(&frontier);
            async move { frontier.claim("shared-key").await }
        });
        gated.entered.notified().await;

        let mut mark = tokio::spawn({
            let frontier = Arc::clone(&frontier);
            async move { frontier.mark_seen("shared-key").await }
        });
        assert!(
            tokio::time::timeout(NEVER, &mut mark).await.is_err(),
            "the mark ran between the check and the mark of the claim in flight"
        );
        assert!(!gated.seen.is_seen("shared-key").await.expect("seen"));

        gated.release.notify_one();
        assert!(claim.await.expect("claim task").expect("claim"));
        mark.await.expect("mark task").expect("mark");
        assert!(gated.seen.is_seen("shared-key").await.expect("seen"));
    }

    #[tokio::test]
    async fn should_claim_a_key_once_in_the_fresh_frontier_of_one_crawl() {
        let frontier = GuardedFrontier::wrap(Arc::new(YieldingFrontier::new(true)));
        let isolated = frontier.isolated().expect("fresh frontier");
        let claims = futures::future::join_all((0..8).map(|_| isolated.claim("shared-key"))).await;
        assert_eq!(
            claims
                .into_iter()
                .map(|claim| claim.expect("claim"))
                .filter(|new| *new)
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn should_share_legacy_claim_guards_across_engine_clones() {
        let engine = CrawlEngine::builder()
            .frontier(YieldingFrontier::new(false))
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
