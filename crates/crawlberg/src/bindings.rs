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
    #[cfg(feature = "browser")]
    owned_browser_pool: Option<std::sync::Arc<crate::browser_pool::BrowserPool>>,
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
        Self {
            inner: engine,
            #[cfg(feature = "browser")]
            owned_browser_pool: None,
        }
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
    let config = config.unwrap_or_default();
    config.validate()?;

    #[cfg(feature = "browser")]
    let (config, browser_pool, owned_browser_pool) = {
        let mut config = config;
        let (browser_pool, owned_browser_pool) = binding_browser_pool(&mut config);
        (config, browser_pool, owned_browser_pool)
    };

    let builder = CrawlEngine::builder().config(config);
    #[cfg(feature = "browser")]
    let builder = match browser_pool {
        Some(pool) => builder.with_browser_pool(pool),
        None => builder,
    };
    let engine = builder.build()?;
    Ok(CrawlEngineHandle {
        inner: engine,
        #[cfg(feature = "browser")]
        owned_browser_pool,
    })
}

#[cfg(feature = "browser")]
fn binding_browser_pool(
    config: &mut CrawlConfig,
) -> (
    Option<std::sync::Arc<crate::browser_pool::BrowserPool>>,
    Option<std::sync::Arc<crate::browser_pool::BrowserPool>>,
) {
    // ~keep A named browser profile determines Chrome's user-data directory at launch, while
    // ~keep a shared pool launches before any crawl can claim that profile.
    if config.browser.backend != crate::types::BrowserBackend::Chromiumoxide || config.browser_profile.is_some() {
        return (None, None);
    }

    if let Some(pool) = config.browser_pool.clone() {
        return (Some(pool), None);
    }

    // ~keep A parked affinity page currently runs without an SSRF watch (#179), and it retains
    // ~keep a pool permit. Binding-created pools therefore reuse Chrome, but close each page as
    // ~keep the pre-pool binding path did, until parked pages have safe lifecycle ownership.
    config.browser.session_affinity = false;
    let pool = crate::browser_pool::BrowserPool::new(binding_browser_pool_config(config));
    (Some(std::sync::Arc::clone(&pool)), Some(pool))
}

#[cfg(feature = "browser")]
fn binding_browser_pool_config(config: &CrawlConfig) -> crate::browser_pool::BrowserPoolConfig {
    let defaults = crate::browser_pool::BrowserPoolConfig::default();
    crate::browser_pool::BrowserPoolConfig {
        max_pages: config.max_concurrent.unwrap_or(defaults.max_pages),
        browser_endpoint: config.browser.endpoint.clone(),
        chrome_path: config.browser.chrome_path.clone(),
        chrome_args: config.browser.chrome_args.clone(),
        ..defaults
    }
}

/// Shut down browser resources created by [`create_engine`].
///
/// The handle is consumed so it cannot be used after shutdown. A browser pool supplied by a
/// Rust caller through [`CrawlConfig::browser_pool`] remains caller-owned and is not shut down.
/// Shutting down one clone also disables the binding-created pool shared by its sibling clones.
#[cfg_attr(alef, alef(skip))]
pub async fn shutdown_engine(engine: CrawlEngineHandle) {
    #[cfg(feature = "browser")]
    if let Some(pool) = engine.owned_browser_pool.as_ref() {
        pool.shutdown().await;
    }

    drop(engine);
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
        return Err(CrawlError::invalid_config("batch_urls must not be empty"));
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
        return Err(CrawlError::invalid_config("batch_urls must not be empty"));
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

#[cfg(all(test, feature = "browser"))]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{BrowserBackend, BrowserConfig};

    #[test]
    fn create_engine_installs_one_browser_pool_for_all_handle_clones() {
        let handle = create_engine(Some(CrawlConfig {
            max_concurrent: Some(1),
            browser: BrowserConfig {
                backend: BrowserBackend::Chromiumoxide,
                ..BrowserConfig::default()
            },
            ..CrawlConfig::default()
        }))
        .expect("binding engine must build");
        let cloned = handle.clone();

        let pool = handle
            .inner
            .config
            .browser_pool
            .as_ref()
            .expect("binding engine must own a browser pool");
        let cloned_pool = cloned
            .inner
            .config
            .browser_pool
            .as_ref()
            .expect("cloned handle must retain the browser pool");
        assert!(
            Arc::ptr_eq(pool, cloned_pool),
            "handle clones must share one browser pool"
        );

        assert!(!handle.inner.config.browser.session_affinity);
        assert!(handle.inner.config.browser_session_pool.is_none());
    }

    #[tokio::test]
    async fn shutdown_engine_shuts_down_the_binding_created_browser_pool() {
        let handle = create_engine(Some(CrawlConfig {
            browser: BrowserConfig {
                backend: BrowserBackend::Chromiumoxide,
                ..BrowserConfig::default()
            },
            ..CrawlConfig::default()
        }))
        .expect("binding engine must build");
        let pool = Arc::clone(
            handle
                .inner
                .config
                .browser_pool
                .as_ref()
                .expect("binding engine must own a browser pool"),
        );

        shutdown_engine(handle).await;

        let error = match pool.acquire_page().await {
            Ok(_) => panic!("shutdown must make the binding-created pool reject acquisition"),
            Err(error) => error,
        };
        assert_eq!(error.to_string(), "browser: pool is shut down");
    }

    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::print_stderr, reason = "test-only skip announcement")]
    async fn shutdown_engine_returns_after_its_chrome_process_family_exits() {
        const TEST_NAME: &str = "shutdown_engine_returns_after_its_chrome_process_family_exits";
        let handle = create_engine(Some(CrawlConfig {
            browser: BrowserConfig {
                backend: BrowserBackend::Chromiumoxide,
                ..BrowserConfig::default()
            },
            ..CrawlConfig::default()
        }))
        .expect("binding engine must build");
        let pool = Arc::clone(
            handle
                .inner
                .config
                .browser_pool
                .as_ref()
                .expect("binding engine must own a browser pool"),
        );
        if crate::browser_pool::tests::expect_chrome_or_skip(TEST_NAME, pool.warm().await).is_none() {
            return;
        }
        let profile = crate::browser_pool::tests::pool_profile_dir(&pool).await;
        let launched_processes = crate::browser_pool::tests::chrome_process_count_for_profile(&profile);
        assert!(
            launched_processes > 0,
            "the regression must observe the Chrome process family before testing shutdown"
        );

        shutdown_engine(handle).await;

        assert_eq!(
            crate::browser_pool::tests::chrome_process_count_for_profile(&profile),
            0,
            "shutdown must not return while a Chrome-family process still uses its unique profile"
        );
    }

    #[test]
    fn binding_browser_pool_config_carries_engine_launch_options() {
        let config = CrawlConfig {
            max_concurrent: Some(3),
            browser: BrowserConfig {
                endpoint: Some("ws://browser.example:9222/devtools/browser/test".to_owned()),
                chrome_path: Some("/opt/chrome".into()),
                chrome_args: vec!["--lang=de".to_owned()],
                ..BrowserConfig::default()
            },
            ..CrawlConfig::default()
        };

        let pool_config = binding_browser_pool_config(&config);

        assert_eq!(pool_config.max_pages, 3);
        assert_eq!(pool_config.browser_endpoint, config.browser.endpoint);
        assert_eq!(pool_config.chrome_path, config.browser.chrome_path);
        assert_eq!(pool_config.chrome_args, config.browser.chrome_args);
    }

    #[test]
    fn create_engine_preserves_a_rust_callers_browser_pools() {
        let browser_pool = crate::browser_pool::BrowserPool::new(crate::browser_pool::BrowserPoolConfig::default());
        let session_pool = Arc::new(crate::browser_session_pool::BrowserSessionPool::new());
        let handle = create_engine(Some(CrawlConfig {
            browser_pool: Some(Arc::clone(&browser_pool)),
            browser_session_pool: Some(Arc::clone(&session_pool)),
            ..CrawlConfig::default()
        }))
        .expect("binding engine must build");

        assert!(Arc::ptr_eq(
            handle
                .inner
                .config
                .browser_pool
                .as_ref()
                .expect("configured browser pool must remain installed"),
            &browser_pool
        ));
        assert!(Arc::ptr_eq(
            handle
                .inner
                .config
                .browser_session_pool
                .as_ref()
                .expect("configured session pool must remain installed"),
            &session_pool
        ));
        assert!(
            handle.owned_browser_pool.is_none(),
            "an externally injected pool must not become owned by the handle"
        );
        assert!(handle.inner.config.browser.session_affinity);
    }

    #[test]
    fn create_engine_keeps_profile_backed_browser_fetches_one_shot() {
        let handle = create_engine(Some(CrawlConfig {
            browser_profile: Some("signed-in".to_owned()),
            ..CrawlConfig::default()
        }))
        .expect("binding engine must build");

        assert!(
            handle.inner.config.browser_pool.is_none(),
            "a shared pool cannot launch the requested per-crawl browser profile"
        );
        assert!(handle.inner.config.browser_session_pool.is_none());
    }
}
