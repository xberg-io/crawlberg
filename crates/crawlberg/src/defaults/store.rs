//! A no-op crawl store that discards all data.

use async_trait::async_trait;

use crate::error::CrawlError;
use crate::traits::{CrawlStats, CrawlStore};
use crate::types::{CrawlPageResult, ScrapeResult};

/// A store that does nothing -- crawl results are discarded.
#[derive(Debug, Clone, Default)]
pub struct NoopStore;

#[async_trait]
impl CrawlStore for NoopStore {
    async fn store_page(&self, _url: &str, _result: &ScrapeResult) -> Result<(), CrawlError> {
        Ok(())
    }

    async fn store_crawl_page(&self, _url: &str, _result: &CrawlPageResult) -> Result<(), CrawlError> {
        Ok(())
    }

    async fn store_error(&self, _url: &str, _error: &CrawlError) -> Result<(), CrawlError> {
        Ok(())
    }

    async fn on_complete(&self, _stats: &CrawlStats) -> Result<(), CrawlError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::CrawlStats;

    #[tokio::test]
    async fn test_noop_store_all_methods_ok() {
        let store = NoopStore;
        assert!(store.store_page("url", &ScrapeResult::default()).await.is_ok());
        assert!(store.store_crawl_page("url", &CrawlPageResult::default()).await.is_ok());
        assert!(store.store_error("url", &CrawlError::other("test")).await.is_ok());
        assert!(store.on_complete(&CrawlStats::default()).await.is_ok());
    }
}
