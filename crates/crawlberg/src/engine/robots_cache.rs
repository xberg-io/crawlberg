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
    max_entries: usize,
}

impl Default for RobotsCache {
    fn default() -> Self {
        Self::with_limits(ROBOTS_CACHE_TTL, ROBOTS_CACHE_MAX_ENTRIES)
    }
}

impl RobotsCache {
    fn with_limits(ttl: Duration, max_entries: usize) -> Self {
        let max_entries = max_entries.max(1);
        Self {
            entries: Mutex::new(HashMap::with_capacity(max_entries)),
            available: Notify::new(),
            access_clock: AtomicU64::new(0),
            ttl,
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
            && cached.fetched_at.elapsed() < self.ttl
        {
            return cached.outcome.clone();
        }

        let outcome = Arc::new(fetch().await);
        *state = Some(CachedOutcome {
            outcome: outcome.clone(),
            fetched_at: Instant::now(),
        });
        outcome
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

    fn key(url: &str, user_agent: &str) -> RobotsCacheKey {
        RobotsCacheKey::new(&Url::parse(url).expect("url"), user_agent)
    }

    #[tokio::test]
    async fn should_refetch_after_freshness_expires() {
        let cache = RobotsCache::with_limits(Duration::from_millis(10), 1);
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

    #[tokio::test]
    async fn should_separate_interpretations_by_user_agent() {
        let cache = RobotsCache::with_limits(Duration::from_secs(1), 2);
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
        let cache = RobotsCache::with_limits(Duration::from_secs(1), 2);

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
