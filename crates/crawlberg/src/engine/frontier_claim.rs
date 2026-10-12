use std::sync::{Arc, Mutex as SyncMutex, PoisonError, Weak};

use ahash::AHashMap;
use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::error::CrawlError;
use crate::traits::{Frontier, FrontierEntry};

// ~keep Existing frontiers have no atomic claim operation, so the engine lets one call at a time
// ~keep run their two-step fallback for a key. The lock is for that key alone: a frontier call
// ~keep that never returns holds back its own key and no other. The calls in flight for a key own
// ~keep its lock and the table only finds it, so the lock ends with the last of those calls,
// ~keep finished or cancelled. The table does not grow with the URLs the engine encounters.
pub(super) struct GuardedFrontier {
    inner: Arc<dyn Frontier>,
    in_flight: SyncMutex<AHashMap<String, Weak<Mutex<()>>>>,
}

impl GuardedFrontier {
    pub(super) fn wrap(inner: Arc<dyn Frontier>) -> Arc<dyn Frontier> {
        Arc::new(Self::new(inner))
    }

    fn new(inner: Arc<dyn Frontier>) -> Self {
        Self {
            inner,
            in_flight: SyncMutex::default(),
        }
    }

    /// The lock that orders the calls for `key`, shared with each call in flight for it.
    ///
    /// ~keep A key whose lock has no owner left is dropped at the next lookup. No call removes
    /// ~keep its own key, so two calls that end together have no count to agree on.
    fn key_lock(&self, key: &str) -> Arc<Mutex<()>> {
        let mut locks = self.in_flight.lock().unwrap_or_else(PoisonError::into_inner);
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(key).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::default();
        locks.insert(key.to_owned(), Arc::downgrade(&lock));
        lock
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
        let lock = self.key_lock(url);
        let _held = lock.lock().await;
        self.inner.mark_seen(url).await
    }

    async fn claim(&self, url: &str) -> Result<bool, CrawlError> {
        let lock = self.key_lock(url);
        let _held = lock.lock().await;
        self.inner.claim(url).await
    }

    fn isolated(&self) -> Option<Arc<dyn Frontier>> {
        self.inner.isolated().map(Self::wrap)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::sync::{Barrier, Notify};

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

        assert!(frontier.claim("plain-key").await.expect("claim"));
        frontier.mark_seen("plain-key").await.expect("mark");
        assert_eq!(keys_listed_and_owned(&frontier).1, 0, "a finished call kept its lock");

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
        assert_eq!(
            keys_listed_and_owned(&frontier),
            (1, 1),
            "the key of the finished calls is dropped, and two calls for one key share one lock"
        );

        stuck.abort();
        assert!(stuck.await.expect_err("cancelled").is_cancelled());
        gated.entered.notified().await;
        assert_eq!(
            keys_listed_and_owned(&frontier),
            (1, 1),
            "the lock ended while a call holds it"
        );
        waiting.abort();
        assert!(waiting.await.expect_err("cancelled").is_cancelled());
        assert_eq!(keys_listed_and_owned(&frontier).1, 0, "a cancelled call kept its lock");

        assert!(frontier.claim("next-key").await.expect("claim"));
        assert_eq!(
            keys_listed_and_owned(&frontier),
            (1, 0),
            "a key with no call in flight stayed in the table after the next lookup"
        );
    }

    /// The count of keys in the table, and of those whose lock a call in flight still owns.
    fn keys_listed_and_owned(frontier: &GuardedFrontier) -> (usize, usize) {
        let locks = frontier.in_flight.lock().expect("table");
        let owned = locks.values().filter(|lock| lock.strong_count() > 0).count();
        (locks.len(), owned)
    }

    /// Eight tasks on eight threads claim one new key in each round, all at once: the calls
    /// for a key end together. Returns the count of claims that won, then the keys listed and
    /// the keys owned after the last call.
    async fn claim_each_key_on_eight_threads(inner: Arc<dyn Frontier>) -> (usize, (usize, usize)) {
        const TASKS: usize = 8;
        let frontier = Arc::new(GuardedFrontier::new(inner));
        let barrier = Arc::new(Barrier::new(TASKS));
        let tasks: Vec<_> = (0..TASKS)
            .map(|_| {
                let frontier = Arc::clone(&frontier);
                let barrier = Arc::clone(&barrier);
                tokio::spawn(async move {
                    let mut wins = 0;
                    for round in 0..CONTENDED_KEYS {
                        barrier.wait().await;
                        if frontier.claim(&format!("key-{round}")).await.expect("claim") {
                            wins += 1;
                        }
                    }
                    wins
                })
            })
            .collect();
        let mut wins = 0;
        for task in tasks {
            wins += task.await.expect("task");
        }
        (wins, keys_listed_and_owned(&frontier))
    }

    const CONTENDED_KEYS: usize = 20_000;

    // ~keep Every task ends its call for a key before any task looks up the next key, so each
    // ~keep lookup drops the key before it. Only the last key is still listed at the end.
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn should_keep_no_lock_after_eight_threads_claim_each_key_of_a_memory_frontier() {
        let (wins, left) = claim_each_key_on_eight_threads(Arc::new(InMemoryFrontier::new())).await;
        assert_eq!(left, (1, 0), "keys or locks stayed after every call ended");
        assert_eq!(wins, CONTENDED_KEYS, "a key had no winner or more than one");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn should_keep_no_lock_after_eight_threads_claim_each_key_of_a_yielding_frontier() {
        let (wins, left) = claim_each_key_on_eight_threads(Arc::new(YieldingFrontier::new(false))).await;
        assert_eq!(left, (1, 0), "keys or locks stayed after every call ended");
        assert_eq!(wins, CONTENDED_KEYS, "a key had no winner or more than one");
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
