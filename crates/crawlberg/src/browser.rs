//! Headless Chrome/CDP browser fallback for fetching JavaScript-rendered pages.
//!
//! This module is only compiled when the `browser` feature is enabled.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::time::Duration;

use tokio_stream::StreamExt;
use tracing::Instrument as _;

use self::launch::launch_or_connect;
use self::navigation::page_fetch;
use crate::browser_pool::{BrowserPool, close_browser_within};
use crate::error::CrawlError;
use crate::http::HttpResponse;
use crate::net::ssrf::validate_url;
use crate::ssrf_intercept::BrowserFirewall;
use crate::telemetry::attributes::{CRAWL_BROWSER_BACKEND, CRAWL_BROWSER_SESSION_ID, CRAWL_PAGES_RENDERED};
use crate::telemetry::metrics::registry;
use crate::types::{BrowserBackend, CookieInfo, CrawlConfig};

mod launch;
mod navigation;

/// Process-wide monotonic session counter for `crawl.browser.session_id`.
static BROWSER_SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

/// How long to wait for the CDP handler task to wind down after a browser is
/// closed, before abandoning it.
const HANDLER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// A page a browser backend fetched, and the HTTP redirects it followed to reach it.
pub(crate) struct BrowserPage {
    pub(crate) response: HttpResponse,
    /// HTTP redirects the browser followed. The native backend does not report its chain,
    /// so for it a landing on another URL counts as one.
    pub(crate) redirects: usize,
}

/// Fetch a URL using a headless Chrome browser via CDP.
///
/// When `pool` is `Some`, acquires a page from the pool, uses it, and returns
/// it on completion. When `pool` is `None`, launches a one-shot browser
/// instance and tears it down afterwards.
///
/// Returns the rendered page, in the `HttpResponse` shape the scrape pipeline reads, and
/// the HTTP redirects the browser followed to reach it.
pub(crate) async fn browser_fetch(
    url: &str,
    config: &CrawlConfig,
    prior_cookies: Option<&[CookieInfo]>,
    pool: Option<&BrowserPool>,
    want_screenshot: bool,
    #[cfg(feature = "browser-native")] native_executor: Option<&crawlberg_browser::adapter::NativeBrowserExecutor>,
) -> Result<BrowserPage, CrawlError> {
    match config.browser.backend {
        BrowserBackend::Chromiumoxide => chromiumoxide_fetch(url, config, prior_cookies, pool, want_screenshot).await,
        BrowserBackend::Native => {
            // ~keep Screenshot capture is implemented only for the chromiumoxide fetch path
            // ~keep (`page_fetch`, in `browser/navigation.rs`); the native backend lives in the
            // ~keep off-limits `crawlberg-browser` crate. Warn instead of silently dropping the
            // ~keep request, matching the rest of `capture_screenshot`'s contract.
            if config.capture_screenshot {
                tracing::warn!(
                    "capture_screenshot is not supported by BrowserBackend::Native; \
                     no screenshot will be captured for this fetch"
                );
            }
            #[cfg(feature = "browser-native")]
            let response = native_fetch(url, config, prior_cookies, native_executor).await?;
            #[cfg(not(feature = "browser-native"))]
            let response = native_fetch(url, config, prior_cookies).await?;
            Ok(BrowserPage {
                redirects: usize::from(response.final_url != url),
                response,
            })
        }
    }
}

async fn chromiumoxide_fetch(
    url: &str,
    config: &CrawlConfig,
    prior_cookies: Option<&[CookieInfo]>,
    pool: Option<&BrowserPool>,
    want_screenshot: bool,
) -> Result<BrowserPage, CrawlError> {
    let session_id = BROWSER_SESSION_COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
    let session_id_str = session_id.to_string();

    let span = tracing::info_span!(
        "crawl.browser.session",
        { CRAWL_BROWSER_BACKEND } = "chromiumoxide",
        { CRAWL_BROWSER_SESSION_ID } = %session_id_str,
        { CRAWL_PAGES_RENDERED } = 1_i64,
    );

    registry().browser_sessions_active.add(1, &[]);
    struct SessionGuard;
    impl Drop for SessionGuard {
        fn drop(&mut self) {
            registry().browser_sessions_active.add(-1, &[]);
        }
    }
    let _guard = SessionGuard;

    chromiumoxide_fetch_inner(url, config, prior_cookies, pool, want_screenshot)
        .instrument(span)
        .await
}

async fn chromiumoxide_fetch_inner(
    url: &str,
    config: &CrawlConfig,
    prior_cookies: Option<&[CookieInfo]>,
    pool: Option<&BrowserPool>,
    want_screenshot: bool,
) -> Result<BrowserPage, CrawlError> {
    let target = url::Url::parse(url).map_err(|e| CrawlError::ssrf_violation(url, format!("invalid URL: {e}")))?;
    validate_url(&target, &config.ssrf)
        .await
        .map_err(|e| CrawlError::ssrf_violation(url, e.to_string()))?;

    match pool {
        Some(pool) => pooled_fetch(url, config, prior_cookies, pool, want_screenshot).await,
        None => one_shot_fetch(url, config, prior_cookies, want_screenshot).await,
    }
}

/// Build the error returned when a browser fetch does not complete within
/// `BrowserConfig::overall_timeout`.
fn overall_deadline_error(overall_timeout: Duration) -> CrawlError {
    CrawlError::browser_timeout(format!(
        "browser fetch exceeded the overall deadline of {overall_timeout:?}"
    ))
}

/// Fetch using a page borrowed from a shared [`BrowserPool`], returning the page
/// to the session pool on success when session affinity is enabled.
///
/// The whole operation -- page acquisition, navigation, rendering, and the
/// page close that follows -- is bounded by `BrowserConfig::overall_timeout`,
/// closing the gap where an unbounded semaphore wait or an unbounded
/// `page.close()` could hold a fetch open indefinitely.
async fn pooled_fetch(
    url: &str,
    config: &CrawlConfig,
    prior_cookies: Option<&[CookieInfo]>,
    pool: &BrowserPool,
    want_screenshot: bool,
) -> Result<BrowserPage, CrawlError> {
    let overall_timeout = config.browser.overall_timeout;
    match tokio::time::timeout(
        overall_timeout,
        pooled_fetch_inner(url, config, prior_cookies, pool, want_screenshot),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(overall_deadline_error(overall_timeout)),
    }
}

async fn pooled_fetch_inner(
    url: &str,
    config: &CrawlConfig,
    prior_cookies: Option<&[CookieInfo]>,
    pool: &BrowserPool,
    want_screenshot: bool,
) -> Result<BrowserPage, CrawlError> {
    if config.browser_profile.is_some() {
        // ~keep Pool browsers launch once, ahead of any per-crawl CrawlConfig; a
        // ~keep profile named later cannot retroactively change that process's
        // ~keep --user-data-dir, so surface it instead of a silent no-op.
        tracing::warn!(
            profile = config.browser_profile.as_deref().unwrap_or_default(),
            "browser_profile is ignored when a shared browser_pool is configured; \
             profiles only apply to per-crawl (non-pooled) browser launches"
        );
    }

    // ~keep `page` + `permit` are held across `page_fetch` below: the previous code let the
    // ~keep acquisition guard drop as the tail expression of this block, which released the
    // ~keep semaphore permit AND spawned a `Target.closeTarget` race against the navigation
    // ~keep that was about to start on the very same CDP target.
    let (page, permit) = if config.browser.session_affinity {
        let session_key = crate::browser_session_pool::SessionKey::from_url(
            url,
            config.browser.proxy.as_ref().map(|p| p.url.as_str()),
        )?;
        let session_pool = config
            .browser_session_pool
            .as_deref()
            .ok_or_else(|| CrawlError::browser_error("session_affinity enabled but session pool is not configured"))?;

        if let Some(reused) = session_pool.acquire(&session_key).await {
            reused
        } else {
            let pooled = pool.acquire_page().await?;
            pooled.into_parts()
        }
    } else {
        let pooled = pool.acquire_page().await?;
        pooled.into_parts()
    };

    let watch = match pool.firewall().await {
        Ok(firewall) => firewall.watch(&page, &config.ssrf, config.max_redirects).await,
        Err(error) => Err(error),
    };
    let watch = match watch {
        Ok(watch) => watch,
        Err(error) => {
            let _ = page.close().await;
            return Err(error);
        }
    };
    let result = page_fetch(url, config, &page, &watch, prior_cookies, want_screenshot).await;

    if config.browser.session_affinity
        && result.is_ok()
        && let Ok(session_key) = crate::browser_session_pool::SessionKey::from_url(
            url,
            config.browser.proxy.as_ref().map(|p| p.url.as_str()),
        )
        && let Some(session_pool) = config.browser_session_pool.as_deref()
    {
        watch.park().await;
        session_pool.insert(session_key, page, permit).await;
    } else {
        watch.close().await;
        drop(permit);
    }

    result
}

/// Launch (or connect to) a browser for this single fetch and tear it down again.
///
/// `BrowserConfig::overall_timeout` bounds launch, page creation, navigation,
/// rendering, and screenshot capture as a single deadline. Shutdown (closing
/// the browser and waiting for its process to exit) runs afterward in the
/// background, bounded by its own `BrowserConfig::shutdown_timeout`: a
/// completed page result is returned to the caller without waiting for a
/// Chrome process that refuses to exit.
async fn one_shot_fetch(
    url: &str,
    config: &CrawlConfig,
    prior_cookies: Option<&[CookieInfo]>,
    want_screenshot: bool,
) -> Result<BrowserPage, CrawlError> {
    let overall_timeout = config.browser.overall_timeout;
    let shutdown_timeout = config.browser.shutdown_timeout;
    let deadline = tokio::time::Instant::now() + overall_timeout;

    let (browser, mut handler, data_dir) = match tokio::time::timeout_at(deadline, launch_or_connect(config)).await {
        Ok(Ok(launched)) => launched,
        Ok(Err(error)) => return Err(error),
        Err(_) => return Err(overall_deadline_error(overall_timeout)),
    };

    let handler_handle = tokio::spawn(async move { while handler.next().await.is_some() {} });

    let browser = Arc::new(browser);
    let firewall = BrowserFirewall::start(Arc::clone(&browser)).await;
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    let fetch_outcome = tokio::time::timeout(remaining, async {
        let firewall = firewall.as_ref().map_err(Clone::clone)?;
        let page = browser
            .new_page("about:blank")
            .await
            .map_err(|e| CrawlError::browser_error(format!("failed to create page: {e}")))?;
        let watch = match firewall.handle().watch(&page, &config.ssrf, config.max_redirects).await {
            Ok(watch) => watch,
            Err(error) => {
                let _ = page.close().await;
                return Err(error);
            }
        };

        let result = page_fetch(url, config, &page, &watch, prior_cookies, want_screenshot).await;
        watch.close().await;
        result
    })
    .await;

    let result = fetch_outcome.unwrap_or_else(|_| Err(overall_deadline_error(overall_timeout)));
    if let Ok(firewall) = firewall {
        firewall.stop().await;
    }
    // ~keep The stopped firewall held the only other reference, so this is the browser itself.
    let browser = Arc::into_inner(browser);

    // ~keep Shutdown is spawned rather than awaited inline: a Chrome process stuck behind a
    // ~keep blocking OS dialog (the originally reported case: a macOS keychain prompt) must
    // ~keep not hold up delivery of a result that was already computed above.
    // ~keep `close_browser_within` still bounds close()/wait() by `shutdown_timeout` and
    // ~keep force-kills the process on expiry, so this background task always finishes.
    tokio::spawn(async move {
        if let Some(mut browser) = browser {
            close_browser_within(&mut browser, shutdown_timeout).await;
        }
        let _ = tokio::time::timeout(HANDLER_SHUTDOWN_TIMEOUT, handler_handle).await;
        if let Some(dir) = data_dir {
            let _ = tokio::fs::remove_dir_all(&dir).await;
        }
    });

    result
}

#[cfg(feature = "browser-native")]
async fn native_fetch(
    url: &str,
    config: &CrawlConfig,
    prior_cookies: Option<&[CookieInfo]>,
    native_executor: Option<&crawlberg_browser::adapter::NativeBrowserExecutor>,
) -> Result<HttpResponse, CrawlError> {
    let native_executor = native_executor.ok_or_else(|| {
        CrawlError::browser_error("native browser executor is not available for BrowserBackend::Native")
    })?;
    crate::native_browser::native_browser_fetch(url, config, prior_cookies, native_executor).await
}

#[cfg(not(feature = "browser-native"))]
async fn native_fetch(
    _url: &str,
    _config: &CrawlConfig,
    _prior_cookies: Option<&[CookieInfo]>,
) -> Result<HttpResponse, CrawlError> {
    Err(CrawlError::invalid_config(
        "browser.backend = native requires the browser-native feature",
    ))
}
