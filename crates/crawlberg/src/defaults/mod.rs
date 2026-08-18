//! Default trait implementations for crawlberg.

mod cache;
pub mod dispatch;
pub mod domain_state;
mod emitter;
mod filter;
mod frontier;
mod llm_extractor;
mod rate_limiter;
mod store;
mod strategy;

pub use cache::NoopCache;
pub use dispatch::{
    FixedBudget, SimpleRetryPolicy, UnlimitedBudget, compute_backoff_ms, default_retry_policy, unlimited_budget,
};
pub use domain_state::{EwmaDomainState, EwmaTracker, LearningRetryPolicy, in_memory_domain_state};
pub use emitter::NoopEmitter;
pub use filter::NoopFilter;
pub use frontier::{InMemoryFrontier, LifoFrontier};
#[cfg(test)]
pub use rate_limiter::NoopRateLimiter;
pub use rate_limiter::PerDomainThrottle;
pub use store::NoopStore;
pub use strategy::{AdaptiveStrategy, BestFirstStrategy, BfsStrategy, DfsStrategy};
