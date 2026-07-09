//! Property-based invariant tests for three behavioural surfaces:
//!
//!   A. EWMA update function — bounded, converges to 0/1.
//!   B. FixedBudget — sum of approved demands ≤ initial; concurrent never-overdraw.
//!   C. compute_backoff_ms — capped at max; monotonic until cap.
//!
//! These tests exercise the real public types (re-exported from `crawlberg`
//! via `#[doc(hidden)]`-equivalent root re-exports). The previous mirrors were
//! removed because they could silently drift from the implementations.

use std::sync::{Arc, OnceLock};

use crawlberg::{EscalationBudget, EwmaTracker, FixedBudget, compute_backoff_ms};
use proptest::prelude::*;
use tokio::runtime::Runtime;

/// Shared single-threaded runtime for the synchronous proptest bodies.
/// `FixedBudget::try_consume` is async; calling it from a sync proptest body
/// requires a runtime. One process-wide runtime avoids 256-runtime churn.
fn rt() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| Runtime::new().unwrap())
}

proptest! {
    /// A1: update output is always in [0.0, 1.0] for any valid prev and blocked flag.
    #[test]
    fn ewma_update_is_bounded(prev in 0.0f32..=1.0, blocked: bool) {
        let tracker = EwmaTracker::default();
        let next = tracker.update(prev, blocked);
        prop_assert!(
            next >= 0.0,
            "EwmaTracker::update({prev}, {blocked}) = {next} — violated lower bound 0.0"
        );
        prop_assert!(
            next <= 1.0,
            "EwmaTracker::update({prev}, {blocked}) = {next} — violated upper bound 1.0"
        );
    }

    /// A2: 200 consecutive blocked observations converge to within 0.01 of 1.0.
    #[test]
    fn ewma_all_block_converges_to_one(prev in 0.0f32..=1.0) {
        let tracker = EwmaTracker::default();
        let mut ewma = prev;
        for _ in 0..200 {
            ewma = tracker.update(ewma, true);
        }
        prop_assert!(
            ewma >= 0.99,
            "all-block convergence: after 200 blocks starting from {prev}, ewma={ewma} < 0.99"
        );
    }

    /// A3: 200 consecutive success observations converge to within 0.01 of 0.0.
    #[test]
    fn ewma_all_success_converges_to_zero(prev in 0.0f32..=1.0) {
        let tracker = EwmaTracker::default();
        let mut ewma = prev;
        for _ in 0..200 {
            ewma = tracker.update(ewma, false);
        }
        prop_assert!(
            ewma <= 0.01,
            "all-success convergence: after 200 successes starting from {prev}, ewma={ewma} > 0.01"
        );
    }
}

proptest! {
    /// B1: sum of approved demands never exceeds the initial budget.
    #[test]
    fn fixed_budget_approved_sum_never_exceeds_initial(
        initial in 1u32..=1000,
        demands in proptest::collection::vec(0u32..=1000, 0..=50),
    ) {
        let budget = FixedBudget::new(initial);
        let mut approved_sum: u64 = 0;
        for demand in &demands {
            if rt().block_on(budget.try_consume(*demand)).is_ok() {
                approved_sum += u64::from(*demand);
            }
        }
        prop_assert!(
            approved_sum <= u64::from(initial),
            "approved_sum={approved_sum} > initial={initial}"
        );
    }
}

/// B2: 100 concurrent try_consume(1) tasks on a budget of 100 — exactly 100 succeed.
///
/// proptest! macro is sync; use tokio runtime inline. Standalone (not in
/// proptest!) because property-style randomization adds nothing here.
#[test]
fn fixed_budget_concurrent_never_overdraws() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(8)
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let budget = Arc::new(FixedBudget::new(100));
        let mut handles = Vec::with_capacity(100);
        for _ in 0..100 {
            let b = budget.clone();
            handles.push(tokio::spawn(async move { b.try_consume(1).await }));
        }
        let mut approvals: u32 = 0;
        for h in handles {
            if h.await.unwrap().is_ok() {
                approvals += 1;
            }
        }
        assert_eq!(
            approvals, 100,
            "expected exactly 100 approvals with initial=100, got {approvals}"
        );
        assert_eq!(
            budget.remaining(),
            0,
            "remaining should be 0 after 100 approved try_consume(1), got {}",
            budget.remaining()
        );
    });
}

proptest! {
    /// C1: compute_backoff_ms is always <= max_backoff_ms.
    #[test]
    fn backoff_never_exceeds_max(
        attempt in 0u32..32,
        max_backoff_ms in 1000u64..=60_000,
    ) {
        let backoff = compute_backoff_ms(attempt, max_backoff_ms);
        prop_assert!(
            backoff <= max_backoff_ms,
            "compute_backoff_ms({attempt}, {max_backoff_ms}) = {backoff} exceeds cap"
        );
    }

    /// C2: compute_backoff_ms is monotonically non-decreasing until the cap is reached.
    ///
    /// For attempt values where 2^attempt * 100 < max_backoff_ms, the next attempt
    /// must produce a value >= this attempt's value.
    #[test]
    fn backoff_is_monotonic_until_cap(
        attempt in 0u32..30,
        max_backoff_ms in 1000u64..=60_000,
    ) {
        let this_backoff = compute_backoff_ms(attempt, max_backoff_ms);
        let next_backoff = compute_backoff_ms(attempt + 1, max_backoff_ms);

        prop_assert!(
            next_backoff >= this_backoff,
            "compute_backoff_ms({attempt}) = {this_backoff} > compute_backoff_ms({}) = {next_backoff}",
            attempt + 1
        );
    }
}
