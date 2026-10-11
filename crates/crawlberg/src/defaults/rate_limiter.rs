//! Rate limiter implementations.

use std::sync::Mutex;
use std::time::Duration;

use ahash::AHashMap;
use async_trait::async_trait;

use crate::error::CrawlError;
use crate::time::Instant;
use crate::traits::RateLimiter;

/// A pseudo-random `[0.0, 1.0)` fraction, used to jitter the per-domain delay.
///
/// ~keep `getrandom` in this crate's non-wasm dependency graph is wasm32-only (see
/// ~keep `Cargo.toml`), so there is no ready RNG. `ahash::RandomState` already carries OS
/// ~keep randomness transitively for hash-flood resistance and re-seeds on every
/// ~keep construction, so hashing anything (even a fixed `seed`) through a freshly built
/// ~keep `RandomState` yields a different, well-distributed value per call without adding
/// ~keep a new dependency purely for jitter.
fn random_unit_fraction(seed: &str) -> f64 {
    let hashed = ahash::RandomState::new().hash_one(seed);
    (hashed as f64) / (u64::MAX as f64)
}

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
    /// Fraction of the computed per-domain delay to randomly jitter by; see
    /// [`PerDomainThrottle::with_jitter_ratio`]. `0.0` (the default) applies none.
    jitter_ratio: f64,
    /// Per-domain state: last request time and optional crawl-delay override.
    state: Mutex<ThrottleState>,
}

impl PerDomainThrottle {
    /// Create a new limiter with the given default delay between requests and no jitter.
    pub fn new(default_delay: Duration) -> Self {
        Self::with_jitter_ratio(default_delay, 0.0)
    }

    /// Create a limiter that also jitters each computed per-domain delay by up to
    /// `±jitter_ratio` (clamped to `[0.0, 1.0]`), so many concurrent crawlers hitting the
    /// same domain don't all wake for their next request at the exact same instant.
    #[must_use]
    pub fn with_jitter_ratio(default_delay: Duration, jitter_ratio: f64) -> Self {
        Self {
            default_delay,
            jitter_ratio: jitter_ratio.clamp(0.0, 1.0),
            state: Mutex::new(ThrottleState {
                domains: AHashMap::new(),
                last_sweep: Instant::now(),
            }),
        }
    }

    /// Apply `jitter_ratio` to `duration`, scaling it by a factor in
    /// `[1 - jitter_ratio, 1 + jitter_ratio]`. A `jitter_ratio` of `0.0` always returns
    /// `duration` unchanged (`factor` is exactly `1.0`), so the default behaves exactly as
    /// before jitter was added.
    fn jitter(&self, duration: Duration, domain: &str) -> Duration {
        if self.jitter_ratio <= 0.0 {
            return duration;
        }
        let random_fraction = random_unit_fraction(domain);
        let factor = (1.0 + self.jitter_ratio * random_fraction.mul_add(2.0, -1.0)).max(0.0);
        Duration::from_secs_f64(duration.as_secs_f64() * factor)
    }
}

/// The slot a waiter holds while it sleeps in [`PerDomainThrottle::acquire`].
///
/// ~keep A waiter dropped before its slot comes (its request was cancelled) gives the slot
/// ~keep back when it is the last in the queue, so the next caller takes it. A waiter with
/// ~keep others queued behind it cannot give its slot back: they already sleep toward fixed
/// ~keep times, so its slot stays empty and it costs the queue one gap at most.
struct SlotReservation<'a> {
    throttle: &'a PerDomainThrottle,
    domain: &'a str,
    slot: Instant,
    previous: Instant,
    /// Time left until the slot; zero once the waiter has slept to it.
    wait: Duration,
}

impl Drop for SlotReservation<'_> {
    fn drop(&mut self) {
        if self.wait.is_zero() {
            return;
        }
        let Ok(mut state) = self.throttle.state.lock() else {
            return;
        };
        if let Some(domain_state) = state.domains.get_mut(self.domain)
            && domain_state.last_request == self.slot
        {
            domain_state.last_request = self.previous;
        }
    }
}

#[async_trait]
impl RateLimiter for PerDomainThrottle {
    async fn acquire(&self, domain: &str) -> Result<(), CrawlError> {
        let reservation = {
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

            // ~keep The slot is reserved here, under the lock and before the sleep, so each
            // ~keep waiter queues one gap after the waiter before it. `last_request` is the last
            // ~keep reserved slot and can be in the future.
            if now.duration_since(domain_state.last_request) >= effective {
                domain_state.last_request = now;
                None
            } else {
                let previous = domain_state.last_request;
                let slot = previous + self.jitter(effective, domain);
                if slot <= now {
                    domain_state.last_request = now;
                    None
                } else {
                    domain_state.last_request = slot;
                    Some(SlotReservation {
                        throttle: self,
                        domain,
                        slot,
                        previous,
                        wait: slot.duration_since(now),
                    })
                }
            }
        };

        if let Some(mut reservation) = reservation {
            tokio::time::sleep(reservation.wait).await;
            reservation.wait = Duration::ZERO;
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
    use std::pin::Pin;
    use std::sync::Arc;
    use std::task::Poll;

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

    #[test]
    fn jitter_ratio_zero_never_changes_the_duration() {
        let throttle = PerDomainThrottle::new(DEFAULT);
        for _ in 0..20 {
            assert_eq!(
                throttle.jitter(Duration::from_millis(400), "example.com"),
                Duration::from_millis(400),
                "jitter_ratio=0.0 must be a no-op"
            );
        }
    }

    #[test]
    fn jitter_ratio_stays_within_the_configured_bound() {
        let base = Duration::from_millis(1000);
        let throttle = PerDomainThrottle::with_jitter_ratio(DEFAULT, 0.2);
        let lower = base.mul_f64(0.8);
        let upper = base.mul_f64(1.2);
        for i in 0..50 {
            let domain = format!("domain-{i}.example");
            let jittered = throttle.jitter(base, &domain);
            assert!(
                jittered >= lower && jittered <= upper,
                "jittered duration {jittered:?} must stay within [{lower:?}, {upper:?}]"
            );
        }
    }

    #[test]
    fn jitter_ratio_is_clamped_to_one() {
        // ~keep A ratio > 1.0 could otherwise scale the factor negative; `Duration::from_secs_f64`
        // ~keep panics on a negative value, so the clamp in `with_jitter_ratio` must hold.
        let throttle = PerDomainThrottle::with_jitter_ratio(DEFAULT, 5.0);
        let jittered = throttle.jitter(Duration::from_millis(500), "example.com");
        assert!(jittered <= Duration::from_millis(1000), "got {jittered:?}");
    }

    /// ~keep One gap is a minute and the tests run on the paused clock of the runtime: a
    /// ~keep sleep to a slot costs no real time, and a stall of the host under a minute cannot
    /// ~keep move a result. The limiter reads the system clock, which the pause does not stop,
    /// ~keep so the tests read the reserved slot in the state and the paused clock, never wall time.
    const GAP: Duration = Duration::from_secs(60);

    const HOST: &str = "one.example";

    /// The last reserved slot of `host`.
    fn reserved_slot(throttle: &PerDomainThrottle, host: &str) -> Instant {
        let state = throttle.state.lock().expect("lock poisoned");
        let domain_state = state.domains.get(host).expect("the host has state");
        domain_state.last_request
    }

    /// Poll `future` one time. Returns `true` when it still waits.
    async fn still_waits<F: Future + Unpin>(future: &mut F) -> bool {
        std::future::poll_fn(|cx| Poll::Ready(Pin::new(&mut *future).poll(cx).is_pending())).await
    }

    /// Take the free first slot of each host, then start `callers` acquires for each host at once.
    ///
    /// Returns the first slot of each host and the time on the paused clock until all callers
    /// hold a slot.
    async fn queue_concurrent_acquires(
        throttle: &Arc<PerDomainThrottle>,
        hosts: &[&'static str],
        callers: usize,
    ) -> (Vec<Instant>, Duration) {
        let mut first_slots = Vec::with_capacity(hosts.len());
        for &host in hosts {
            throttle.acquire(host).await.expect("the first slot is free");
            first_slots.push(reserved_slot(throttle, host));
        }
        let started = tokio::time::Instant::now();
        let mut tasks = tokio::task::JoinSet::new();
        for &host in hosts {
            for _ in 0..callers {
                let throttle = throttle.clone();
                tasks.spawn(async move { throttle.acquire(host).await });
            }
        }
        while let Some(joined) = tasks.join_next().await {
            joined.expect("the task must not panic").expect("acquire must succeed");
        }
        (first_slots, started.elapsed())
    }

    #[tokio::test(start_paused = true)]
    async fn five_concurrent_callers_on_one_host_each_take_the_next_slot() {
        let throttle = Arc::new(PerDomainThrottle::new(GAP));

        let (first_slots, slept) = queue_concurrent_acquires(&throttle, &[HOST], 5).await;

        assert_eq!(
            reserved_slot(&throttle, HOST),
            first_slots[0] + GAP * 5,
            "5 callers behind the first slot must reserve 5 slots, one gap of {GAP:?} apart"
        );
        assert!(
            slept > GAP * 4 && slept < GAP * 6,
            "the last caller must sleep to the fifth slot, all held a slot after {slept:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_callers_on_two_hosts_queue_for_each_host_alone() {
        let throttle = Arc::new(PerDomainThrottle::new(GAP));
        let hosts = [HOST, "two.example"];

        let (first_slots, slept) = queue_concurrent_acquires(&throttle, &hosts, 5).await;

        for (host, first_slot) in hosts.into_iter().zip(first_slots) {
            assert_eq!(
                reserved_slot(&throttle, host),
                first_slot + GAP * 5,
                "5 callers on {host} must reserve 5 slots of that host, one gap of {GAP:?} apart"
            );
        }
        assert!(
            slept > GAP * 4 && slept < GAP * 6,
            "the two hosts must not share one queue (10 gaps), all held a slot after {slept:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_cancelled_last_waiter_gives_its_slot_back() {
        let throttle = PerDomainThrottle::new(GAP);
        throttle.acquire(HOST).await.expect("the first slot is free");
        let first_slot = reserved_slot(&throttle, HOST);

        let mut waiter = throttle.acquire(HOST);
        assert!(still_waits(&mut waiter).await, "the waiter must wait for its slot");
        assert_eq!(
            reserved_slot(&throttle, HOST),
            first_slot + GAP,
            "the waiter must hold the next slot"
        );
        drop(waiter);

        assert_eq!(
            reserved_slot(&throttle, HOST),
            first_slot,
            "the cancelled last waiter must give its slot back"
        );
        let mut next = throttle.acquire(HOST);
        assert!(still_waits(&mut next).await, "the next caller must wait for a slot");
        assert_eq!(
            reserved_slot(&throttle, HOST),
            first_slot + GAP,
            "the next caller must take the slot that the cancelled waiter gave back"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_cancelled_waiter_with_another_behind_it_leaves_its_slot_empty() {
        let throttle = PerDomainThrottle::new(GAP);
        throttle.acquire(HOST).await.expect("the first slot is free");
        let first_slot = reserved_slot(&throttle, HOST);

        let mut cancelled = throttle.acquire(HOST);
        assert!(still_waits(&mut cancelled).await, "the first waiter must wait");
        let mut behind = throttle.acquire(HOST);
        assert!(still_waits(&mut behind).await, "the second waiter must wait");
        drop(cancelled);

        assert_eq!(
            reserved_slot(&throttle, HOST),
            first_slot + GAP * 2,
            "the waiter behind the cancelled one must keep its slot"
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
