//! Rate limiter implementations.

use std::sync::{Arc, Mutex};
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
    /// The queue of the domain: the request that holds it sleeps out the gap, the others wait
    /// for it in the order of arrival.
    turn: Arc<tokio::sync::Mutex<()>>,
}

impl DomainState {
    /// A domain with no delay of its own, whose last request was at `last_request`.
    fn new(last_request: Instant) -> Self {
        Self {
            last_request,
            crawl_delay: None,
            robots_delay: None,
            consecutive_success: 0,
            turn: Arc::default(),
        }
    }

    /// The delay between two requests, before jitter.
    fn effective_delay(&self, default_delay: Duration) -> Duration {
        match (self.crawl_delay, self.robots_delay) {
            (Some(crawl), Some(robots)) => crawl.max(robots),
            (Some(delay), None) | (None, Some(delay)) => delay,
            (None, None) => default_delay,
        }
    }

    /// Double the delay after a 429, capped at [`MAX_BACKOFF`].
    fn back_off(&mut self, default_delay: Duration) {
        self.consecutive_success = 0;
        let current = self.crawl_delay.unwrap_or(default_delay);
        self.crawl_delay = Some(current.saturating_mul(2).min(MAX_BACKOFF));
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
    /// The state of `domain`, made on first use so that its first request does not wait.
    fn domain(&mut self, domain: &str, now: Instant, default_delay: Duration) -> &mut DomainState {
        self.domains
            .entry(domain.to_owned())
            .or_insert_with(|| DomainState::new(now.checked_sub(default_delay).unwrap_or(now)))
    }

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
        // ~keep A request in the queue holds a clone of `turn`. Its domain stays, however long
        // ~keep the wait: a new state would start a second queue beside the first.
        self.domains.retain(|_, state| {
            Arc::strong_count(&state.turn) > 1 || now.duration_since(state.last_request) < DOMAIN_STATE_TTL
        });
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
        Duration::try_from_secs_f64(duration.as_secs_f64() * factor).unwrap_or(Duration::MAX)
    }

    /// How long a request that arrives at `now` waits after the last request of the domain.
    ///
    /// ~keep Jitter can shorten the gap, but never under the delay that robots.txt asks for.
    fn wait_for(&self, state: &DomainState, domain: &str, now: Instant) -> Duration {
        let effective = state.effective_delay(self.default_delay);
        let elapsed = now.duration_since(state.last_request);
        if elapsed >= effective {
            return Duration::ZERO;
        }
        let floor = state.robots_delay.unwrap_or_default();
        self.jitter(effective, domain).max(floor).saturating_sub(elapsed)
    }
}

#[async_trait]
impl RateLimiter for PerDomainThrottle {
    async fn acquire(&self, domain: &str) -> Result<(), CrawlError> {
        let turn = {
            let mut state = self.state.lock().expect("lock poisoned");
            let now = Instant::now();
            state.sweep_if_due(now);
            state.domain(domain, now, self.default_delay).turn.clone()
        };
        // ~keep The requests of one domain queue on `turn` in the order of arrival, and the one
        // ~keep that holds it sleeps out the gap. A cancelled request drops its place in the
        // ~keep queue, or the turn itself, so it costs the requests behind it nothing. The delay
        // ~keep is read when the turn comes. A backoff therefore reaches the requests that wait
        // ~keep behind the one that holds the turn, not the one that holds it: that one has read
        // ~keep its delay already, and the next one takes the turn at the moment of its grant.
        let _turn = turn.lock().await;
        let wait = {
            let mut state = self.state.lock().expect("lock poisoned");
            let now = Instant::now();
            let domain_state = state.domain(domain, now, self.default_delay);
            self.wait_for(domain_state, domain, now)
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
        let mut state = self.state.lock().expect("lock poisoned");
        let now = Instant::now();
        state.domain(domain, now, self.default_delay).last_request = now;
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
        let domain_state = state.domain(domain, Instant::now(), self.default_delay);
        domain_state.robots_delay = Some(delay);
        domain_state.crawl_delay = Some(delay);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::task::Poll;

    use super::*;

    fn state_with(last_request: Instant) -> DomainState {
        DomainState::new(last_request)
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

    #[test]
    fn jitter_never_cuts_the_wait_under_the_robots_delay() {
        let robots = Duration::from_secs(2);
        let throttle = PerDomainThrottle::with_jitter_ratio(DEFAULT, 1.0);
        let now = Instant::now();
        let mut state = state_with(now);
        state.robots_delay = Some(robots);
        state.crawl_delay = Some(robots * 4);
        for i in 0..200 {
            let wait = throttle.wait_for(&state, &format!("domain-{i}.example"), now);
            assert!(
                wait >= robots,
                "the wait {wait:?} must not be under the robots.txt delay of {robots:?}"
            );
        }
    }

    #[test]
    fn a_delay_at_the_end_of_the_range_does_not_panic() {
        let throttle = PerDomainThrottle::with_jitter_ratio(DEFAULT, 1.0);
        let now = Instant::now();
        let mut states = ThrottleState {
            domains: AHashMap::new(),
            last_sweep: now,
        };
        let state = states.domain(HOST, now, Duration::MAX);
        state.robots_delay = Some(Duration::MAX);
        state.crawl_delay = Some(Duration::MAX);

        // ~keep Each call draws a new jitter factor, and only a factor over one is out of range.
        for _ in 0..50 {
            assert_eq!(throttle.wait_for(state, HOST, now), Duration::MAX);
        }
        state.back_off(DEFAULT);
        assert_eq!(state.crawl_delay, Some(MAX_BACKOFF));
    }

    #[test]
    fn sweep_keeps_an_idle_domain_that_has_a_request_in_its_queue() {
        let now = Instant::now();
        let mut state = ThrottleState {
            domains: AHashMap::new(),
            last_sweep: now - SWEEP_INTERVAL,
        };
        let idle = state_with(now - DOMAIN_STATE_TTL - Duration::from_secs(1));
        let _queued = idle.turn.clone();
        state.domains.insert(HOST.to_owned(), idle);

        state.sweep_if_due(now);

        assert!(
            state.domains.contains_key(HOST),
            "a domain with a request in its queue must survive the sweep"
        );
    }

    /// ~keep One gap is a minute and the tests run on the paused clock of the runtime: a
    /// ~keep sleep costs no real time. The limiter reads the real monotonic clock, which the
    /// ~keep pause does not stop, so a wait is shorter than its gap by the real time the test
    /// ~keep took. The tests read the paused clock and allow a quarter of the expected time.
    const GAP: Duration = Duration::from_secs(60);

    const HOST: &str = "one.example";

    /// The callers that wait for one host at the same time.
    const CALLERS: usize = 5;

    /// `true` when `actual` is within a quarter of `expected`.
    fn near(actual: Duration, expected: Duration) -> bool {
        actual > expected - expected / 4 && actual < expected + expected / 4
    }

    /// Poll `future` one time. Returns `true` when it still waits.
    async fn still_waits<F: Future + Unpin>(future: &mut F) -> bool {
        std::future::poll_fn(|cx| Poll::Ready(Pin::new(&mut *future).poll(cx).is_pending())).await
    }

    /// Send the first request of each host, then start `callers` acquires for each host at once.
    ///
    /// Returns, for each host, the times on the paused clock at which its callers were let
    /// through, in that order.
    async fn grant_times(
        throttle: &Arc<PerDomainThrottle>,
        hosts: &[&'static str],
        callers: usize,
    ) -> Vec<Vec<Duration>> {
        for &host in hosts {
            throttle.acquire(host).await.expect("the first request does not wait");
        }
        let started = tokio::time::Instant::now();
        let mut tasks = tokio::task::JoinSet::new();
        for (index, &host) in hosts.iter().enumerate() {
            for _ in 0..callers {
                let throttle = throttle.clone();
                tasks.spawn(async move {
                    throttle.acquire(host).await.expect("acquire must succeed");
                    (index, started.elapsed())
                });
            }
        }
        let mut granted = vec![Vec::new(); hosts.len()];
        while let Some(joined) = tasks.join_next().await {
            let (index, at) = joined.expect("the task must not panic");
            granted[index].push(at);
        }
        granted
    }

    /// Assert that the callers of `host` were let through one gap apart, the first after one gap.
    fn assert_one_gap_apart(host: &str, granted: &[Duration]) {
        assert_eq!(granted.len(), CALLERS, "every caller on {host} must be let through");
        for (turn, at) in (1u32..).zip(granted) {
            assert!(
                near(*at, GAP * turn),
                "caller {turn} on {host} must be let through {turn} gaps of {GAP:?} after the first request, got {granted:?}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn five_concurrent_callers_on_one_host_are_let_through_one_gap_apart() {
        let throttle = Arc::new(PerDomainThrottle::new(GAP));

        let granted = grant_times(&throttle, &[HOST], CALLERS).await;

        assert_one_gap_apart(HOST, &granted[0]);
    }

    // ~keep The grant times alone do not show which caller was let through. Each caller arrives
    // ~keep at its first poll; the tasks then start in the reverse order, so only the queue can
    // ~keep give each caller the turn of its arrival.
    #[tokio::test(start_paused = true)]
    async fn callers_on_one_host_are_let_through_in_the_order_of_arrival() {
        let throttle = Arc::new(PerDomainThrottle::new(GAP));
        throttle.acquire(HOST).await.expect("the first request does not wait");
        let started = tokio::time::Instant::now();
        let mut waiters = Vec::new();
        for arrival in (1u32..).take(CALLERS) {
            let throttle = throttle.clone();
            let mut waiter = Box::pin(async move {
                throttle.acquire(HOST).await.expect("acquire must succeed");
                (arrival, started.elapsed())
            });
            assert!(still_waits(&mut waiter).await, "caller {arrival} must wait");
            waiters.push(waiter);
        }
        let mut tasks = tokio::task::JoinSet::new();
        for waiter in waiters.into_iter().rev() {
            tasks.spawn(waiter);
        }
        let mut granted = Vec::new();
        while let Some(joined) = tasks.join_next().await {
            granted.push(joined.expect("the task must not panic"));
        }

        let order: Vec<u32> = granted.iter().map(|(arrival, _)| *arrival).collect();
        assert_eq!(
            order,
            (1u32..).take(CALLERS).collect::<Vec<_>>(),
            "the callers must be let through in the order of arrival, got {granted:?}"
        );
        for (arrival, at) in &granted {
            assert!(
                near(*at, GAP * *arrival),
                "caller {arrival} must be let through {arrival} gaps of {GAP:?} after the first request, got {granted:?}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_callers_on_two_hosts_queue_for_each_host_alone() {
        let throttle = Arc::new(PerDomainThrottle::new(GAP));
        let hosts = [HOST, "two.example"];

        let granted = grant_times(&throttle, &hosts, CALLERS).await;

        for (host, times) in hosts.into_iter().zip(&granted) {
            assert_one_gap_apart(host, times);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_delay_of_zero_never_waits() {
        let throttle = Arc::new(PerDomainThrottle::new(Duration::ZERO));

        let granted = grant_times(&throttle, &[HOST], CALLERS).await;

        assert_eq!(granted[0], vec![Duration::ZERO; CALLERS]);
    }

    // ~keep The API server drops a request at its timeout, so the requests that wait for one
    // ~keep domain are cancelled in the order of arrival. None of them may cost a later request a gap.
    #[tokio::test(start_paused = true)]
    async fn waiters_cancelled_in_the_order_of_arrival_cost_the_next_caller_no_gap() {
        let throttle = Arc::new(PerDomainThrottle::new(GAP));
        throttle.acquire(HOST).await.expect("the first request does not wait");
        let mut cancelled = tokio::task::JoinSet::new();
        for seconds in 1..=5 {
            let throttle = throttle.clone();
            cancelled.spawn(async move {
                tokio::time::timeout(Duration::from_secs(seconds), throttle.acquire(HOST))
                    .await
                    .is_err()
            });
        }
        while let Some(timed_out) = cancelled.join_next().await {
            assert!(
                timed_out.expect("the task must not panic"),
                "each waiter must be cancelled before its turn"
            );
        }

        let started = tokio::time::Instant::now();
        throttle.acquire(HOST).await.expect("acquire must succeed");
        let waited = started.elapsed();

        assert!(
            waited < GAP + GAP / 4,
            "5 cancelled waiters must cost the next caller no gap: it waited {waited:?}, one gap is {GAP:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_waiter_behind_a_cancelled_one_takes_its_turn() {
        let throttle = PerDomainThrottle::new(GAP);
        throttle.acquire(HOST).await.expect("the first request does not wait");
        let started = tokio::time::Instant::now();

        let mut cancelled = throttle.acquire(HOST);
        assert!(still_waits(&mut cancelled).await, "the first waiter must wait");
        let mut behind = throttle.acquire(HOST);
        assert!(still_waits(&mut behind).await, "the second waiter must wait");
        drop(cancelled);
        behind.await.expect("acquire must succeed");

        let waited = started.elapsed();
        assert!(
            near(waited, GAP),
            "the waiter behind a cancelled one must be let through after one gap of {GAP:?}, it waited {waited:?}"
        );
    }

    // ~keep The second caller is queued and does not hold the turn when it is dropped. The first
    // ~keep caller keeps the turn, and the third is let through one gap after the first send.
    #[tokio::test(start_paused = true)]
    async fn a_queued_waiter_that_is_cancelled_costs_the_caller_behind_it_no_gap() {
        let throttle = PerDomainThrottle::new(GAP);
        throttle.acquire(HOST).await.expect("the first request does not wait");
        let started = tokio::time::Instant::now();

        let mut first = Box::pin(throttle.acquire(HOST));
        assert!(still_waits(&mut first).await, "the first waiter must wait");
        let mut queued = Box::pin(throttle.acquire(HOST));
        assert!(still_waits(&mut queued).await, "the second waiter must wait");
        let mut third = Box::pin(throttle.acquire(HOST));
        assert!(still_waits(&mut third).await, "the third waiter must wait");
        drop(queued);
        first.await.expect("acquire must succeed");
        let first_sent = started.elapsed();
        third.await.expect("acquire must succeed");
        let third_sent = started.elapsed();

        assert!(
            near(first_sent, GAP),
            "the first waiter must be let through after one gap of {GAP:?}, it waited {first_sent:?}"
        );
        assert!(
            near(third_sent - first_sent, GAP),
            "the third waiter must be let through one gap of {GAP:?} after the first, got {first_sent:?} and {third_sent:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_backoff_reaches_a_request_that_already_waits() {
        let delay = GAP / 4;
        let throttle = PerDomainThrottle::new(delay);
        throttle.acquire(HOST).await.expect("the first request does not wait");
        let started = tokio::time::Instant::now();

        let mut first = throttle.acquire(HOST);
        assert!(still_waits(&mut first).await, "the first waiter must wait");
        let mut second = throttle.acquire(HOST);
        assert!(still_waits(&mut second).await, "the second waiter must wait");
        throttle.record_response(HOST, 429).await.expect("record must succeed");
        first.await.expect("acquire must succeed");
        second.await.expect("acquire must succeed");

        let waited = started.elapsed();
        assert!(
            near(waited, delay * 3),
            "the second waiter must wait one delay of {delay:?} and then the doubled delay, it waited {waited:?}"
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
