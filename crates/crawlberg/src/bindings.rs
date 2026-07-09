//! Bridge between crawlberg's trait-based engine and polyglot bindings.
//!
//! The core [`CrawlEngine`] uses `Arc<dyn Trait>` for pluggable components,
//! which cannot cross FFI boundaries. This module provides a config-only
//! construction path with default implementations, plus async adapter
//! functions that alef can generate bindings for.
//!
//! # Telemetry propagation
//!
//! [`with_traceparent`] and [`current_traceparent`] are Rust-only helpers.
//! They are not part of the alef binding surface because `with_traceparent`
//! requires a Rust callback. Language clients should propagate trace context
//! through their host OpenTelemetry SDK until a dedicated binding-safe
//! trace-context API exists.

#[allow(unused_imports)]
pub use crate::telemetry::{current_traceparent, with_traceparent};

use crate::engine::CrawlEngine;
use crate::error::CrawlError;
use crate::interact::PageAction;
#[cfg(not(target_arch = "wasm32"))]
use crate::types::{BatchCrawlStreamRequest, CrawlEvent, CrawlStreamRequest};
use crate::types::{CrawlConfig, CrawlResult, InteractionResult, MapResult, ScrapeResult};
#[cfg(not(target_arch = "wasm32"))]
use futures::future::BoxFuture;
#[cfg(not(target_arch = "wasm32"))]
use futures::stream::{BoxStream, StreamExt};
use serde::{Deserialize, Serialize};

/// Opaque handle to a configured crawl engine.
///
/// Constructed via [`create_engine`] with an optional [`CrawlConfig`].
/// Default implementations for all pluggable components are used internally.
#[derive(Clone)]
pub struct CrawlEngineHandle {
    inner: CrawlEngine,
}

impl CrawlEngineHandle {
    /// Wrap a pre-built [`CrawlEngine`] as a handle.
    ///
    /// Use this when you need to inject Rust-only components (a pre-built
    /// `BrowserPool` from `crate::browser_pool` or `NativeBrowserExecutor`
    /// from `crawlberg_browser::adapter`, both feature-gated behind
    /// `browser`) via [`crate::CrawlEngineBuilder`] and then expose the
    /// result through the binding-friendly `CrawlEngineHandle` API.
    ///
    /// Rust-only: excluded from alef-generated polyglot bindings. Language
    /// clients construct handles via [`create_engine`] alone.
    #[cfg_attr(alef, alef(skip))]
    pub fn from_engine(engine: CrawlEngine) -> Self {
        Self { inner: engine }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl CrawlEngineHandle {
    /// Stream a single-URL crawl, yielding [`CrawlEvent`]s as pages are processed.
    ///
    /// Returns an async stream that emits one event per crawled page, plus a
    /// terminal `Complete` event. On per-URL failure during the crawl, emits an
    /// `Error` event followed by `Complete`. The stream item type is wrapped in
    /// a `Result` to surface transport-level errors; today every emit is `Ok`.
    ///
    /// Language bindings expose this as the native streaming shape for each
    /// target. WASM does not expose native streaming wrappers.
    pub fn crawl_stream(
        &self,
        req: CrawlStreamRequest,
    ) -> BoxFuture<'static, Result<BoxStream<'static, Result<CrawlEvent, CrawlError>>, CrawlError>> {
        let engine = self.inner.clone();
        Box::pin(async move {
            let stream = engine.crawl_stream(&req.url);
            Ok(stream.map(Ok::<CrawlEvent, CrawlError>).boxed())
        })
    }

    /// Stream a multi-URL crawl, yielding [`CrawlEvent`]s across all seeds.
    ///
    /// Returns an async stream that emits one event per crawled page across all
    /// seeds, plus terminal `Complete` and `Error` events as appropriate. The
    /// stream item type is wrapped in a `Result` to surface transport-level
    /// errors; today every emit is `Ok`.
    ///
    /// Language bindings expose this as the native streaming shape for each
    /// target. WASM does not expose native streaming wrappers.
    pub fn batch_crawl_stream(
        &self,
        req: BatchCrawlStreamRequest,
    ) -> BoxFuture<'static, Result<BoxStream<'static, Result<CrawlEvent, CrawlError>>, CrawlError>> {
        let engine = self.inner.clone();
        Box::pin(async move {
            let url_refs: Vec<&str> = req.urls.iter().map(String::as_str).collect();
            let stream = engine.batch_crawl_stream(&url_refs);
            Ok(stream.map(Ok::<CrawlEvent, CrawlError>).boxed())
        })
    }
}

/// Result from a single URL in a batch scrape operation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BatchScrapeResult {
    /// The URL that was scraped.
    pub url: String,
    /// The scrape result, if successful.
    pub result: Option<ScrapeResult>,
    /// The error message, if the scrape failed.
    pub error: Option<String>,
}

/// Result from a single URL in a batch crawl operation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BatchCrawlResult {
    /// The seed URL that was crawled.
    pub url: String,
    /// The crawl result, if successful.
    pub result: Option<CrawlResult>,
    /// The error message, if the crawl failed.
    pub error: Option<String>,
}

/// Aggregate result of a batch scrape, exposing per-URL results plus precomputed counts.
///
/// The counts are derived once at construction so every binding language can read them
/// as plain integer fields without re-iterating the `results` vector.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BatchScrapeResults {
    /// Per-URL scrape results, in the order URLs were submitted.
    pub results: Vec<BatchScrapeResult>,
    /// Total number of URLs in the batch (equal to `results.len()`).
    pub total_count: usize,
    /// Number of URLs whose scrape succeeded (`error` is `None`).
    pub completed_count: usize,
    /// Number of URLs whose scrape failed (`error` is `Some`).
    pub failed_count: usize,
}

impl From<Vec<BatchScrapeResult>> for BatchScrapeResults {
    fn from(results: Vec<BatchScrapeResult>) -> Self {
        let total_count = results.len();
        let failed_count = results.iter().filter(|r| r.error.is_some()).count();
        let completed_count = total_count - failed_count;
        Self {
            results,
            total_count,
            completed_count,
            failed_count,
        }
    }
}

/// Aggregate result of a batch crawl, exposing per-URL results plus precomputed counts.
///
/// The counts are derived once at construction so every binding language can read them
/// as plain integer fields without re-iterating the `results` vector.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BatchCrawlResults {
    /// Per-URL crawl results, in the order seed URLs were submitted.
    pub results: Vec<BatchCrawlResult>,
    /// Total number of seed URLs in the batch (equal to `results.len()`).
    pub total_count: usize,
    /// Number of seed URLs whose crawl succeeded (`error` is `None`).
    pub completed_count: usize,
    /// Number of seed URLs whose crawl failed (`error` is `Some`).
    pub failed_count: usize,
}

impl From<Vec<BatchCrawlResult>> for BatchCrawlResults {
    fn from(results: Vec<BatchCrawlResult>) -> Self {
        let total_count = results.len();
        let failed_count = results.iter().filter(|r| r.error.is_some()).count();
        let completed_count = total_count - failed_count;
        Self {
            results,
            total_count,
            completed_count,
            failed_count,
        }
    }
}

/// Create a new crawl engine with the given configuration.
///
/// If `config` is `None`, uses [`CrawlConfig::default()`].
/// Returns an error if the configuration is invalid.
pub fn create_engine(config: Option<CrawlConfig>) -> Result<CrawlEngineHandle, CrawlError> {
    let mut builder = CrawlEngine::builder();
    if let Some(config) = config {
        config.validate()?;
        builder = builder.config(config);
    }
    let engine = builder.build()?;
    Ok(CrawlEngineHandle { inner: engine })
}

/// Scrape a single URL, returning extracted page data.
pub async fn scrape(engine: &CrawlEngineHandle, url: &str) -> Result<ScrapeResult, CrawlError> {
    engine.inner.scrape(url).await
}

/// Crawl a website starting from `url`, following links up to the configured depth.
pub async fn crawl(engine: &CrawlEngineHandle, url: &str) -> Result<CrawlResult, CrawlError> {
    engine.inner.crawl(url).await
}

/// Discover all pages on a website by following links and sitemaps.
pub async fn map_urls(engine: &CrawlEngineHandle, url: &str) -> Result<MapResult, CrawlError> {
    engine.inner.map(url).await
}

/// Execute browser actions on a single page.
pub async fn interact(
    engine: &CrawlEngineHandle,
    url: &str,
    actions: Vec<PageAction>,
) -> Result<InteractionResult, CrawlError> {
    engine.inner.interact(url, &actions).await
}

/// Scrape multiple URLs concurrently.
pub async fn batch_scrape(engine: &CrawlEngineHandle, urls: Vec<String>) -> Result<BatchScrapeResults, CrawlError> {
    if urls.is_empty() {
        return Err(CrawlError::InvalidConfig("batch_urls must not be empty".into()));
    }
    let url_refs: Vec<&str> = urls.iter().map(String::as_str).collect();
    let results = engine.inner.batch_scrape(&url_refs).await;
    let per_url: Vec<BatchScrapeResult> = results
        .into_iter()
        .map(|(url, result)| match result {
            Ok(r) => BatchScrapeResult {
                url,
                result: Some(r),
                error: None,
            },
            Err(e) => BatchScrapeResult {
                url,
                result: None,
                error: Some(e.to_string()),
            },
        })
        .collect();
    Ok(BatchScrapeResults::from(per_url))
}

/// Stream a single-URL crawl, yielding [`CrawlEvent`]s as pages are processed.
///
/// Free-function counterpart to [`CrawlEngineHandle::crawl_stream`] that accepts
/// a bare URL (rather than a [`CrawlStreamRequest`]) so it mirrors the calling
/// convention of [`scrape`] / [`crawl`] / [`map_urls`] for the polyglot e2e
/// surface.
#[cfg(not(target_arch = "wasm32"))]
pub async fn crawl_stream(
    engine: &CrawlEngineHandle,
    url: &str,
) -> Result<BoxStream<'static, Result<CrawlEvent, CrawlError>>, CrawlError> {
    engine.crawl_stream(CrawlStreamRequest { url: url.to_string() }).await
}

/// Stream a multi-URL crawl, yielding [`CrawlEvent`]s across all seeds.
///
/// Free-function counterpart to [`CrawlEngineHandle::batch_crawl_stream`] that
/// accepts a bare URL list (rather than a [`BatchCrawlStreamRequest`]) for
/// symmetry with [`batch_scrape`] / [`batch_crawl`].
#[cfg(not(target_arch = "wasm32"))]
pub async fn batch_crawl_stream(
    engine: &CrawlEngineHandle,
    urls: Vec<String>,
) -> Result<BoxStream<'static, Result<CrawlEvent, CrawlError>>, CrawlError> {
    engine.batch_crawl_stream(BatchCrawlStreamRequest { urls }).await
}

/// Crawl multiple seed URLs concurrently, each following links to configured depth.
pub async fn batch_crawl(engine: &CrawlEngineHandle, urls: Vec<String>) -> Result<BatchCrawlResults, CrawlError> {
    if urls.is_empty() {
        return Err(CrawlError::InvalidConfig("batch_urls must not be empty".into()));
    }
    let url_refs: Vec<&str> = urls.iter().map(String::as_str).collect();
    let results = engine.inner.batch_crawl(&url_refs).await;
    let per_url: Vec<BatchCrawlResult> = results
        .into_iter()
        .map(|(url, result)| match result {
            Ok(r) => {
                if let Some(ref err) = r.error {
                    BatchCrawlResult {
                        url,
                        result: None,
                        error: Some(err.clone()),
                    }
                } else {
                    BatchCrawlResult {
                        url,
                        result: Some(r),
                        error: None,
                    }
                }
            }
            Err(e) => BatchCrawlResult {
                url,
                result: None,
                error: Some(e.to_string()),
            },
        })
        .collect();
    Ok(BatchCrawlResults::from(per_url))
}
