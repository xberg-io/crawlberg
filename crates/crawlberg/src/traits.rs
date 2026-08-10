//! Trait-based extension points for the crawl engine.
#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use crate::error::CrawlError;
use crate::types::{CachedPage, CrawlPageResult, ScrapeResult};
use async_trait::async_trait;

/// An entry in the URL frontier queue.
#[derive(Debug, Clone)]
pub struct FrontierEntry {
    /// URL waiting to be crawled.
    pub url: String,
    /// Crawl depth at which this URL was discovered.
    pub depth: usize,
    /// Document-only depth: number of consecutive `LinkType::Document` hops from
    /// the nearest ancestor HTML page. Incremented each time a `Document` link is
    /// re-enqueued via `follow_document_urls`. Zero for ordinary HTML pages.
    pub doc_depth: u32,
    /// Priority score for this entry. Higher values mean higher priority.
    pub priority: f64,
}

/// Statistics about an ongoing or completed crawl.
#[derive(Debug, Clone, Default)]
pub struct CrawlStats {
    /// Number of pages successfully crawled so far.
    pub pages_crawled: usize,
    /// Number of pages that failed to crawl (network errors, parse failures, etc.).
    pub pages_failed: usize,
    /// Total number of URLs discovered (queued + crawled + filtered).
    pub urls_discovered: usize,
    /// Number of URLs rejected by filters before being crawled.
    pub urls_filtered: usize,
    /// Wall-clock time elapsed since the crawl started.
    pub elapsed: Duration,
}

/// Emitted by the engine when a page has finished being processed.
#[derive(Debug, Clone)]
pub struct PageEvent {
    /// URL of the page that was processed.
    pub url: String,
    /// Final HTTP status code returned by the page request.
    pub status_code: u16,
    /// Crawl depth at which this page was reached.
    pub depth: usize,
}

/// Emitted when a page fails to be processed.
#[derive(Debug, Clone)]
pub struct ErrorEvent {
    /// URL that triggered the error.
    pub url: String,
    /// Human-readable error description.
    pub error: String,
}

/// Emitted when the crawl completes (all queues drained or limits reached).
#[derive(Debug, Clone)]
pub struct CompleteEvent {
    /// Final count of successfully crawled pages.
    pub pages_crawled: usize,
}

/// URL queue and deduplication.
///
/// The engine uses `is_seen`/`mark_seen` for URL deduplication during crawling.
/// The `push`/`pop` methods are available for custom frontier implementations
/// (e.g., distributed queues, persistent URL storage) but the default engine
/// manages its own in-memory working set for strategy-based URL selection.
/// This design keeps the hot path lock-free and allows the strategy to have
/// random access to all candidates for intelligent selection.
#[async_trait]
pub trait Frontier: Send + Sync {
    /// Push a new entry onto the frontier.
    async fn push(&self, entry: FrontierEntry) -> Result<(), CrawlError>;

    /// Pop the next entry from the frontier.
    async fn pop(&self) -> Result<Option<FrontierEntry>, CrawlError>;

    /// Pop up to `n` entries from the frontier.
    async fn pop_batch(&self, n: usize) -> Result<Vec<FrontierEntry>, CrawlError> {
        let mut batch = Vec::with_capacity(n);
        for _ in 0..n {
            match self.pop().await? {
                Some(entry) => batch.push(entry),
                None => break,
            }
        }
        Ok(batch)
    }

    /// Return the number of entries in the frontier.
    async fn len(&self) -> Result<usize, CrawlError>;

    /// Check whether the frontier is empty.
    async fn is_empty(&self) -> Result<bool, CrawlError> {
        Ok(self.len().await? == 0)
    }

    /// Check whether a URL has already been seen.
    async fn is_seen(&self, url: &str) -> Result<bool, CrawlError>;

    /// Mark a URL as seen.
    async fn mark_seen(&self, url: &str) -> Result<(), CrawlError>;

    /// Return a fresh instance scoped to a single crawl call, or `None` to keep
    /// sharing this instance's state across calls.
    ///
    /// `CrawlEngine` is designed to be constructed once and reused for many
    /// `crawl()`/`batch_crawl()` calls (see `CrawlEngineBuilder` docs). Because
    /// `CrawlEngine::clone()` shares the same `Arc<dyn Frontier>`, an
    /// implementation whose `seen` set is never cleared would leak state from
    /// one call into the next, silently truncating later crawls, and would race
    /// under concurrent `batch_crawl` calls sharing the same `seen` set.
    ///
    /// ~keep The default `None` preserves existing behavior for implementations
    /// that intentionally persist `seen` state across calls (e.g. a distributed
    /// or resumable frontier backed by external storage). `InMemoryFrontier`
    /// overrides this to return a fresh, empty instance per call.
    fn isolated(&self) -> Option<Arc<dyn Frontier>> {
        None
    }
}

/// Per-domain rate limiting / throttling.
#[async_trait]
pub trait RateLimiter: Send + Sync {
    /// Wait until a request to the given domain is permitted.
    async fn acquire(&self, domain: &str) -> Result<(), CrawlError>;

    /// Record a response status for adaptive back-off.
    async fn record_response(&self, domain: &str, status: u16) -> Result<(), CrawlError>;

    /// Set the crawl-delay for a domain (e.g. from robots.txt).
    async fn set_crawl_delay(&self, domain: &str, delay: Duration) -> Result<(), CrawlError>;
}

/// Persistence for crawl results.
#[async_trait]
pub trait CrawlStore: Send + Sync {
    /// Store a successfully scraped page.
    async fn store_page(&self, url: &str, result: &ScrapeResult) -> Result<(), CrawlError>;

    /// Store a crawl page result.
    async fn store_crawl_page(&self, url: &str, result: &CrawlPageResult) -> Result<(), CrawlError>;

    /// Store an error encountered while crawling a URL.
    async fn store_error(&self, url: &str, error: &CrawlError) -> Result<(), CrawlError>;

    /// Called once when the crawl completes.
    async fn on_complete(&self, stats: &CrawlStats) -> Result<(), CrawlError>;
}

/// Crawl lifecycle event emitter.
#[async_trait]
pub trait EventEmitter: Send + Sync {
    /// A page was crawled.
    async fn on_page(&self, event: &PageEvent);

    /// An error occurred.
    async fn on_error(&self, event: &ErrorEvent);

    /// The crawl completed.
    async fn on_complete(&self, event: &CompleteEvent);

    /// A new URL was discovered.
    async fn on_discovered(&self, url: &str, depth: usize);
}

/// Crawl strategy for URL selection and scoring.
///
/// This is a synchronous trait -- implementations must be `Send + Sync`.
pub trait CrawlStrategy: Send + Sync {
    /// Select the next URL to crawl from a set of candidates.
    /// Returns the index into `candidates`, or `None` if none should be selected.
    fn select_next(&self, candidates: &[FrontierEntry]) -> Option<usize>;

    /// Score a URL for prioritisation.
    fn score_url(&self, url: &str, depth: usize) -> f64 {
        let _ = url;
        1.0 / (depth as f64 + 1.0)
    }

    /// Whether the crawl should continue given current stats.
    fn should_continue(&self, stats: &CrawlStats) -> bool {
        let _ = stats;
        true
    }

    /// Called after each page is processed. Used by adaptive strategies to track content.
    fn on_page_processed(&self, _page: &CrawlPageResult) {}
}

/// Post-extraction content filter.
#[async_trait]
pub trait ContentFilter: Send + Sync {
    /// Filter a crawled page. Return `None` to discard it.
    async fn filter(&self, page: CrawlPageResult) -> Result<Option<CrawlPageResult>, CrawlError>;
}

/// HTTP response cache for avoiding re-fetching unchanged pages.
#[async_trait]
pub trait CrawlCache: Send + Sync {
    /// Get a cached page by URL key.
    async fn get(&self, key: &str) -> Result<Option<CachedPage>, CrawlError>;
    /// Store a page in the cache.
    async fn set(&self, key: &str, page: &CachedPage) -> Result<(), CrawlError>;
    /// Check if a URL is cached.
    async fn has(&self, key: &str) -> Result<bool, CrawlError>;
}
