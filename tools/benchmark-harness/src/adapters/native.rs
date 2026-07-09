//! Native crawlberg adapter — drives the Rust core library directly.

use std::time::Duration;

use async_trait::async_trait;
use crawlberg::{CrawlConfig, CrawlEngineHandle, create_engine};

use crate::Result;
use crate::adapter::{ScrapeAdapter, ScrapeInput, ScrapeOutput};

/// Adapter that calls the native crawlberg scraping engine in-process.
///
/// This adapter uses the public binding API (`crawlberg::scrape` /
/// `crawlberg::batch_scrape`) so it exercises the same code path as every
/// language binding.
pub struct NativeAdapter {
    engine: CrawlEngineHandle,
}

impl NativeAdapter {
    /// Create a new adapter with the default [`CrawlConfig`].
    pub fn new() -> Result<Self> {
        Self::with_config(CrawlConfig::default())
    }

    /// Create an adapter with a custom [`CrawlConfig`].
    pub fn with_config(config: CrawlConfig) -> Result<Self> {
        let engine =
            create_engine(Some(config)).map_err(|e| crate::Error::Adapter(format!("failed to create engine: {e}")))?;
        Ok(Self { engine })
    }
}

#[async_trait]
impl ScrapeAdapter for NativeAdapter {
    fn name(&self) -> &str {
        "crawlberg-native"
    }

    fn version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }

    fn supports_batch(&self) -> bool {
        false
    }

    /// Scrape a single URL, wrapping the call with the given timeout.
    ///
    /// `cached_html` is accepted for interface compatibility but ignored here:
    /// in cached mode the runner remaps URLs to a localhost server so the
    /// engine fetches pages normally.
    async fn scrape(&self, url: &str, _cached_html: Option<&str>, timeout: Duration) -> Result<ScrapeOutput> {
        let result = tokio::time::timeout(timeout, crawlberg::scrape(&self.engine, url))
            .await
            .map_err(|_| crate::Error::Adapter(format!("scrape timed out after {timeout:?}")))?
            .map_err(|e| crate::Error::Adapter(format!("scrape failed: {e}")))?;

        Ok(scrape_result_to_output(result))
    }

    /// Scrape multiple URLs using the engine's native batch path.
    ///
    /// `cached_html` fields in `entries` are not forwarded to the batch API.
    /// If any entry carries a `cached_html` value a warning is emitted because
    /// the cache-served HTML will not be used; callers should route cached runs
    /// through the sequential [`scrape`](Self::scrape) path instead (i.e. set
    /// [`supports_batch`](Self::supports_batch) to `false`, which this adapter
    /// does).
    ///
    /// The internal timeout is governed by the [`CrawlConfig`] passed at
    /// construction time — no additional wrapper is applied here.
    async fn batch_scrape(&self, entries: &[ScrapeInput], _timeout: Duration) -> Result<Vec<ScrapeOutput>> {
        for entry in entries {
            if entry.cached_html.is_some() {
                tracing::warn!(
                    url = %entry.url,
                    "batch_scrape: cached_html is set but will be ignored; \
                     use sequential scrape for cached mode"
                );
            }
        }

        let urls: Vec<String> = entries.iter().map(|e| e.url.clone()).collect();

        let batch_results = crawlberg::batch_scrape(&self.engine, urls)
            .await
            .map_err(|e| crate::error::Error::Adapter(format!("batch_scrape failed: {e}")))?;

        let outputs = batch_results
            .results
            .into_iter()
            .map(|batch| {
                if let Some(result) = batch.result {
                    scrape_result_to_output(result)
                } else {
                    let error_msg = batch.error.unwrap_or_else(|| "unknown error".to_string());
                    ScrapeOutput {
                        status_code: 0,
                        content: None,
                        html: String::new(),
                        content_size: 0,
                        browser_used: false,
                        js_render_hint: false,
                        error: Some(error_msg),
                    }
                }
            })
            .collect();

        Ok(outputs)
    }
}

/// Convert a [`crawlberg::ScrapeResult`] into the harness [`ScrapeOutput`].
fn scrape_result_to_output(result: crawlberg::ScrapeResult) -> ScrapeOutput {
    let content = result.markdown.map(|m| m.content);
    ScrapeOutput {
        status_code: result.status_code,
        content,
        html: result.html,
        content_size: result.body_size,
        browser_used: result.browser_used,
        js_render_hint: result.js_render_hint,
        error: None,
    }
}
