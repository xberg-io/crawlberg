//! Rate limiter implementations.

use std::sync::Mutex;
use std::time::Duration;

use ahash::AHashMap;
use async_trait::async_trait;

use crate::error::CrawlError;
use crate::time::Instant;
use crate::traits::RateLimiter;

/// Maximum backoff duration for 429 responses.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// Clean responses required before a domain's backoff is relaxed by one halving.
const SUCCESSES_BEFORE_RELAX: u32 = 5;

/// A rate limiter that does nothing, allowing all requests through immediately.
#[derive(Debug, Clone, Default)]
pub struct NoopRateLimiter;

#[async_trait]
impl RateLimiter for NoopRateLimiter {
    async fn acquire(&self, _domain: &str) -> Result<(), CrawlError> {
        Ok(())
    }

    async fn record_response(&self, _domain: &str, _status: u16) -> Result<(), CrawlError> {
        Ok(())
    }

    async fn set_crawl_delay(&self, _domain: &str, _delay: Duration) -> Result<(), CrawlError> {
        Ok(())
    }
}

/// Per-domain state tracked by [`PerDomainThrottle`].
#[derive(Debug, Clone)]
struct DomainState {
    last_request: Instant,
    crawl_delay: Option<Duration>,
    robots_delay: Option<Duration>,
    consecutive_success: u32,
}

impl DomainState {
    /// Double the delay after a 429, capped at [`MAX_BACKOFF`].
    fn back_off(&mut self, default_delay: Duration) {
        self.consecutive_success = 0;
        let current = self.crawl_delay.unwrap_or(default_delay);
        self.crawl_delay = Some((current * 2).min(MAX_BACKOFF));
    }

    /// Halve the delay once [`SUCCESSES_BEFORE_RELAX`] clean responses have accumulated,
    /// clearing it entirely when halving would take it to or below the floor.
    ///
    /// ~keep The floor is the robots-declared delay when there is one, so relaxing never
    /// undercuts a crawl-delay the site asked for; without a robots value it falls back to the
    /// configured default rather than to zero.
    fn record_success(&mut self, default_delay: Duration) {
        self.consecutive_success += 1;
        if self.consecutive_success < SUCCESSES_BEFORE_RELAX {
            return;
        }
        self.consecutive_success = 0;

        let Some(current) = self.crawl_delay else {
            return;
        };
        let floor = self.robots_delay.unwrap_or(default_delay);
        let halved = current / 2;
        self.crawl_delay = if halved <= floor { None } else { Some(halved) };
    }
}

/// How long a domain's throttle state survives without being touched.
///
/// ~keep TTL rather than LRU, deliberately: this state decays in *meaning*, not merely
/// in staleness. A domain untouched for an hour should start fresh regardless of how
/// much unrelated traffic ran in between, whereas LRU eviction order is driven by other
/// domains' request volume — the wrong signal entirely.
const DOMAIN_STATE_TTL: Duration = Duration::from_secs(3600);

/// Minimum gap between sweeps, so eviction stays amortized O(1) per `acquire`.
const SWEEP_INTERVAL: Duration = Duration::from_secs(300);

/// Per-domain throttle state plus the bookkeeping needed to expire it.
#[derive(Debug)]
struct ThrottleState {
    domains: AHashMap<String, DomainState>,
    last_sweep: Instant,
}

impl ThrottleState {
    /// Drop domains untouched for longer than [`DOMAIN_STATE_TTL`].
    ///
    /// ~keep Without this the map grows for the life of the process: a crawler running
    /// for days across many hosts never releases a single entry. Runs at most once per
    /// [`SWEEP_INTERVAL`], so the O(n) walk is amortized to O(1) per request.
    fn sweep_if_due(&mut self, now: Instant) {
        if now.duration_since(self.last_sweep) < SWEEP_INTERVAL {
            return;
        }
        let before = self.domains.len();
        // ~keep `last_request` is set into the future when a request is delayed, so
        // `duration_since` must saturate rather than panic — it does, returning zero,
        // which correctly treats a pending domain as live.
        self.domains
            .retain(|_, state| now.duration_since(state.last_request) < DOMAIN_STATE_TTL);
        self.last_sweep = now;
        let evicted = before - self.domains.len();
        if evicted > 0 {
            tracing::debug!(evicted, retained = self.domains.len(), "expired idle per-domain state");
        }
    }
}

/// A per-domain token bucket rate limiter.
///
/// Enforces a configurable delay between requests to the same domain.
/// Respects robots.txt crawl-delay via `set_crawl_delay`.
#[derive(Debug)]
pub struct PerDomainThrottle {
    default_delay: Duration,
    /// Per-domain state: last request time and optional crawl-delay override.
    state: Mutex<ThrottleState>,
}

impl PerDomainThrottle {
    /// Create a new limiter with the given default delay between requests.
    pub fn new(default_delay: Duration) -> Self {
        Self {
            default_delay,
            state: Mutex::new(ThrottleState {
                domains: AHashMap::new(),
                last_sweep: Instant::now(),
            }),
        }
    }
}

#[async_trait]
impl RateLimiter for PerDomainThrottle {
    async fn acquire(&self, domain: &str) -> Result<(), CrawlError> {
        let sleep_duration = {
            let mut state = self.state.lock().expect("lock poisoned");
            let now = Instant::now();
            state.sweep_if_due(now);
            let domain_state = state.domains.entry(domain.to_owned()).or_insert(DomainState {
                last_request: now - self.default_delay,
                crawl_delay: None,
                robots_delay: None,
                consecutive_success: 0,
            });

            let effective = match (&domain_state.crawl_delay, &domain_state.robots_delay) {
                (Some(cd), Some(rd)) => std::cmp::max(*cd, *rd),
                (Some(cd), None) => *cd,
                (None, Some(rd)) => *rd,
                (None, None) => self.default_delay,
            };

            let elapsed = now.duration_since(domain_state.last_request);

            if elapsed < effective {
                let duration = effective - elapsed;
                domain_state.last_request = now + duration;
                Some(duration)
            } else {
                domain_state.last_request = now;
                None
            }
        };

        if let Some(duration) = sleep_duration {
            tokio::time::sleep(duration).await;
        }

        Ok(())
    }

    async fn record_response(&self, domain: &str, status: u16) -> Result<(), CrawlError> {
        let mut state = self.state.lock().expect("lock poisoned");
        let Some(domain_state) = state.domains.get_mut(domain) else {
            return Ok(());
        };

        if status == 429 {
            domain_state.back_off(self.default_delay);
        } else if status < 400 {
            domain_state.record_success(self.default_delay);
        }
        Ok(())
    }

    async fn set_crawl_delay(&self, domain: &str, delay: Duration) -> Result<(), CrawlError> {
        let mut state = self.state.lock().expect("lock poisoned");
        let domain_state = state.domains.entry(domain.to_owned()).or_insert(DomainState {
            last_request: Instant::now() - self.default_delay,
            crawl_delay: None,
            robots_delay: None,
            consecutive_success: 0,
        });
        domain_state.robots_delay = Some(delay);
        domain_state.crawl_delay = Some(delay);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with(last_request: Instant) -> DomainState {
        DomainState {
            last_request,
            crawl_delay: None,
            robots_delay: None,
            consecutive_success: 0,
        }
    }

    const DEFAULT: Duration = Duration::from_millis(400);

    // ~keep `record_response` had no coverage at all before these -- neither the 429 backoff nor
    // the success-relaxation ladder -- so the whole transition table below is characterisation of
    // behaviour that already shipped, not new policy.

    #[test]
    fn should_double_the_delay_from_the_default_when_backing_off_without_one_set() {
        let mut state = state_with(Instant::now());
        state.consecutive_success = 3;

        state.back_off(DEFAULT);

        assert_eq!(state.crawl_delay, Some(DEFAULT * 2));
        assert_eq!(state.consecutive_success, 0, "a 429 restarts the success run");
    }

    #[test]
    fn should_cap_the_delay_at_max_backoff_when_doubling_would_exceed_it() {
        let mut state = state_with(Instant::now());
        state.crawl_delay = Some(MAX_BACKOFF);

        state.back_off(DEFAULT);

        assert_eq!(state.crawl_delay, Some(MAX_BACKOFF));
    }

    #[test]
    fn should_not_relax_the_delay_until_the_success_threshold_is_reached() {
        let mut state = state_with(Instant::now());
        state.crawl_delay = Some(Duration::from_secs(8));

        for _ in 0..SUCCESSES_BEFORE_RELAX - 1 {
            state.record_success(DEFAULT);
        }

        assert_eq!(state.crawl_delay, Some(Duration::from_secs(8)));
        assert_eq!(state.consecutive_success, SUCCESSES_BEFORE_RELAX - 1);
    }

    #[test]
    fn should_halve_the_delay_and_reset_the_run_when_the_threshold_is_reached() {
        let mut state = state_with(Instant::now());
        state.crawl_delay = Some(Duration::from_secs(8));

        for _ in 0..SUCCESSES_BEFORE_RELAX {
            state.record_success(DEFAULT);
        }

        assert_eq!(state.crawl_delay, Some(Duration::from_secs(4)));
        assert_eq!(state.consecutive_success, 0);
    }

    #[test]
    fn should_clear_the_delay_when_halving_reaches_the_robots_floor() {
        let mut state = state_with(Instant::now());
        state.crawl_delay = Some(Duration::from_secs(4));
        state.robots_delay = Some(Duration::from_secs(2));

        for _ in 0..SUCCESSES_BEFORE_RELAX {
            state.record_success(DEFAULT);
        }

        assert_eq!(
            state.crawl_delay, None,
            "halving to exactly the floor drops the override"
        );
    }

    #[test]
    fn should_use_the_default_delay_as_the_floor_when_robots_declares_none() {
        let mut state = state_with(Instant::now());
        state.crawl_delay = Some(DEFAULT * 2);

        for _ in 0..SUCCESSES_BEFORE_RELAX {
            state.record_success(DEFAULT);
        }

        assert_eq!(state.crawl_delay, None);
    }

    #[test]
    fn should_reset_the_success_run_at_the_threshold_even_with_no_delay_to_relax() {
        let mut state = state_with(Instant::now());

        for _ in 0..SUCCESSES_BEFORE_RELAX {
            state.record_success(DEFAULT);
        }

        assert_eq!(state.crawl_delay, None);
        assert_eq!(
            state.consecutive_success, 0,
            "the run resets on reaching the threshold whether or not a delay was set"
        );
    }

    /// ~keep Time is injected rather than slept: the TTL is an hour, so a sleeping test
    /// would either take an hour or (if shortened) assert nothing about the real value.
    #[test]
    fn sweep_drops_idle_domains_and_keeps_live_ones() {
        let now = Instant::now();
        let mut state = ThrottleState {
            domains: AHashMap::new(),
            last_sweep: now - SWEEP_INTERVAL,
        };
        state.domains.insert(
            "idle.example".to_owned(),
            state_with(now - DOMAIN_STATE_TTL - Duration::from_secs(1)),
        );
        state
            .domains
            .insert("live.example".to_owned(), state_with(now - Duration::from_secs(5)));

        state.sweep_if_due(now);

        assert!(
            !state.domains.contains_key("idle.example"),
            "a domain untouched past the TTL must be evicted, map still holds {:?}",
            state.domains.keys().collect::<Vec<_>>()
        );
        assert!(
            state.domains.contains_key("live.example"),
            "a recently-touched domain must survive, map holds {:?}",
            state.domains.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn sweep_is_skipped_until_the_interval_elapses() {
        let now = Instant::now();
        let mut state = ThrottleState {
            domains: AHashMap::new(),
            last_sweep: now,
        };
        state.domains.insert(
            "idle.example".to_owned(),
            state_with(now - DOMAIN_STATE_TTL - Duration::from_secs(1)),
        );

        state.sweep_if_due(now);

        assert_eq!(
            state.domains.len(),
            1,
            "an expired entry must survive until a sweep is actually due, so the hot path stays O(1)"
        );
    }

    #[tokio::test]
    async fn acquire_keeps_serving_a_domain_after_its_state_is_swept() {
        let throttle = PerDomainThrottle::new(Duration::from_millis(1));
        throttle
            .acquire("example.com")
            .await
            .expect("first acquire must succeed");
        {
            let mut state = throttle.state.lock().expect("lock poisoned");
            state.last_sweep = Instant::now() - SWEEP_INTERVAL - Duration::from_secs(1);
            for entry in state.domains.values_mut() {
                entry.last_request = Instant::now() - DOMAIN_STATE_TTL - Duration::from_secs(1);
            }
        }

        throttle
            .acquire("example.com")
            .await
            .expect("acquire must still succeed after the domain's state expires");

        let state = throttle.state.lock().expect("lock poisoned");
        assert_eq!(
            state.domains.len(),
            1,
            "the domain must be re-created fresh, not left evicted or duplicated"
        );
    }
}
