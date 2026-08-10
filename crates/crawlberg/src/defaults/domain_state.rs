//! In-process domain-state backend, EWMA utility, and the learning
//! retry policy that consults a [`DomainStatePort`] for prior block
//! rates.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use dashmap::DashMap;

use crate::types::{
    AttemptOutcome, DomainObservation, DomainRecommendation, DomainStatePort, ObservedOutcome, RetryDirective,
    RetryPolicy, Tier,
};

use super::dispatch::SimpleRetryPolicy;

/// Pure-math EWMA with promote/demote thresholds. Stateless — caller
/// supplies the prior and the observation.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(alef, alef(skip))]
pub struct EwmaTracker {
    /// Smoothing factor 0.0 < alpha < 1.0. Higher = react faster.
    alpha: f32,
    /// Block rate above which the dispatcher should promote starting_tier.
    promote_threshold: f32,
    /// Block rate below which the dispatcher should demote starting_tier.
    demote_threshold: f32,
    /// Minimum sample count before promotion takes effect.
    min_samples_promote: u64,
    /// Minimum sample count before demotion takes effect.
    min_samples_demote: u64,
}

impl EwmaTracker {
    /// Recommended defaults: alpha=0.1 (~72h half-life at typical rates),
    /// promote at 0.4 / 10 samples, demote at 0.1 / 50 samples.
    pub const fn new() -> Self {
        Self {
            alpha: 0.1,
            promote_threshold: 0.4,
            demote_threshold: 0.1,
            min_samples_promote: 10,
            min_samples_demote: 50,
        }
    }

    /// Update the EWMA given whether the current observation was a block.
    ///
    /// # Panics (debug only)
    ///
    /// Panics in debug builds if `self.alpha` is outside `(0.0, 1.0)`. The
    /// constructor hardcodes `0.1`; this assert guards any future `with_alpha`
    /// builder from silently breaking the weighted-average math.
    pub fn update(&self, prev: f32, blocked: bool) -> f32 {
        debug_assert!(
            self.alpha > 0.0 && self.alpha < 1.0,
            "EwmaTracker alpha must be in (0.0, 1.0); got {}",
            self.alpha
        );
        let observation = if blocked { 1.0 } else { 0.0 };
        self.alpha.mul_add(observation, (1.0 - self.alpha) * prev)
    }

    /// Return `true` if the EWMA and sample count warrant promoting to a
    /// higher tier.
    pub fn should_promote(&self, ewma: f32, sample_count: u64) -> bool {
        sample_count >= self.min_samples_promote && ewma >= self.promote_threshold
    }

    /// Return `true` if the EWMA and sample count warrant demoting to a
    /// lower tier.
    pub fn should_demote(&self, ewma: f32, sample_count: u64) -> bool {
        sample_count >= self.min_samples_demote && ewma <= self.demote_threshold
    }
}

impl Default for EwmaTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Internal snapshot stored per domain in the DashMap.
#[derive(Debug, Clone)]
struct DomainSnapshot {
    block_ewma: f32,
    sample_count: u64,
    /// Lowercase WAF vendor identifier, if one has been observed.
    classifier: Option<String>,
    starting_tier: Tier,
    /// When this domain was last observed, used to expire idle entries.
    last_touched: Instant,
}

/// How long a domain's learned state survives without being observed.
///
/// ~keep TTL rather than LRU, deliberately: a learned block-rate decays in *meaning*,
/// not merely in staleness. A domain unseen for an hour should be re-learned from
/// scratch regardless of how much unrelated traffic ran in between, whereas LRU
/// eviction order is driven by other domains' request volume — the wrong signal.
const DOMAIN_STATE_TTL: Duration = Duration::from_secs(3600);

/// Minimum gap between sweeps, keeping eviction amortized O(1) per `observe`.
const SWEEP_INTERVAL: Duration = Duration::from_secs(300);

/// Process-local domain state backed by an EWMA block-rate model.
/// `DashMap`-backed, ephemeral — no persistence across restarts.
/// For multi-process / multi-tenant learning, use xberg-enterprise's
/// PostgresDomainState.
#[derive(Debug, Default)]
#[cfg_attr(alef, alef(skip))]
pub struct EwmaDomainState {
    inner: DashMap<String, DomainSnapshot>,
    ewma: EwmaTracker,
    last_sweep: Mutex<Option<Instant>>,
}

impl EwmaDomainState {
    /// Drop domains unobserved for longer than [`DOMAIN_STATE_TTL`].
    ///
    /// ~keep Without this the map grows for the life of the process. Runs at most once
    /// per [`SWEEP_INTERVAL`], so the O(n) `retain` is amortized to O(1) per observation.
    fn sweep_if_due(&self, now: Instant) {
        {
            let mut last_sweep = self.last_sweep.lock().expect("lock poisoned");
            match *last_sweep {
                Some(previous) if now.duration_since(previous) < SWEEP_INTERVAL => return,
                _ => *last_sweep = Some(now),
            }
        }
        let before = self.inner.len();
        self.inner
            .retain(|_, snapshot| now.duration_since(snapshot.last_touched) < DOMAIN_STATE_TTL);
        let evicted = before - self.inner.len();
        if evicted > 0 {
            tracing::debug!(evicted, retained = self.inner.len(), "expired idle domain state");
        }
    }
}

impl EwmaDomainState {
    /// Create a new EWMA domain state with default EWMA settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Override the EWMA tracker configuration.
    pub fn with_ewma(mut self, ewma: EwmaTracker) -> Self {
        self.ewma = ewma;
        self
    }
}

#[async_trait]
impl DomainStatePort for EwmaDomainState {
    async fn recommend(&self, domain: &str) -> DomainRecommendation {
        let Some(snapshot) = self.inner.get(domain).map(|s| s.clone()) else {
            return DomainRecommendation::default();
        };

        let starting_tier = if self.ewma.should_promote(snapshot.block_ewma, snapshot.sample_count) {
            Tier::Bypass
        } else {
            Tier::Http
        };

        let sample_weight = (snapshot.sample_count as f32 / 50.0).min(1.0);
        let decisiveness = snapshot.block_ewma.max(1.0 - snapshot.block_ewma);
        let confidence = Some(sample_weight * decisiveness);

        DomainRecommendation {
            starting_tier,
            confidence,
        }
    }

    async fn observe(&self, domain: &str, observation: &DomainObservation) {
        // ~keep Permanent errors carry no bot-protection signal; keep them out of the EWMA.
        if matches!(observation.outcome, ObservedOutcome::Permanent) {
            return;
        }

        let blocked = matches!(
            observation.outcome,
            ObservedOutcome::WafBlocked { .. } | ObservedOutcome::Transient
        );

        let vendor = match &observation.outcome {
            ObservedOutcome::WafBlocked { vendor } => Some(vendor.clone()),
            _ => None,
        };

        let now = Instant::now();
        self.sweep_if_due(now);

        let prev = self.inner.get(domain).map(|s| s.clone()).unwrap_or(DomainSnapshot {
            block_ewma: 0.0,
            sample_count: 0,
            classifier: None,
            starting_tier: Tier::Http,
            last_touched: now,
        });

        let next_ewma = self.ewma.update(prev.block_ewma, blocked);
        let next_sample_count = prev.sample_count + 1;
        let next_classifier = vendor.or(prev.classifier);
        let next_starting_tier = if self.ewma.should_promote(next_ewma, next_sample_count) {
            Tier::Bypass
        } else if self.ewma.should_demote(next_ewma, next_sample_count) {
            Tier::Http
        } else {
            prev.starting_tier
        };

        // ~keep Last-writer-wins can lose one sample, but avoids holding a shard lock across EWMA math.
        self.inner.insert(
            domain.to_string(),
            DomainSnapshot {
                block_ewma: next_ewma,
                sample_count: next_sample_count,
                classifier: next_classifier,
                starting_tier: next_starting_tier,
                last_touched: now,
            },
        );
    }
}

/// Retry policy that consults a [`DomainStatePort`] for the per-domain
/// prior on each decision. Falls back to [`SimpleRetryPolicy`] semantics
/// when no state is available for the domain.
#[derive(Debug)]
#[cfg_attr(alef, alef(skip))]
pub struct LearningRetryPolicy {
    state: Arc<dyn DomainStatePort>,
    fallback: SimpleRetryPolicy,
}

impl LearningRetryPolicy {
    /// Create a new learning policy backed by the given state port.
    pub fn new(state: Arc<dyn DomainStatePort>) -> Self {
        Self {
            state,
            fallback: SimpleRetryPolicy::new(),
        }
    }

    /// Override the fallback policy used for the immediate retry decision.
    pub fn with_fallback(mut self, fallback: SimpleRetryPolicy) -> Self {
        self.fallback = fallback;
        self
    }
}

#[async_trait]
impl RetryPolicy for LearningRetryPolicy {
    async fn decide(&self, outcome: &AttemptOutcome) -> RetryDirective {
        let directive = self.fallback.decide(outcome).await;

        if let Ok(parsed) = url::Url::parse(&outcome.url)
            && let Some(domain) = parsed.host_str()
        {
            let observed_outcome = classify_outcome(outcome);
            let observation = DomainObservation::now(outcome.previous_tier, observed_outcome);
            self.state.observe(domain, &observation).await;
        }

        directive
    }

    fn name(&self) -> &'static str {
        "learning"
    }
}

/// Translate an [`AttemptOutcome`] into an [`ObservedOutcome`] for state recording.
///
/// Mapping:
/// - WAF signal present → `WafBlocked { vendor }`
/// - Permanent host-level errors → `Permanent` (observe will skip these)
/// - Other errors (5xx, timeout, rate-limited, transient) → `Transient`
/// - Success (no error) → `Success`
fn classify_outcome(outcome: &AttemptOutcome) -> ObservedOutcome {
    if let Some(ref signal) = outcome.waf_signal {
        return ObservedOutcome::WafBlocked {
            vendor: signal.vendor.clone(),
        };
    }

    match &outcome.error {
        Some(crate::error::CrawlError::WafBlocked { vendor, .. }) => {
            ObservedOutcome::WafBlocked { vendor: vendor.clone() }
        }
        Some(
            crate::error::CrawlError::Dns(_)
            | crate::error::CrawlError::Ssl(_)
            | crate::error::CrawlError::Connection(_)
            | crate::error::CrawlError::InvalidConfig(_)
            | crate::error::CrawlError::Unsupported(_)
            | crate::error::CrawlError::NotFound(_)
            | crate::error::CrawlError::Unauthorized(_)
            | crate::error::CrawlError::Gone(_)
            | crate::error::CrawlError::DataLoss(_)
            | crate::error::CrawlError::BrowserError(_)
            | crate::error::CrawlError::BrowserTimeout(_)
            | crate::error::CrawlError::SsrfPolicyViolation { .. },
        ) => ObservedOutcome::Permanent,
        Some(
            crate::error::CrawlError::Forbidden(_)
            | crate::error::CrawlError::RateLimited(_)
            | crate::error::CrawlError::ServerError(_)
            | crate::error::CrawlError::BadGateway(_)
            | crate::error::CrawlError::Timeout(_)
            | crate::error::CrawlError::Other(_),
        ) => ObservedOutcome::Transient,
        None => ObservedOutcome::Success,
    }
}

/// Convenience constructor: `Arc<dyn DomainStatePort>` backed by an in-memory EWMA map.
#[must_use]
#[cfg_attr(alef, alef(skip))]
pub fn in_memory_domain_state() -> Arc<dyn DomainStatePort> {
    Arc::new(EwmaDomainState::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::WafSignal;

    #[test]
    fn ewma_starts_at_zero_and_climbs() {
        let tracker = EwmaTracker::new();
        let mut ewma = 0.0;
        for _ in 0..20 {
            ewma = tracker.update(ewma, true);
        }
        assert!(
            ewma > 0.5,
            "ewma should climb past 0.5 after 20 blocked observations, got {ewma}"
        );
    }

    #[test]
    fn ewma_decays_to_zero() {
        let tracker = EwmaTracker::new();
        let mut ewma = 0.9;
        for _ in 0..50 {
            ewma = tracker.update(ewma, false);
        }
        assert!(
            ewma < 0.05,
            "ewma should decay below 0.05 after 50 clean observations, got {ewma}"
        );
    }

    #[test]
    fn promote_only_with_enough_samples() {
        let tracker = EwmaTracker::new();
        assert!(!tracker.should_promote(0.9, 5), "below min_samples_promote");
        assert!(tracker.should_promote(0.5, 10));
        assert!(!tracker.should_promote(0.3, 100), "ewma below threshold");
    }

    #[test]
    fn demote_requires_low_ewma_and_high_samples() {
        let tracker = EwmaTracker::new();
        assert!(!tracker.should_demote(0.05, 10), "below min_samples_demote");
        assert!(tracker.should_demote(0.05, 100));
        assert!(!tracker.should_demote(0.2, 100), "ewma above demote threshold");
    }

    #[tokio::test]
    async fn ewma_state_records_and_recommends() {
        let state = EwmaDomainState::new();
        let observation = DomainObservation::now(
            Tier::Http,
            ObservedOutcome::WafBlocked {
                vendor: "cloudflare".into(),
            },
        );
        state.observe("example.com", &observation).await;
        let rec = state.recommend("example.com").await;
        assert!(
            rec.confidence.unwrap_or(0.0) > 0.0,
            "confidence should be non-zero after a block"
        );
    }

    #[tokio::test]
    async fn ewma_state_promotes_starting_tier_after_streak() {
        let state = EwmaDomainState::new();
        let observation = DomainObservation::now(Tier::Http, ObservedOutcome::Transient);
        for _ in 0..30 {
            state.observe("blocked.example", &observation).await;
        }
        let rec = state.recommend("blocked.example").await;
        assert_eq!(rec.starting_tier, Tier::Bypass, "should promote after sustained blocks");
    }

    #[tokio::test]
    async fn ewma_state_returns_default_for_unseen_domain() {
        let state = EwmaDomainState::new();
        let rec = state.recommend("never-seen.example").await;
        assert_eq!(rec, DomainRecommendation::default());
    }

    #[tokio::test]
    async fn waf_blocked_outcome_sets_classifier() {
        let state = EwmaDomainState::new();
        let observation = DomainObservation::now(
            Tier::Http,
            ObservedOutcome::WafBlocked {
                vendor: "cloudflare".into(),
            },
        );
        state.observe("example.com", &observation).await;
        let snapshot = state.inner.get("example.com").map(|s| s.clone()).expect("recorded");
        assert_eq!(snapshot.classifier.as_deref(), Some("cloudflare"));
    }

    #[tokio::test]
    async fn learning_policy_records_outcome_on_waf_blocked() {
        let state = Arc::new(EwmaDomainState::new());
        let policy = LearningRetryPolicy::new(state.clone() as Arc<dyn DomainStatePort>);
        let outcome = AttemptOutcome {
            attempt: 0,
            url: Arc::from("https://example.com/path"),
            status: None,
            error: Some(crate::error::CrawlError::WafBlocked {
                vendor: "cloudflare".into(),
                message: "cloudflare".into(),
            }),
            waf_signal: None,
            body_size: 0,
            content_density: 0.0,
            bytes_transferred: None,
            previous_tier: Tier::Http,
        };
        let directive = policy.decide(&outcome).await;
        assert!(matches!(directive, RetryDirective::Escalate { .. }));
        let snapshot = state
            .inner
            .get("example.com")
            .map(|s| s.clone())
            .expect("state should record domain");
        assert!(snapshot.block_ewma > 0.0, "blocked outcome should increase ewma");
        assert_eq!(snapshot.sample_count, 1);
    }

    #[tokio::test]
    async fn learning_policy_name_is_learning() {
        let state = Arc::new(EwmaDomainState::new()) as Arc<dyn DomainStatePort>;
        let policy = LearningRetryPolicy::new(state);
        assert_eq!(policy.name(), "learning");
    }

    #[tokio::test]
    async fn learning_policy_does_not_record_permanent_errors() {
        let state = Arc::new(EwmaDomainState::new());
        let policy = LearningRetryPolicy::new(state.clone() as Arc<dyn DomainStatePort>);

        let outcome = AttemptOutcome {
            attempt: 0,
            url: Arc::from("https://dead.example/"),
            status: None,
            error: Some(crate::error::CrawlError::Dns("nxdomain".into())),
            waf_signal: None,
            body_size: 0,
            content_density: 0.0,
            bytes_transferred: None,
            previous_tier: Tier::Http,
        };
        policy.decide(&outcome).await;

        let snapshot = state.inner.get("dead.example").map(|s| s.clone());
        assert!(
            snapshot.is_none(),
            "DNS error must not pollute domain state; got {snapshot:?}"
        );
    }

    /// ~keep Time is injected rather than slept: the TTL is an hour, so a sleeping test
    /// would either take an hour or, if shortened, assert nothing about the real value.
    #[tokio::test]
    async fn sweep_drops_idle_domains_and_keeps_live_ones() {
        let state = EwmaDomainState::new();
        let now = Instant::now();
        state.inner.insert(
            "idle.example".to_owned(),
            DomainSnapshot {
                block_ewma: 0.9,
                sample_count: 10,
                classifier: None,
                starting_tier: Tier::Bypass,
                last_touched: now - DOMAIN_STATE_TTL - Duration::from_secs(1),
            },
        );
        state.inner.insert(
            "live.example".to_owned(),
            DomainSnapshot {
                block_ewma: 0.1,
                sample_count: 3,
                classifier: None,
                starting_tier: Tier::Http,
                last_touched: now - Duration::from_secs(5),
            },
        );
        *state.last_sweep.lock().expect("lock poisoned") = Some(now - SWEEP_INTERVAL - Duration::from_secs(1));

        state.sweep_if_due(now);

        assert!(
            !state.inner.contains_key("idle.example"),
            "a domain unobserved past the TTL must be evicted"
        );
        assert!(
            state.inner.contains_key("live.example"),
            "a recently-observed domain must survive the sweep"
        );
    }

    #[tokio::test]
    async fn sweep_is_skipped_until_the_interval_elapses() {
        let state = EwmaDomainState::new();
        let now = Instant::now();
        state.inner.insert(
            "idle.example".to_owned(),
            DomainSnapshot {
                block_ewma: 0.9,
                sample_count: 10,
                classifier: None,
                starting_tier: Tier::Bypass,
                last_touched: now - DOMAIN_STATE_TTL - Duration::from_secs(1),
            },
        );
        *state.last_sweep.lock().expect("lock poisoned") = Some(now);

        state.sweep_if_due(now);

        assert_eq!(
            state.inner.len(),
            1,
            "an expired entry must survive until a sweep is due, keeping the hot path O(1)"
        );
    }

    #[tokio::test]
    async fn observe_skips_permanent_outcome() {
        let state = EwmaDomainState::new();
        let observation = DomainObservation::now(Tier::Http, ObservedOutcome::Permanent);
        state.observe("permanent.example", &observation).await;
        let snapshot = state.inner.get("permanent.example").map(|s| s.clone());
        assert!(snapshot.is_none(), "Permanent outcome must not be recorded");
    }

    #[tokio::test]
    async fn learning_policy_records_waf_signal_vendor() {
        let state = Arc::new(EwmaDomainState::new());
        let policy = LearningRetryPolicy::new(state.clone() as Arc<dyn DomainStatePort>);
        let outcome = AttemptOutcome {
            attempt: 0,
            url: Arc::from("https://example.com/"),
            status: Some(200),
            error: None,
            waf_signal: Some(WafSignal {
                vendor: "datadome".into(),
                fingerprint_id: "datadome_js_v1".into(),
                weight: 0.9,
            }),
            body_size: 100,
            content_density: 0.1,
            bytes_transferred: Some(100),
            previous_tier: Tier::Http,
        };
        policy.decide(&outcome).await;
        let snapshot = state
            .inner
            .get("example.com")
            .map(|s| s.clone())
            .expect("waf_signal should trigger recording");
        assert_eq!(snapshot.classifier.as_deref(), Some("datadome"));
    }

    #[tokio::test]
    async fn observe_under_concurrent_writers_does_not_panic() {
        let state = Arc::new(EwmaDomainState::new());
        let observation = DomainObservation::now(Tier::Http, ObservedOutcome::Transient);
        let mut handles = Vec::new();
        for _ in 0..50 {
            let s = state.clone();
            let o = observation.clone();
            handles.push(tokio::spawn(async move {
                s.observe("contended.example", &o).await;
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        // ~keep Last-writer-wins may lose observations; the invariant is consistent state with sample_count in [1, 50].
        let snapshot = state
            .inner
            .get("contended.example")
            .map(|s| s.clone())
            .expect("recorded");
        assert!(snapshot.sample_count >= 1);
        assert!(snapshot.sample_count <= 50);
        assert!(snapshot.block_ewma > 0.0);
    }

    #[tokio::test]
    async fn ewma_oscillation_promotes_demotes_then_re_promotes() {
        let state = EwmaDomainState::new();
        let domain = "oscillating.example";

        for _ in 0..20 {
            let obs = DomainObservation::now(
                Tier::Http,
                ObservedOutcome::WafBlocked {
                    vendor: "cloudflare".into(),
                },
            );
            state.observe(domain, &obs).await;
        }
        assert_eq!(state.recommend(domain).await.starting_tier, Tier::Bypass);

        for _ in 0..200 {
            let obs = DomainObservation::now(Tier::Bypass, ObservedOutcome::Success);
            state.observe(domain, &obs).await;
        }
        assert_eq!(state.recommend(domain).await.starting_tier, Tier::Http);

        for _ in 0..200 {
            let obs = DomainObservation::now(
                Tier::Http,
                ObservedOutcome::WafBlocked {
                    vendor: "cloudflare".into(),
                },
            );
            state.observe(domain, &obs).await;
        }
        assert_eq!(state.recommend(domain).await.starting_tier, Tier::Bypass);
    }

    #[tokio::test]
    async fn learning_policy_does_not_panic_on_unparsable_url() {
        let state: Arc<dyn DomainStatePort> = Arc::new(EwmaDomainState::new());
        let policy = LearningRetryPolicy::new(state.clone());

        let outcome = AttemptOutcome {
            attempt: 0,
            url: Arc::from("not a url"),
            status: None,
            error: None,
            waf_signal: None,
            body_size: 0,
            content_density: 0.0,
            bytes_transferred: None,
            previous_tier: Tier::Http,
        };

        let _ = policy.decide(&outcome).await;

        let rec = state.recommend("not a url").await;
        assert_eq!(rec.starting_tier, Tier::Http);
        assert_eq!(rec.confidence, None);
    }
}
