//! Tier-dispatch types: escalation strategy, retry policy, WAF classifier,
//! per-domain state, and budget interfaces.
//!
//! Pure type declarations. The engine wires these in [`crate::engine`]
//! starting in Commit 1.3.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::error::CrawlError;
use crate::http::HttpResponse;
use crate::types::bypass::DynBypassProvider;

/// Defines the escalation chain when a tier produces a block signal.
///
/// `BrowserOnly` is the `#[default]` — preserves the pre-tier-dispatch behavior
/// of the engine: HTTP → Browser on `WafBlocked` / `Forbidden` when
/// `BrowserMode::Auto` is set, no vendor escalation.
///
/// ## Choosing a strategy
///
/// | Strategy | Best for |
/// |---|---|
/// | `None` | Diagnostic / audit crawls where you want raw HTTP errors |
/// | `BrowserOnly` | Default; JS-heavy sites where browser is already configured |
/// | `BypassFirst` | Legacy: engine auto-selects this when `bypass` is set and strategy is unset |
/// | `BypassOnly` | WAF-heavy targets without a browser backend configured |
/// | `BypassThenBrowser` | Maximum resilience: vendor bypass then headless Chrome |
///
/// # Examples
///
/// ```
/// # use crawlberg::EscalationStrategy;
/// let audit_strategy = EscalationStrategy::None;
/// let browser_strategy = EscalationStrategy::BrowserOnly;
/// let resilient_strategy = EscalationStrategy::BypassThenBrowser;
/// // Use BrowserOnly for JS-heavy sites, BypassThenBrowser for maximum resilience.
/// ```
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationStrategy {
    /// HTTP only; surface failures as-is. No escalation.
    None,
    /// HTTP → Browser on WafBlocked / Forbidden. The pre-dispatch behavior.
    #[default]
    BrowserOnly,
    /// Legacy semantic: skip HTTP entirely, always route through the configured
    /// `bypass` provider. The engine auto-selects this when `escalation_strategy`
    /// is left at its default (`BrowserOnly`) AND `config.bypass` is configured —
    /// this preserves the pre-tier-dispatch `bypass` field behavior for existing
    /// callers of `CrawlConfig::default() + .bypass = Some(...)`.
    BypassFirst,
    /// HTTP → Bypass on WafBlocked / Forbidden. Browser never invoked.
    BypassOnly,
    /// HTTP → Bypass → Browser. Bypass first (cheaper than browser+proxy);
    /// browser as the ultimate fallback.
    BypassThenBrowser,
}

/// Which tier produced the current attempt's outcome.
///
/// # Examples
///
/// ```
/// # use crawlberg::Tier;
/// let tier = Tier::Http;
/// match tier {
///     Tier::Http => println!("HTTP tier"),
///     Tier::Bypass => println!("Bypass tier"),
///     Tier::Browser => println!("Browser tier"),
///     // `#[non_exhaustive]`: callers outside the crate must include a wildcard
///     // so future variants do not break their match.
///     _ => println!("unknown tier"),
/// }
/// ```
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// Plain HTTP fetch tier.
    Http,
    /// Bypass-vendor tier (caller-supplied [`crate::types::BypassProvider`]).
    Bypass,
    /// Headless-browser tier.
    Browser,
}

/// Why the dispatcher should escalate to the next tier.
///
/// # Examples
///
/// ```
/// # use crawlberg::EscalationReason;
/// let waf_block = EscalationReason::WafBlocked {
///     vendor: "cloudflare".to_string(),
/// };
/// let soft_block = EscalationReason::SoftBlock;
/// let render = EscalationReason::RenderNeeded;
/// let unreliable = EscalationReason::OriginUnreliable;
///
/// match waf_block {
///     EscalationReason::WafBlocked { vendor } => println!("Blocked by {}", vendor),
///     EscalationReason::SoftBlock => println!("Soft block detected"),
///     EscalationReason::RenderNeeded => println!("JS render needed"),
///     EscalationReason::OriginUnreliable => println!("Origin unreachable"),
///     // `#[non_exhaustive]`: callers outside the crate must include a wildcard
///     // so future variants do not break their match.
///     _ => println!("unknown reason"),
/// }
/// ```
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationReason {
    /// WAF or bot protection detected the request.
    WafBlocked {
        /// Lowercase vendor identifier, e.g. `"cloudflare"`.
        vendor: String,
    },
    /// 200 with low content density — likely a soft block.
    SoftBlock,
    /// 200 but body is a SPA shell that needs JS render.
    RenderNeeded,
    /// Sustained 5xx; origin probably unreachable, escalate anyway.
    OriginUnreliable,
    /// Antibot strategy hook returned [`crate::types::Decision::EscalateBrowser`].
    AntibotEscalate,
}

/// Rich context passed to [`RetryPolicy::decide`] on each attempt.
///
/// Inspired by spider-rs `AttemptOutcome`
/// (<https://github.com/spider-rs/spider> — `spider/src/retry_strategy.rs`).
/// Field set is intentionally a subset — we omit UA / fingerprint /
/// chrome_connection because those are caller (xberg-enterprise) concerns.
///
/// All fields are owned so async impls can clone or move into spawned tasks
/// without borrow-checker issues. The previous `<'a>` lifetime was incompatible
/// with policies that record outcomes to background tasks.
///
/// # Examples
///
/// ```
/// # use crawlberg::{AttemptOutcome, Tier, WafSignal};
/// # use std::sync::Arc;
/// let outcome = AttemptOutcome {
///     attempt: 0,
///     url: Arc::from("https://example.com"),
///     status: Some(403),
///     error: None,
///     waf_signal: Some(WafSignal {
///         vendor: "cloudflare".to_string(),
///         fingerprint_id: "challenge_slug".to_string(),
///         weight: 0.95,
///     }),
///     body_size: 1024,
///     content_density: 0.05,
///     bytes_transferred: Some(2048),
///     previous_tier: Tier::Http,
/// };
/// assert_eq!(outcome.status, Some(403));
/// assert_eq!(outcome.attempt, 0);
/// ```
#[derive(Debug, Clone)]
pub struct AttemptOutcome {
    /// Zero-based attempt index.
    pub attempt: u32,
    /// The URL being fetched.
    pub url: Arc<str>,
    /// HTTP status code, if a response was received.
    pub status: Option<u16>,
    /// Error from this attempt, if one occurred.
    pub error: Option<CrawlError>,
    /// WAF fingerprint match from this attempt, if detected.
    pub waf_signal: Option<WafSignal>,
    /// Response body size in bytes.
    pub body_size: usize,
    /// `text_bytes / html_bytes`. Used to detect SPA shells (typical 0.0–0.05).
    pub content_density: f32,
    /// Total bytes transferred over the wire for this attempt.
    pub bytes_transferred: Option<u64>,
    /// The tier that produced this attempt.
    pub previous_tier: Tier,
}

/// Errors returned by [`WafClassifier::classify`].
///
/// `BuildError` is for classifier-internal construction problems (TOML parse
/// failures, AC matcher build failures). `ClassifyError` is for per-call
/// problems (e.g. response body decoding failures).
///
/// The engine treats both variants as `None` for dispatch purposes and logs
/// them at WARN — a misconfigured classifier does NOT crash the dispatcher.
///
/// # Examples
///
/// ```
/// # use crawlberg::WafClassifyError;
/// let build_err = WafClassifyError::BuildError("invalid toml".to_string());
/// let classify_err = WafClassifyError::ClassifyError("bad encoding".to_string());
///
/// match build_err {
///     WafClassifyError::BuildError(msg) => println!("Build failed: {}", msg),
///     WafClassifyError::ClassifyError(msg) => println!("Classify failed: {}", msg),
///     // `#[non_exhaustive]`: callers outside the crate must include a wildcard
///     // so future variants do not break their match.
///     _ => println!("other classify error"),
/// }
/// ```
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum WafClassifyError {
    /// Classifier construction failed (e.g. TOML parse or AC build error).
    #[error("waf classifier build: {0}")]
    BuildError(String),
    /// Per-call classification failure (e.g. response body decoding error).
    #[error("waf classify: {0}")]
    ClassifyError(String),
}

/// What the dispatcher does next, returned by [`RetryPolicy::decide`].
///
/// # Examples
///
/// ```
/// # use crawlberg::{RetryDirective, EscalationReason};
/// let stop = RetryDirective::Stop;
/// let retry = RetryDirective::Retry { backoff_ms: 1000 };
/// let escalate = RetryDirective::Escalate {
///     reason: EscalationReason::SoftBlock,
/// };
///
/// match escalate {
///     RetryDirective::Stop => println!("Stop"),
///     RetryDirective::Retry { backoff_ms } => println!("Wait {}ms", backoff_ms),
///     RetryDirective::Escalate { reason } => println!("Escalate: {:?}", reason),
///     // `#[non_exhaustive]`: callers outside the crate must include a wildcard
///     // so future variants do not break their match.
///     _ => println!("other directive"),
/// }
/// ```
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum RetryDirective {
    /// Stop. Surface the current result to the caller.
    Stop,
    /// Retry the same tier after `backoff_ms`. Use for rate-limit / 5xx.
    Retry {
        /// Milliseconds to wait before retrying.
        backoff_ms: u64,
    },
    /// Escalate to the next tier per [`EscalationStrategy`].
    Escalate {
        /// Reason for escalation, used for metrics and logging.
        reason: EscalationReason,
    },
}

/// Pluggable per-attempt decision policy.
///
/// Default impl in `crate::defaults::dispatch::SimpleRetryPolicy` (Commit 1.2)
/// uses a per-error mapping with no learning. Callers can wire
/// state-backed policies (e.g. EWMA, per-domain priors) via this trait.
///
/// # Examples
///
/// ```
/// # use crawlberg::{RetryPolicy, RetryDirective, AttemptOutcome};
/// # use async_trait::async_trait;
/// # use std::fmt;
/// #[derive(Debug)]
/// struct AlwaysStop;
///
/// #[async_trait]
/// impl RetryPolicy for AlwaysStop {
///     async fn decide(&self, _outcome: &AttemptOutcome) -> RetryDirective {
///         RetryDirective::Stop
///     }
///     fn name(&self) -> &'static str {
///         "always_stop"
///     }
/// }
/// ```
#[async_trait]
pub trait RetryPolicy: Send + Sync + fmt::Debug {
    /// Decide what the dispatcher does after the given attempt.
    async fn decide(&self, outcome: &AttemptOutcome) -> RetryDirective;
    /// Stable, lowercase policy identifier for span attributes and metrics.
    fn name(&self) -> &'static str;
}

/// Convenience alias for an owned, type-erased retry policy on
/// [`crate::types::CrawlConfig`].
pub type DynRetryPolicy = Arc<dyn RetryPolicy>;

/// Output of a WAF classifier — a single fingerprint match.
///
/// # Examples
///
/// ```
/// # use crawlberg::WafSignal;
/// let signal = WafSignal {
///     vendor: "cloudflare".to_string(),
///     fingerprint_id: "challenge_slug".to_string(),
///     weight: 0.95,
/// };
/// assert_eq!(signal.vendor, "cloudflare");
/// assert!(signal.weight > 0.9);
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct WafSignal {
    /// Lowercase vendor identifier: `"cloudflare"`, `"datadome"`, …
    pub vendor: String,
    /// Stable, dot-free fingerprint identifier. Used as a metric label.
    pub fingerprint_id: String,
    /// 0.0–1.0 confidence weight. Multi-signal fingerprints get higher weight.
    pub weight: f32,
}

/// Pluggable WAF detection.
///
/// Default impl in `crate::waf::TomlClassifier` (Commit 1.4) loads
/// `rules/waf_fingerprints.toml`, runs Aho-Corasick over the body and
/// checks response headers.
///
/// # Examples
///
/// ```
/// # use crawlberg::{WafClassifier, WafSignal, WafClassifyError};
/// # use crawlberg::http::HttpResponse;
/// # use std::fmt;
/// #[derive(Debug)]
/// struct AlwaysClean;
///
/// impl WafClassifier for AlwaysClean {
///     fn classify(&self, _response: &HttpResponse) -> Result<Option<WafSignal>, WafClassifyError> {
///         Ok(None)
///     }
/// }
/// ```
pub trait WafClassifier: Send + Sync + fmt::Debug {
    /// Inspect the response; return a [`WafSignal`] if any fingerprint matches.
    ///
    /// Returns `Ok(None)` for clean responses, `Ok(Some(sig))` for matches,
    /// and `Err(WafClassifyError)` only for classifier-internal failures
    /// (TOML parse errors at construction time, malformed responses, etc).
    /// A misconfigured classifier MUST surface via `Err`, not silently as `Ok(None)`.
    fn classify(&self, response: &HttpResponse) -> Result<Option<WafSignal>, WafClassifyError>;
}

/// Convenience alias for an owned, type-erased WAF classifier.
pub type DynWafClassifier = Arc<dyn WafClassifier>;

/// Recommendation returned by [`DomainStatePort::recommend`] for the next
/// fetch attempt against a domain. Generic over the backend's internal
/// model — the only data the engine needs to act on is which tier to
/// start at and how confident the backend is in that choice.
///
/// # Examples
///
/// ```
/// # use crawlberg::{DomainRecommendation, Tier};
/// let rec = DomainRecommendation {
///     starting_tier: Tier::Browser,
///     confidence: Some(0.85),
/// };
/// println!("Start at {:?} with confidence {:?}", rec.starting_tier, rec.confidence);
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct DomainRecommendation {
    /// Recommended starting tier for the next request to this domain.
    pub starting_tier: Tier,
    /// Confidence in the recommendation, in `0.0..=1.0` where `1.0` is "strong
    /// signal". `None` means "no opinion" — typically because the backend has
    /// no observations yet, or implements a rule-based model that does not
    /// produce a probability. Policies should treat `None` as "fall through to
    /// default behaviour" rather than substituting a numeric value.
    pub confidence: Option<f32>,
}

/// Default `DomainRecommendation` is "no information": HTTP tier, no confidence.
impl Default for DomainRecommendation {
    fn default() -> Self {
        Self {
            starting_tier: Tier::Http,
            confidence: None,
        }
    }
}

/// Single fetch outcome reported to [`DomainStatePort::observe`]. The
/// backend turns these into its own state model (EWMA, rule-based,
/// histogram, etc).
///
/// # Examples
///
/// ```
/// # use crawlberg::{DomainObservation, Tier, ObservedOutcome};
/// let obs = DomainObservation::now(Tier::Http, ObservedOutcome::Success);
/// assert_eq!(obs.tier, Tier::Http);
/// assert_eq!(obs.outcome, ObservedOutcome::Success);
/// ```
#[derive(Debug, Clone)]
pub struct DomainObservation {
    /// The tier this observation came from.
    pub tier: Tier,
    /// What happened. The outcome enum is the load-bearing classification.
    pub outcome: ObservedOutcome,
    /// When the observation was made. Backends may use this for decay /
    /// time-windowing.
    pub timestamp: std::time::SystemTime,
}

impl DomainObservation {
    /// Construct an observation with `timestamp = SystemTime::now()`.
    pub fn now(tier: Tier, outcome: ObservedOutcome) -> Self {
        Self {
            tier,
            outcome,
            timestamp: std::time::SystemTime::now(),
        }
    }
}

/// Classification of a single fetch outcome.
///
/// # Examples
///
/// ```
/// # use crawlberg::ObservedOutcome;
/// let success = ObservedOutcome::Success;
/// let blocked = ObservedOutcome::WafBlocked {
///     vendor: "datadome".to_string(),
/// };
/// let transient = ObservedOutcome::Transient;
/// let permanent = ObservedOutcome::Permanent;
///
/// match success {
///     ObservedOutcome::Success => println!("Clean response"),
///     ObservedOutcome::WafBlocked { vendor } => println!("Blocked by {}", vendor),
///     ObservedOutcome::Transient => println!("Transient failure"),
///     ObservedOutcome::Permanent => println!("Permanent failure"),
///     // `#[non_exhaustive]`: callers outside the crate must include a wildcard
///     // so future variants do not break their match.
///     _ => println!("unknown outcome"),
/// }
/// ```
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObservedOutcome {
    /// Fetch returned content cleanly (HTTP 2xx, no WAF signals).
    Success,
    /// WAF detected the request. `vendor` matches `EscalationReason::WafBlocked.vendor`.
    WafBlocked {
        /// Lowercase vendor identifier, e.g. `"cloudflare"`.
        vendor: String,
    },
    /// Fetch failed for a transient reason (5xx, timeout, rate-limited).
    Transient,
    /// Fetch failed for a permanent reason (DNS, SSL, NotFound).
    /// Backends should usually ignore these — they say nothing about
    /// domain-level bot protection.
    Permanent,
}

/// Persistent per-domain dispatch state.
///
/// Default impl in `crate::defaults::domain_state::EwmaDomainState`
/// (Commit 1.5) is a process-local `DashMap`. xberg-enterprise provides a
/// Postgres-backed impl in its `dispatch-postgres` crate.
///
/// The trait is generic over the observation model — self-hosters with
/// non-EWMA backends (Redis, rule-based, ML-driven) implement against
/// `DomainRecommendation` / `DomainObservation` without forced EWMA semantics.
///
/// # Examples
///
/// ```
/// # use crawlberg::{DomainStatePort, DomainRecommendation, DomainObservation, Tier};
/// # use async_trait::async_trait;
/// # use std::fmt;
/// #[derive(Debug)]
/// struct AlwaysDefault;
///
/// #[async_trait]
/// impl DomainStatePort for AlwaysDefault {
///     async fn recommend(&self, _domain: &str) -> DomainRecommendation {
///         DomainRecommendation::default()
///     }
///
///     async fn observe(&self, _domain: &str, _observation: &DomainObservation) {
///         // No-op for this example
///     }
/// }
/// ```
#[async_trait]
pub trait DomainStatePort: Send + Sync + fmt::Debug {
    /// Lookup the backend's recommendation for the next request to `domain`.
    /// Implementations that have no observations for the domain return
    /// `DomainRecommendation::default()`.
    async fn recommend(&self, domain: &str) -> DomainRecommendation;

    /// Record an outcome observation. Backends update their internal
    /// model however they choose.
    async fn observe(&self, domain: &str, observation: &DomainObservation);
}

/// Convenience alias for an owned, type-erased domain-state backend.
pub type DynDomainStatePort = Arc<dyn DomainStatePort>;

/// Pluggable per-job escalation budget.
///
/// Returned `BudgetExhausted` causes the dispatcher to refuse further
/// escalation. Implementations decide whether the job degrades to the
/// cheapest tier or fails outright.
///
/// # Examples
///
/// ```
/// # use crawlberg::{EscalationBudget, BudgetExhausted};
/// # use async_trait::async_trait;
/// # use std::fmt;
/// #[derive(Debug)]
/// struct UnlimitedBudget;
///
/// #[async_trait]
/// impl EscalationBudget for UnlimitedBudget {
///     async fn try_consume(&self, _cost_cents: u32) -> Result<(), BudgetExhausted> {
///         Ok(())
///     }
/// }
/// ```
#[async_trait]
pub trait EscalationBudget: Send + Sync + fmt::Debug {
    /// Attempt to debit `cost_cents` from the remaining budget. `Ok(())`
    /// means the dispatcher may escalate; `Err` means it must not.
    async fn try_consume(&self, cost_cents: u32) -> Result<(), BudgetExhausted>;
}

/// Convenience alias for an owned, type-erased budget.
pub type DynEscalationBudget = Arc<dyn EscalationBudget>;

/// Returned by [`EscalationBudget::try_consume`] when no budget remains.
///
/// # Examples
///
/// ```
/// # use crawlberg::BudgetExhausted;
/// let err = BudgetExhausted;
/// match Err::<(), _>(err) {
///     Err(BudgetExhausted) => println!("No budget"),
///     Ok(()) => println!("Budget available"),
/// }
/// ```
#[derive(Debug, Clone, Copy, Error, PartialEq, Eq)]
#[error("escalation_budget_exhausted")]
pub struct BudgetExhausted;

/// Bundle of pluggable dispatch components attached to [`crate::types::CrawlConfig`].
///
/// The `antibot_strategy` field accepts an optional `Arc<dyn AntibotStrategy>`.
/// Because it holds an opaque trait object it is excluded from alef-generated
/// polyglot bindings (`#[cfg_attr(alef, alef(skip))]`). Language clients that
/// need custom antibot logic must subclass or wrap `DefaultAntibotStrategy` at
/// the Rust layer and expose a language-friendly surface separately.
///
/// When `antibot_strategy` is `None` (the default), the engine's built-in
/// WAF-signal → escalation behaviour is preserved unchanged.
///
/// Move the seven Session 1 / 1.5 trait-object and config fields off
/// `CrawlConfig` into a single `Option<DispatchProfile>` field. Callers that
/// relied on `CrawlConfig.bypass.is_some()` auto-promoting the strategy to
/// `BypassFirst` must now set `strategy: EscalationStrategy::BypassFirst`
/// explicitly in this struct (Commit 1.5.12 breaking change).
///
/// # Examples
///
/// ```
/// # use crawlberg::{DispatchProfile, EscalationStrategy};
/// let profile = DispatchProfile::builder()
///     .strategy(EscalationStrategy::BypassThenBrowser)
///     .max_total_attempts(15)
///     .build();
/// assert_eq!(profile.strategy, EscalationStrategy::BypassThenBrowser);
/// assert_eq!(profile.max_total_attempts, 15);
/// ```
#[derive(Debug, Clone)]
pub struct DispatchProfile {
    /// Caller-supplied bypass provider.
    pub bypass: Option<DynBypassProvider>,
    /// Escalation strategy for the HTTP → Bypass → Browser dispatch chain.
    pub strategy: EscalationStrategy,
    /// Pluggable per-attempt retry/escalation decision policy.
    pub retry_policy: Option<DynRetryPolicy>,
    /// Pluggable WAF classifier.
    pub waf_classifier: Option<DynWafClassifier>,
    /// Pluggable per-domain state backend.
    pub domain_state: Option<DynDomainStatePort>,
    /// Pluggable per-job escalation budget.
    pub escalation_budget: Option<DynEscalationBudget>,
    /// Maximum total fetch attempts across all tiers before the dispatcher
    /// gives up. Guards against buggy custom `RetryPolicy` impls that never
    /// return `Stop`. Default 10.
    pub max_total_attempts: u32,
    /// Optional pluggable antibot hook pair.
    ///
    /// When `Some`, `pre_request` fires before each tower-stack fetch and
    /// `post_response` fires between WAF classification and the retry policy.
    /// When `None`, the engine's built-in WAF-signal → escalation logic applies.
    ///
    /// Excluded from alef-generated bindings — opaque trait object.
    #[cfg_attr(alef, alef(skip))]
    pub antibot_strategy: Option<crate::types::antibot::DynAntibotStrategy>,
}

impl DispatchProfile {
    /// Start a fluent builder for `DispatchProfile`. See [`crate::DispatchProfileBuilder`].
    pub fn builder() -> crate::types::builder::DispatchProfileBuilder {
        crate::types::builder::DispatchProfileBuilder::default()
    }
}

impl Default for DispatchProfile {
    fn default() -> Self {
        Self {
            bypass: None,
            strategy: EscalationStrategy::default(),
            retry_policy: None,
            waf_classifier: None,
            domain_state: None,
            escalation_budget: None,
            max_total_attempts: 10,
            antibot_strategy: None,
        }
    }
}

// ~keep DispatchProfile must stay Send + Sync; this assertion catches non-Send fields at compile time.
const _: () = {
    fn _assert_send_sync<T: Send + Sync>() {}
    fn _check() {
        _assert_send_sync::<DispatchProfile>();
    }
};
