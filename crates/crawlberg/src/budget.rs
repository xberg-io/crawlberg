//! Pluggable page budget hook for controlling crawl extent.
//!
//! Allows embedders to enforce per-crawl page allowances without crawlberg
//! having any knowledge of how budgets are computed or enforced. The hook is
//! consulted before each page fetch; a return of `Err(Exhausted)` halts the
//! crawl gracefully.

use async_trait::async_trait;

/// External policy hook for page budget enforcement.
///
/// Implementations are consulted before each page fetch during a crawl.
/// Returning `Err(BudgetError::Exhausted)` signals the crawl loop to stop
/// gracefully and return pages fetched so far. Returning `Err(BudgetError::Backend)`
/// is also treated as exhaustion for safety (fail-closed).
///
/// The budget hook is completely tenant-agnostic — crawlberg only checks and
/// does not care how the budget is implemented or what it represents.
#[async_trait]
pub trait PageBudget: Send + Sync + 'static {
    /// Consult the budget before fetching a page.
    ///
    /// Return `Ok(())` to permit the fetch. Return `Err(BudgetError::Exhausted)`
    /// to halt the crawl gracefully. Return `Err(BudgetError::Backend(...))` to
    /// signal an error consulting the budget; this is treated as exhaustion.
    async fn check(&self) -> Result<(), BudgetError>;
}

/// Errors returned by a [`PageBudget`] implementation.
#[derive(Debug, thiserror::Error)]
pub enum BudgetError {
    /// Page allowance has been exhausted.
    #[error("page budget exhausted")]
    Exhausted,

    /// The budget backend encountered an error (e.g. database outage).
    ///
    /// The crawl treats this as exhaustion (fail-closed) to avoid
    /// continuing indefinitely if the budget check is unavailable.
    #[error("budget backend error: {0}")]
    Backend(String),
}

/// Default no-op page budget that always permits page fetches.
///
/// Use as the default when no external budget policy is configured.
#[derive(Default, Clone)]
pub struct DefaultPageBudget;

#[async_trait]
impl PageBudget for DefaultPageBudget {
    async fn check(&self) -> Result<(), BudgetError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn default_page_budget_always_permits() {
        let budget = DefaultPageBudget;
        for _ in 0..100 {
            assert!(budget.check().await.is_ok());
        }
    }

    /// Test budget that exhausts after n calls.
    struct ConsumeN {
        remaining: Arc<AtomicUsize>,
    }

    impl ConsumeN {
        fn new(n: usize) -> Self {
            Self {
                remaining: Arc::new(AtomicUsize::new(n)),
            }
        }
    }

    #[async_trait]
    impl PageBudget for ConsumeN {
        async fn check(&self) -> Result<(), BudgetError> {
            match self.remaining.fetch_sub(1, Ordering::SeqCst) {
                0 => Err(BudgetError::Exhausted),
                _ => Ok(()),
            }
        }
    }

    #[tokio::test]
    async fn consume_n_budget_exhausts() {
        let budget = ConsumeN::new(3);
        assert!(budget.check().await.is_ok());
        assert!(budget.check().await.is_ok());
        assert!(budget.check().await.is_ok());
        assert!(matches!(budget.check().await, Err(BudgetError::Exhausted)));
    }

    /// Test budget that returns a backend error.
    struct BackendError;

    #[async_trait]
    impl PageBudget for BackendError {
        async fn check(&self) -> Result<(), BudgetError> {
            Err(BudgetError::Backend("database connection lost".into()))
        }
    }

    #[tokio::test]
    async fn backend_error_treated_as_exhausted() {
        let budget = BackendError;
        assert!(matches!(
            budget.check().await,
            Err(BudgetError::Backend(msg)) if msg == "database connection lost"
        ));
    }
}
