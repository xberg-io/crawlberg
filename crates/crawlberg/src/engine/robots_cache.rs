use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, Notify};
use url::Url;

use crate::helpers::RobotsOutcome;

/// ~keep Bounds retained parsed rules while leaving room for broad multi-origin batches.
const ROBOTS_CACHE_MAX_ENTRIES: usize = 512;
/// ~keep Limits stale policy exposure and ensures transient failures are retried.
const ROBOTS_CACHE_TTL: Duration = Duration::from_secs(300);
/// ~keep Applies to a denial the origin never answered for (DNS, TLS, connect, read timeout).
/// Because this cache is shared by every crawl on the engine handle, retaining such a denial
/// for `ROBOTS_CACHE_TTL` would fail-closed unrelated crawls of that origin over one blip.
/// Short enough that a blip clears within a crawl, long enough to still coalesce a burst.
const ROBOTS_CACHE_TRANSIENT_DENIAL_TTL: Duration = Duration::from_secs(15);

#[derive(Clone, Eq, Hash, PartialEq)]
pub(crate) struct RobotsCacheKey {
    scheme: String,
    host: String,
    effective_port: u16,
    user_agent: String,
}

impl RobotsCacheKey {
    pub(crate) fn new(url: &Url, user_agent: &str) -> Self {
        Self {
            scheme: url.scheme().to_owned(),
            host: url.host_str().unwrap_or_default().to_owned(),
            effective_port: url.port_or_known_default().unwrap_or(0),
            user_agent: user_agent.to_owned(),
        }
    }
}

struct CachedOutcome {
    outcome: Arc<RobotsOutcome>,
    fetched_at: Instant,
    /// ~keep Pinned per entry rather than read from the cache at lookup time so that the
    /// freshness check cannot use a different TTL than the one the entry was admitted under.
    ttl: Duration,
}

struct CacheSlot {
    state: Mutex<Option<CachedOutcome>>,
    users: AtomicUsize,
    last_access: AtomicU64,
}

impl CacheSlot {
    fn new(last_access: u64) -> Self {
        Self {
            state: Mutex::new(None),
            users: AtomicUsize::new(1),
            last_access: AtomicU64::new(last_access),
        }
    }
}

struct CacheUse<'a> {
    cache: &'a RobotsCache,
    slot: Arc<CacheSlot>,
}

impl Drop for CacheUse<'_> {
    fn drop(&mut self) {
        self.slot.users.fetch_sub(1, Ordering::Release);
        self.cache.available.notify_one();
    }
}

pub(crate) struct RobotsCache {
    entries: Mutex<HashMap<RobotsCacheKey, Arc<CacheSlot>>>,
    available: Notify,
    access_clock: AtomicU64,
    ttl: Duration,
    transient_denial_ttl: Duration,
    max_entries: usize,
}

impl Default for RobotsCache {
    fn default() -> Self {
        Self::with_limits(
            ROBOTS_CACHE_TTL,
            ROBOTS_CACHE_TRANSIENT_DENIAL_TTL,
            ROBOTS_CACHE_MAX_ENTRIES,
        )
    }
}

impl RobotsCache {
    fn with_limits(ttl: Duration, transient_denial_ttl: Duration, max_entries: usize) -> Self {
        let max_entries = max_entries.max(1);
        Self {
            entries: Mutex::new(HashMap::with_capacity(max_entries)),
            available: Notify::new(),
            access_clock: AtomicU64::new(0),
            ttl,
            transient_denial_ttl,
            max_entries,
        }
    }

    pub(crate) async fn get_or_fetch<F, Fut>(&self, key: RobotsCacheKey, fetch: F) -> Arc<RobotsOutcome>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = RobotsOutcome>,
    {
        let cache_use = self.acquire(key).await;
        // ~keep This mutex belongs to one origin-and-user-agent slot. Holding it across the
        // ~keep fetch coalesces that key without serializing reads for unrelated origins.
        let mut state = cache_use.slot.state.lock().await;
        if let Some(cached) = state.as_ref()
            && cached.fetched_at.elapsed() < cached.ttl
        {
            return cached.outcome.clone();
        }

        let outcome = Arc::new(fetch().await);
        *state = Some(CachedOutcome {
            ttl: self.ttl_for(&outcome),
            outcome: outcome.clone(),
            fetched_at: Instant::now(),
        });
        outcome
    }

    /// How long `outcome` may be served before it must be fetched again.
    ///
    /// ~keep The split of responsibility is deliberate: `RobotsOutcome` decides what kind of
    /// failure it is, this cache decides how long each kind is worth keeping.
    fn ttl_for(&self, outcome: &RobotsOutcome) -> Duration {
        if outcome.is_transient_denial() {
            self.transient_denial_ttl
        } else {
            self.ttl
        }
    }

    async fn acquire(&self, key: RobotsCacheKey) -> CacheUse<'_> {
        loop {
            // ~keep Arm the notification before inspecting capacity so the last active slot
            // ~keep cannot become idle between the inspection and the wait, losing its wakeup.
            let notified = self.available.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let access = self.access_clock.fetch_add(1, Ordering::Relaxed);
            let mut entries = self.entries.lock().await;
            if let Some(slot) = entries.get(&key) {
                slot.users.fetch_add(1, Ordering::Acquire);
                slot.last_access.store(access, Ordering::Relaxed);
                return CacheUse {
                    cache: self,
                    slot: slot.clone(),
                };
            }

            if entries.len() >= self.max_entries {
                let evict = entries
                    .iter()
                    .filter(|(_, slot)| slot.users.load(Ordering::Acquire) == 0)
                    .min_by_key(|(_, slot)| slot.last_access.load(Ordering::Relaxed))
                    .map(|(key, _)| key.clone());
                if let Some(evict) = evict {
                    entries.remove(&evict);
                } else {
                    drop(entries);
                    notified.await;
                    continue;
                }
            }

            let slot = Arc::new(CacheSlot::new(access));
            entries.insert(key.clone(), slot.clone());
            return CacheUse { cache: self, slot };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helpers::RobotsDenial;

    const FULL_TTL: Duration = Duration::from_secs(300);
    const SHORT_TTL: Duration = Duration::from_millis(10);
    /// ~keep How long the blocked caller must stay blocked for the wait branch to be proven
    /// reached; a pass here means it did not merely lose a race to the holder.
    const BLOCKED_PROBE: Duration = Duration::from_millis(100);
    const WAKEUP_BUDGET: Duration = Duration::from_secs(5);

    fn key(url: &str, user_agent: &str) -> RobotsCacheKey {
        RobotsCacheKey::new(&Url::parse(url).expect("url"), user_agent)
    }

    #[tokio::test]
    async fn should_refetch_after_freshness_expires() {
        let cache = RobotsCache::with_limits(Duration::from_millis(10), Duration::from_millis(10), 1);
        let key = key("https://example.com/page", "crawler");
        let fetches = AtomicUsize::new(0);

        cache
            .get_or_fetch(key.clone(), || async {
                fetches.fetch_add(1, Ordering::Relaxed);
                RobotsOutcome::AllowAll
            })
            .await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        cache
            .get_or_fetch(key, || async {
                fetches.fetch_add(1, Ordering::Relaxed);
                RobotsOutcome::AllowAll
            })
            .await;

        assert_eq!(
            fetches.load(Ordering::Relaxed),
            2,
            "an expired policy must be fetched again"
        );
    }

    fn denial(denial: RobotsDenial) -> RobotsOutcome {
        RobotsOutcome::DisallowAll {
            reason: "robots.txt unreachable".to_owned(),
            denial,
        }
    }

    async fn fetches_over_a_denial(denial_kind: RobotsDenial) -> usize {
        let cache = RobotsCache::with_limits(FULL_TTL, SHORT_TTL, 1);
        let key = key("https://example.com/page", "crawler");
        let fetches = AtomicUsize::new(0);

        for _ in 0..2 {
            cache
                .get_or_fetch(key.clone(), || async {
                    fetches.fetch_add(1, Ordering::Relaxed);
                    denial(denial_kind)
                })
                .await;
            tokio::time::sleep(SHORT_TTL * 2).await;
        }
        fetches.load(Ordering::Relaxed)
    }

    #[tokio::test]
    async fn should_refetch_a_denial_when_the_origin_was_never_reached() {
        assert_eq!(
            fetches_over_a_denial(RobotsDenial::Transient).await,
            2,
            "a denial the origin never answered for must expire on the short TTL"
        );
    }

    #[tokio::test]
    async fn should_retain_a_denial_when_the_origin_refused() {
        // ~keep Negative control for the split: it fails if the short TTL is applied to every
        // denial, which is what distinguishes the split from a blanket TTL reduction.
        assert_eq!(
            fetches_over_a_denial(RobotsDenial::Sustained).await,
            1,
            "an origin's own refusal must serve the full backoff, not the short TTL"
        );
    }

    #[tokio::test]
    async fn should_wait_for_a_free_slot_when_every_entry_is_in_use() {
        let cache = Arc::new(RobotsCache::with_limits(FULL_TTL, FULL_TTL, 1));
        let holder_has_the_slot = Arc::new(Notify::new());
        let release_holder = Arc::new(Notify::new());

        let holder = tokio::spawn({
            let cache = cache.clone();
            let holder_has_the_slot = holder_has_the_slot.clone();
            let release_holder = release_holder.clone();
            async move {
                cache
                    .get_or_fetch(key("https://held.example/page", "crawler"), || async move {
                        holder_has_the_slot.notify_one();
                        release_holder.notified().await;
                        RobotsOutcome::AllowAll
                    })
                    .await;
            }
        });
        holder_has_the_slot.notified().await;

        let mut waiter = tokio::spawn({
            let cache = cache.clone();
            async move {
                cache
                    .get_or_fetch(key("https://waiting.example/page", "crawler"), || async {
                        RobotsOutcome::AllowAll
                    })
                    .await;
            }
        });

        assert!(
            tokio::time::timeout(BLOCKED_PROBE, &mut waiter).await.is_err(),
            "the only slot is held by a live caller, so a new key must block instead of evicting"
        );

        release_holder.notify_one();
        holder.await.expect("the holding task must not panic");
        tokio::time::timeout(WAKEUP_BUDGET, waiter)
            .await
            .expect("releasing the slot must wake the blocked caller")
            .expect("the waiting task must not panic");

        assert_eq!(
            cache.entries.lock().await.len(),
            1,
            "the woken caller must reuse the freed slot rather than grow the cache"
        );
    }

    #[tokio::test]
    async fn should_separate_interpretations_by_user_agent() {
        let cache = RobotsCache::with_limits(Duration::from_secs(1), Duration::from_secs(1), 2);
        let fetches = AtomicUsize::new(0);

        for user_agent in ["crawler-a", "crawler-b"] {
            cache
                .get_or_fetch(key("https://example.com/page", user_agent), || async {
                    fetches.fetch_add(1, Ordering::Relaxed);
                    RobotsOutcome::AllowAll
                })
                .await;
        }

        assert_eq!(
            fetches.load(Ordering::Relaxed),
            2,
            "different user agents must not share interpreted rules"
        );
    }

    #[tokio::test]
    async fn should_bound_retained_entries() {
        let cache = RobotsCache::with_limits(Duration::from_secs(1), Duration::from_secs(1), 2);

        for url in ["https://one.example", "https://two.example", "https://three.example"] {
            cache
                .get_or_fetch(key(url, "crawler"), || async { RobotsOutcome::AllowAll })
                .await;
        }

        assert_eq!(
            cache.entries.lock().await.len(),
            2,
            "the cache must enforce its entry cap"
        );
    }
}
