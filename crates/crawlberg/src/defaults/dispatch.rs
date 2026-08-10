//! Default impls of [`crate::types::RetryPolicy`] and
//! [`crate::types::EscalationBudget`]. These work standalone — no state
//! backend, no persistence. xberg-enterprise's `dispatch-postgres` crate
//! provides learning impls on top of these traits.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;

use crate::error::CrawlError;
use crate::types::{AttemptOutcome, BudgetExhausted, EscalationBudget, EscalationReason, RetryDirective, RetryPolicy};

/// Per-error mapping with no learning. The simplest possible
/// [`RetryPolicy`] — useful as a baseline and as a fallback when no
/// state backend is configured.
///
/// Mapping:
///
/// | `CrawlError` variant | Directive |
/// |---|---|
/// | `WafBlocked` | `Escalate { reason: WafBlocked }` |
/// | `Forbidden` | `Escalate { reason: WafBlocked }` (403 treated as block) |
/// | `RateLimited` | `Retry { backoff_ms: min(2^attempt * 100, max_backoff_ms) }` |
/// | `ServerError`, `BadGateway`, `Timeout` | `Retry` up to `max_retries`, then `Stop` |
/// | `Dns`, `Ssl`, `Connection`, `InvalidConfig`, `Unsupported` | `Stop` (permanent) |
/// | other | `Stop` |
#[derive(Debug, Clone)]
pub struct SimpleRetryPolicy {
    max_retries: u32,
    max_backoff_ms: u64,
}

impl SimpleRetryPolicy {
    /// Standard defaults: 3 retries, 60s backoff cap.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            max_retries: 3,
            max_backoff_ms: 60_000,
        }
    }

    /// Override the maximum retry count.
    #[must_use]
    pub const fn with_max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }

    /// Override the backoff cap.
    #[must_use]
    pub const fn with_max_backoff_ms(mut self, max_backoff_ms: u64) -> Self {
        self.max_backoff_ms = max_backoff_ms;
        self
    }
}

impl Default for SimpleRetryPolicy {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RetryPolicy for SimpleRetryPolicy {
    async fn decide(&self, outcome: &AttemptOutcome) -> RetryDirective {
        let Some(ref error) = outcome.error else {
            return RetryDirective::Stop;
        };
        match error {
            CrawlError::WafBlocked { vendor, .. } => RetryDirective::Escalate {
                reason: EscalationReason::WafBlocked { vendor: vendor.clone() },
            },
            CrawlError::Forbidden(_) => RetryDirective::Escalate {
                reason: EscalationReason::WafBlocked {
                    vendor: "unknown".to_string(),
                },
            },
            CrawlError::RateLimited(_)
            | CrawlError::ServerError(_)
            | CrawlError::BadGateway(_)
            | CrawlError::Timeout(_) => {
                if outcome.attempt >= self.max_retries {
                    RetryDirective::Stop
                } else {
                    let backoff = compute_backoff_ms(outcome.attempt, self.max_backoff_ms);
                    RetryDirective::Retry { backoff_ms: backoff }
                }
            }
            CrawlError::Dns(_)
            | CrawlError::Ssl(_)
            | CrawlError::Connection(_)
            | CrawlError::InvalidConfig(_)
            | CrawlError::Unsupported(_)
            | CrawlError::NotFound(_)
            | CrawlError::Unauthorized(_)
            | CrawlError::Gone(_)
            | CrawlError::DataLoss(_)
            | CrawlError::BrowserError(_)
            | CrawlError::BrowserTimeout(_)
            | CrawlError::SsrfPolicyViolation { .. }
            | CrawlError::Other(_) => RetryDirective::Stop,
        }
    }

    fn name(&self) -> &'static str {
        "simple"
    }
}

/// Internal exponential backoff helper. Reachable from integration tests
/// (proptest invariants) via the `#[doc(hidden)] pub use` in the crate
/// root; not part of the public API surface and may change without a
/// semver bump.
#[doc(hidden)]
pub fn compute_backoff_ms(attempt: u32, max_backoff_ms: u64) -> u64 {
    let exp = 1u64.checked_shl(attempt).unwrap_or(u64::MAX);
    exp.saturating_mul(100).min(max_backoff_ms)
}

/// [`EscalationBudget`] that always permits escalation. Used by default
/// when no budget is configured on `CrawlConfig`.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnlimitedBudget;

#[async_trait]
impl EscalationBudget for UnlimitedBudget {
    async fn try_consume(&self, _cost_cents: u32) -> Result<(), BudgetExhausted> {
        Ok(())
    }
}

/// [`EscalationBudget`] backed by an atomic counter. Decrements on each
/// `try_consume`; returns `Err(BudgetExhausted)` once the remaining
/// budget can't cover the request. Useful for self-hosters that want
/// per-process spend caps without a database.
#[derive(Debug)]
pub struct FixedBudget {
    remaining_cents: AtomicU32,
}

impl FixedBudget {
    /// Create a budget with `initial_cents` available to spend.
    #[must_use]
    pub fn new(initial_cents: u32) -> Self {
        Self {
            remaining_cents: AtomicU32::new(initial_cents),
        }
    }

    /// Read the current remaining budget without consuming any.
    #[must_use]
    pub fn remaining(&self) -> u32 {
        self.remaining_cents.load(Ordering::Acquire)
    }
}

#[async_trait]
impl EscalationBudget for FixedBudget {
    async fn try_consume(&self, cost_cents: u32) -> Result<(), BudgetExhausted> {
        let mut current = self.remaining_cents.load(Ordering::Acquire);
        loop {
            if current < cost_cents {
                return Err(BudgetExhausted);
            }
            let next = current - cost_cents;
            match self
                .remaining_cents
                .compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return Ok(()),
                Err(actual) => current = actual,
            }
        }
    }
}

/// Convenience constructor: `Arc<dyn RetryPolicy>` for the default policy.
#[must_use]
pub fn default_retry_policy() -> Arc<dyn RetryPolicy> {
    Arc::new(SimpleRetryPolicy::new())
}

/// Convenience constructor: `Arc<dyn EscalationBudget>` that never blocks.
#[must_use]
pub fn unlimited_budget() -> Arc<dyn EscalationBudget> {
    Arc::new(UnlimitedBudget)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::types::Tier;

    fn outcome_with_error(error: CrawlError, attempt: u32) -> AttemptOutcome {
        AttemptOutcome {
            attempt,
            url: Arc::from("https://example.com/"),
            status: None,
            error: Some(error),
            waf_signal: None,
            body_size: 0,
            content_density: 0.0,
            bytes_transferred: None,
            previous_tier: Tier::Http,
        }
    }

    #[tokio::test]
    async fn waf_blocked_escalates() {
        let policy = SimpleRetryPolicy::new();
        let err = CrawlError::WafBlocked {
            vendor: "cloudflare".into(),
            message: "cloudflare detected".into(),
        };
        let directive = policy.decide(&outcome_with_error(err, 0)).await;
        assert!(matches!(directive, RetryDirective::Escalate { .. }));
    }

    #[tokio::test]
    async fn waf_blocked_escalation_carries_vendor() {
        let policy = SimpleRetryPolicy::new();
        let err = CrawlError::WafBlocked {
            vendor: "cloudflare".into(),
            message: "challenge".into(),
        };
        let outcome = outcome_with_error(err, 0);
        match policy.decide(&outcome).await {
            RetryDirective::Escalate {
                reason: EscalationReason::WafBlocked { vendor },
            } => {
                assert_eq!(vendor, "cloudflare");
            }
            other => panic!("expected Escalate {{ WafBlocked }}, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn forbidden_escalates() {
        let policy = SimpleRetryPolicy::new();
        let err = CrawlError::Forbidden("403".into());
        let directive = policy.decide(&outcome_with_error(err, 0)).await;
        assert!(matches!(directive, RetryDirective::Escalate { .. }));
    }

    #[tokio::test]
    async fn rate_limited_retries_with_backoff() {
        let policy = SimpleRetryPolicy::new();
        let err = CrawlError::RateLimited("429".into());
        let directive = policy.decide(&outcome_with_error(err, 0)).await;
        match directive {
            RetryDirective::Retry { backoff_ms } => assert_eq!(
                backoff_ms, 100,
                "attempt=0 must back off for exactly 2^0 * 100 = 100ms, got {backoff_ms}"
            ),
            other => panic!("expected Retry, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rate_limited_stops_after_max_retries() {
        let policy = SimpleRetryPolicy::new().with_max_retries(2);
        let err = CrawlError::RateLimited("429".into());
        let directive = policy.decide(&outcome_with_error(err, 2)).await;
        assert_eq!(directive, RetryDirective::Stop);
    }

    #[tokio::test]
    async fn max_retries_3_allows_three_retries_then_stops() {
        let policy = SimpleRetryPolicy::new().with_max_retries(3);
        let err = CrawlError::RateLimited("429".into());

        for attempt in 0..3 {
            let directive = policy.decide(&outcome_with_error(err.clone(), attempt)).await;
            assert!(
                matches!(directive, RetryDirective::Retry { .. }),
                "attempt={attempt}: expected Retry, got {directive:?}"
            );
        }

        let directive = policy.decide(&outcome_with_error(err.clone(), 3)).await;
        assert_eq!(
            directive,
            RetryDirective::Stop,
            "attempt=3 with max_retries=3: expected Stop"
        );
    }

    #[tokio::test]
    async fn dns_short_circuits() {
        let policy = SimpleRetryPolicy::new();
        let err = CrawlError::Dns("nxdomain".into());
        let directive = policy.decide(&outcome_with_error(err, 0)).await;
        assert_eq!(directive, RetryDirective::Stop);
    }

    #[tokio::test]
    async fn ssl_short_circuits() {
        let policy = SimpleRetryPolicy::new();
        let err = CrawlError::Ssl("handshake".into());
        let directive = policy.decide(&outcome_with_error(err, 0)).await;
        assert_eq!(directive, RetryDirective::Stop);
    }

    #[tokio::test]
    async fn no_error_stops() {
        let policy = SimpleRetryPolicy::new();
        let outcome = AttemptOutcome {
            attempt: 0,
            url: Arc::from("https://example.com/"),
            status: Some(200),
            error: None,
            waf_signal: None,
            body_size: 1024,
            content_density: 0.5,
            bytes_transferred: Some(1024),
            previous_tier: Tier::Http,
        };
        let directive = policy.decide(&outcome).await;
        assert_eq!(directive, RetryDirective::Stop);
    }

    #[tokio::test]
    async fn backoff_grows_then_caps() {
        let policy = SimpleRetryPolicy::new().with_max_backoff_ms(1000);
        let err = CrawlError::Timeout("slow".into());
        let expected_backoff_ms = [100u64, 200u64];
        for attempt in 0..2 {
            match policy.decide(&outcome_with_error(err.clone(), attempt)).await {
                RetryDirective::Retry { backoff_ms } => assert_eq!(
                    backoff_ms, expected_backoff_ms[attempt as usize],
                    "attempt={attempt}: expected backoff 2^{attempt} * 100 = \
                     {}, got {backoff_ms}",
                    expected_backoff_ms[attempt as usize]
                ),
                other => panic!("attempt={attempt}: expected Retry, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn unlimited_budget_always_ok() {
        let budget = UnlimitedBudget;
        for cents in [0u32, 1, 1_000, u32::MAX] {
            assert!(budget.try_consume(cents).await.is_ok());
        }
    }

    #[tokio::test]
    async fn fixed_budget_drains_and_exhausts() {
        let budget = FixedBudget::new(100);
        assert!(budget.try_consume(40).await.is_ok());
        assert_eq!(budget.remaining(), 60);
        assert!(budget.try_consume(60).await.is_ok());
        assert_eq!(budget.remaining(), 0);
        assert_eq!(budget.try_consume(1).await, Err(BudgetExhausted));
    }

    #[tokio::test]
    async fn fixed_budget_rejects_oversized_request() {
        let budget = FixedBudget::new(50);
        assert_eq!(budget.try_consume(100).await, Err(BudgetExhausted));
        assert_eq!(budget.remaining(), 50, "rejected debit must not deduct");
    }

    #[test]
    fn compute_backoff_ms_is_capped() {
        assert_eq!(compute_backoff_ms(0, 1000), 100);
        assert_eq!(compute_backoff_ms(1, 1000), 200);
        assert_eq!(compute_backoff_ms(10, 1000), 1000);
        assert_eq!(compute_backoff_ms(63, 1000), 1000);
    }
}
