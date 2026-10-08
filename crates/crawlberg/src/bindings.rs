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

    /// A site that answers every request with one small page.
    async fn one_page_site() -> wiremock::MockServer {
        let site = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_string("<html><head><title>T</title></head><body><p>Some text.</p></body></html>")
                    .append_header("content-type", "text/html"),
            )
            .mount(&site)
            .await;
        site
    }

    /// An engine as a binding makes it, whose every fetch goes through its pooled Chrome.
    fn browser_engine() -> CrawlEngineHandle {
        create_engine(Some(CrawlConfig {
            browser: BrowserConfig {
                backend: BrowserBackend::Chromiumoxide,
                mode: crate::BrowserMode::Always,
                ..BrowserConfig::default()
            },
            ..CrawlConfig::builder().allow_private_networks(true).build()
        }))
        .expect("binding engine must build")
    }

    fn owned_pool(handle: &CrawlEngineHandle) -> Arc<crate::browser_pool::BrowserPool> {
        Arc::clone(
            handle
                .owned_browser_pool
                .as_ref()
                .expect("binding engine must own a browser pool"),
        )
    }

    /// Whether a Chrome can be launched here. Announces the skip when none can.
    async fn chrome_launches(test_name: &str) -> bool {
        let pool = crate::browser_pool::BrowserPool::new(crate::browser_pool::BrowserPoolConfig::default());
        let launched = crate::browser_pool::tests::expect_chrome_or_skip(test_name, pool.warm().await).is_some();
        pool.shutdown().await;
        launched
    }

    /// The pool does not keep itself alive through a page under its SSRF check: with a watch
    /// still open, dropping every other owner drops the pool.
    ///
    /// ~keep The watched page stored the crawl's config, the config holds the pool, and the pool
    /// ~keep owns the check's listener, which holds the page until Chrome reports its target
    /// ~keep destroyed. When that report did not come, the pool and its Chrome lived for as long
    /// ~keep as the process (xberg-io/crawlberg#594).
    #[tokio::test(flavor = "multi_thread")]
    async fn a_watched_page_does_not_keep_the_browser_pool_alive() {
        const TEST_NAME: &str = "a_watched_page_does_not_keep_the_browser_pool_alive";
        let pool = crate::browser_pool::BrowserPool::new(crate::browser_pool::BrowserPoolConfig::default());
        if crate::browser_pool::tests::expect_chrome_or_skip(TEST_NAME, pool.warm().await).is_none() {
            return;
        }
        let profile = crate::browser_pool::tests::pool_profile_dir(&pool).await;
        let config = CrawlConfig {
            browser_pool: Some(Arc::clone(&pool)),
            ..CrawlConfig::default()
        };
        let (page, _permit) = pool
            .acquire_page()
            .await
            .expect("the pool must open a page")
            .into_parts();
        let firewall = pool.firewall().await.expect("a warm pool has a check");
        let watch = firewall.watch(&page, &config, 0).await.expect("the watch must start");
        let weak = Arc::downgrade(&pool);
        assert!(weak.upgrade().is_some(), "the pool is alive while the test owns it");

        drop(config);
        drop(pool);

        let alive = weak.upgrade().is_some();
        drop(watch);
        drop(page);
        assert!(!alive, "a page under the check must not own the pool it belongs to");
        tokio::task::spawn_blocking(move || {
            crate::browser_pool::tests::assert_profile_directory_is_gone_for_good(&profile);
        })
        .await
        .expect("the dropped pool must stop its Chrome and remove its profile directory");
    }

    /// Dropping the last clone of an engine handle stops the Chrome its fetches used and removes
    /// that Chrome's profile directory, and an earlier clone's drop does neither.
    #[tokio::test(flavor = "multi_thread")]
    async fn dropping_the_last_engine_clone_stops_its_chrome() {
        const TEST_NAME: &str = "dropping_the_last_engine_clone_stops_its_chrome";
        if !chrome_launches(TEST_NAME).await {
            return;
        }
        let site = one_page_site().await;
        let handle = browser_engine();
        scrape(&handle, &site.uri())
            .await
            .expect("the browser scrape must succeed");
        let profile = crate::browser_pool::tests::pool_profile_dir(&owned_pool(&handle)).await;
        let clone = handle.clone();

        drop(handle);

        // ~keep A second scrape proves the Chrome still serves the remaining clone.
        scrape(&clone, &site.uri())
            .await
            .expect("the remaining clone must still scrape through the shared Chrome");
        assert!(
            crate::browser_pool::tests::chrome_process_count_for_profile(&profile) > 0,
            "the Chrome must run while a clone of the handle is alive"
        );

        drop(clone);

        tokio::task::spawn_blocking(move || {
            crate::browser_pool::tests::assert_profile_directory_is_gone_for_good(&profile);
        })
        .await
        .expect("dropping the last clone must stop the Chrome and remove its profile directory");
    }

    /// A page that a pooled fetch closes leaves no target in the SSRF check, and no fetch waits
    /// out the close timeout.
    ///
    /// ~keep On the Chromium headless shell each fetch took five seconds longer and left the
    /// ~keep page's target in the check (xberg-io/crawlberg#595). A Chrome with a target of its own
    /// ~keep does not show it, so this test proves the defect only when `CHROME` names the shell.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_pooled_fetch_leaves_no_target_in_the_check_and_does_not_wait_for_one() {
        const TEST_NAME: &str = "a_pooled_fetch_leaves_no_target_in_the_check_and_does_not_wait_for_one";
        if !chrome_launches(TEST_NAME).await {
            return;
        }
        let site = one_page_site().await;
        let handle = browser_engine();
        let pool = owned_pool(&handle);
        for fetch in 0..3 {
            let started = std::time::Instant::now();
            scrape(&handle, &format!("{}/p{fetch}", site.uri()))
                .await
                .expect("the browser scrape must succeed");
            let elapsed = started.elapsed();
            let left = pool
                .firewall()
                .await
                .expect("a pool that fetched has a check")
                .watched_target_count();
            assert_eq!(left, 0, "fetch {fetch} left its closed page's target in the check");
            assert!(
                elapsed < handle.inner.config.browser.shutdown_timeout,
                "fetch {fetch} took {elapsed:?}, as long as the wait for a page that does not close"
            );
        }
        drop(pool);
        shutdown_engine(handle).await;
    }

    /// Set on the child copy of this test binary that [`assert_exit_leaves_no_chrome`] starts, to
    /// what the child does with its engine before it exits: `keep` or `drop`.
    const EXIT_CHILD: &str = "CRAWLBERG_TEST_EXIT_CHILD";

    /// The marker before the profile directory that the child prints for its parent.
    const EXIT_CHILD_PROFILE_LINE: &str = "crawlberg-exit-child-profile: ";

    /// How long the parent waits for its child to scrape one page and exit.
    const EXIT_CHILD_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

    /// Start this test binary again as a child that scrapes one page through a pooled Chrome,
    /// then keeps or drops its engine as `engine_at_exit` says and exits at once. Return the
    /// profile directory of that Chrome once the child has exited with 0, or `None` when no
    /// Chrome can be launched here. A child that does not exit is killed and fails the test.
    ///
    /// ~keep A process that exits runs no destructor of a value still alive and waits for no
    /// ~keep thread, so a kept engine left its Chrome running with parent 1, and a dropped one left
    /// ~keep the profile directory its teardown thread had not removed yet (xberg-io/crawlberg#594).
    /// ~keep The child prints the directory only after it has seen Chrome run on it.
    #[allow(
        clippy::print_stdout,
        reason = "the child reports its profile directory to its parent on stdout"
    )]
    async fn profile_of_exited_child(test_name: &str, engine_at_exit: &str) -> Option<std::path::PathBuf> {
        if let Some(mode) = std::env::var_os(EXIT_CHILD) {
            let site = one_page_site().await;
            let handle = browser_engine();
            scrape(&handle, &site.uri())
                .await
                .expect("the browser scrape must succeed");
            let profile = crate::browser_pool::tests::pool_profile_dir(&owned_pool(&handle)).await;
            println!("{EXIT_CHILD_PROFILE_LINE}{}", profile.display());
            if mode == "drop" {
                drop(handle);
            } else {
                if mode == "keep-with-the-list-locked" {
                    crate::browser_pool::tests::hold_the_live_profile_list_for_good();
                }
                std::mem::forget(handle);
            }
            std::process::exit(0);
        }
        if !chrome_launches(test_name).await {
            return None;
        }
        let test = std::thread::current()
            .name()
            .expect("libtest names each test's thread after the test")
            .to_owned();
        let child = std::env::current_exe().expect("the test binary must be readable");
        let mode = engine_at_exit.to_owned();
        let output = tokio::task::spawn_blocking(move || {
            let mut child = std::process::Command::new(child)
                .args([test.as_str(), "--exact", "--nocapture", "--test-threads=1"])
                .env(EXIT_CHILD, mode)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("the test binary must start");
            let deadline = std::time::Instant::now() + EXIT_CHILD_WAIT;
            while child.try_wait().expect("the child must be waitable").is_none() {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            child.wait_with_output().expect("the child's output must be readable")
        })
        .await
        .expect("the child run must not panic");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let profile = stdout
            .lines()
            .find_map(|line| Some(line.split_once(EXIT_CHILD_PROFILE_LINE)?.1))
            .map(std::path::PathBuf::from);
        let Some(profile) = profile.filter(|_| output.status.success()) else {
            panic!(
                "the child run of {test_name} must scrape, print its profile directory and exit with 0: {}\nstdout:\n{stdout}\nstderr:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        };
        Some(profile)
    }

    /// Assert that the child of [`profile_of_exited_child`] left no process on its Chrome's
    /// profile directory, and that the directory is gone.
    async fn assert_exit_leaves_no_chrome(test_name: &str, engine_at_exit: &str) {
        let Some(profile) = profile_of_exited_child(test_name, engine_at_exit).await else {
            return;
        };
        let users = crate::browser_pool::tests::chrome_process_count_for_profile(&profile);
        let exists = profile.exists();
        let left = crate::browser_pool::tests::what_is_left(&profile);
        crate::browser_pool::tests::kill_processes_naming_profile(&profile);
        let _ = std::fs::remove_dir_all(&profile);
        assert_eq!(
            users, 0,
            "the exited child left Chrome processes on its profile: {left}"
        );
        assert!(
            !exists,
            "the exited child left its profile directory {}: {left}",
            profile.display()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_process_that_exits_with_a_live_engine_leaves_no_chrome_and_no_profile_directory() {
        assert_exit_leaves_no_chrome(
            "a_process_that_exits_with_a_live_engine_leaves_no_chrome_and_no_profile_directory",
            "keep",
        )
        .await;
    }

    /// A process still exits when the list its exit hook reads is locked for good, as it is when
    /// a thread ended while it held the lock. The hook then leaves the Chrome.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_process_whose_profile_list_is_locked_still_exits() {
        let profile = profile_of_exited_child(
            "a_process_whose_profile_list_is_locked_still_exits",
            "keep-with-the-list-locked",
        )
        .await;
        if let Some(profile) = profile {
            crate::browser_pool::tests::kill_processes_naming_profile(&profile);
            let _ = std::fs::remove_dir_all(&profile);
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_process_that_exits_right_after_dropping_its_engine_leaves_no_profile_directory() {
        assert_exit_leaves_no_chrome(
            "a_process_that_exits_right_after_dropping_its_engine_leaves_no_profile_directory",
            "drop",
        )
        .await;
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
