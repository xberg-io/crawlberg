//! Headless Chrome/CDP browser fallback for fetching JavaScript-rendered pages.
//!
//! This module is only compiled when the `browser` feature is enabled.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::time::Duration;

use chromiumoxide::browser::Browser;
use chromiumoxide::cdp::browser_protocol::target::TargetId;
use tokio::sync::OwnedSemaphorePermit;
use tokio::task::JoinHandle;
use tokio_stream::StreamExt;
use tracing::Instrument as _;

use self::launch::launch_or_connect;
use self::navigation::page_fetch;
use crate::browser_pool::{BrowserPool, ExternalTabCleanup, kill_browser, release_browser};
use crate::error::CrawlError;
use crate::http::HttpResponse;
use crate::net::ssrf::validate_url;
use crate::ssrf_intercept::{BrowserFirewall, BrowserOrigin, PageContext, Watch};
use crate::telemetry::attributes::{CRAWL_BROWSER_BACKEND, CRAWL_BROWSER_SESSION_ID, CRAWL_PAGES_RENDERED};
use crate::telemetry::metrics::registry;
use crate::types::{BrowserBackend, CookieInfo, CrawlConfig};

mod launch;
mod navigation;

/// Process-wide monotonic session counter for `crawl.browser.session_id`.
static BROWSER_SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

/// A page a browser backend fetched, and the HTTP redirects it followed to reach it.
pub(crate) struct BrowserPage {
    pub(crate) response: HttpResponse,
    /// HTTP redirects the browser followed. The native backend does not report its chain,
    /// so for it a landing on another URL counts as one.
    pub(crate) redirects: usize,
    /// The URLs the SSRF policy refused for requests the page sent, credential-redacted.
    pub(crate) refused: Vec<String>,
}

/// Fetch a URL using a headless Chrome browser via CDP.
///
/// When `pool` is `Some`, acquires a page from the pool, uses it, and returns
/// it on completion. When `pool` is `None`, launches a one-shot browser
/// instance and tears it down afterwards.
///
/// Returns the rendered page, in the `HttpResponse` shape the scrape pipeline reads, and
/// the HTTP redirects the browser followed to reach it. The page's status is handled the way
/// HTTP mode handles it: a 404 or 500 page is the error the HTTP fetch returns.
pub(crate) async fn browser_fetch(
    url: &str,
    config: &CrawlConfig,
    prior_cookies: Option<&[CookieInfo]>,
    pool: Option<&BrowserPool>,
    want_screenshot: bool,
    #[cfg(feature = "browser-native")] native_executor: Option<&crawlberg_browser::adapter::NativeBrowserExecutor>,
) -> Result<BrowserPage, CrawlError> {
    let page = match config.browser.backend {
        BrowserBackend::Chromiumoxide => chromiumoxide_fetch(url, config, prior_cookies, pool, want_screenshot).await?,
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
            let (response, refused) = native_fetch(url, config, prior_cookies, native_executor).await?;
            #[cfg(not(feature = "browser-native"))]
            let (response, refused) = native_fetch(url, config, prior_cookies).await?;
            BrowserPage {
                redirects: usize::from(response.final_url != url),
                response,
                refused,
            }
        }
    };
    Ok(BrowserPage {
        response: crate::http::rendered_status_outcome(page.response, page.redirects > 0, config)?,
        redirects: page.redirects,
        refused: page.refused,
    })
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
/// Page acquisition, navigation and rendering are bounded as a single
/// `BrowserConfig::overall_timeout` deadline, closing the gap where an unbounded semaphore
/// wait could hold a fetch open indefinitely. The page release that follows runs on every
/// path including the deadline path, and is bounded by `BrowserConfig::shutdown_timeout`.
async fn pooled_fetch(
    url: &str,
    config: &CrawlConfig,
    prior_cookies: Option<&[CookieInfo]>,
    pool: &BrowserPool,
    want_screenshot: bool,
) -> Result<BrowserPage, CrawlError> {
    let overall_timeout = config.browser.overall_timeout;
    let deadline = tokio::time::Instant::now() + overall_timeout;

    crate::types::warn_ignored_launch_options(
        &config.browser,
        "a shared browser_pool is configured; the pool launches Chrome from its own BrowserPoolConfig",
    );
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

    let (page, permit) = match tokio::time::timeout_at(deadline, acquire_pooled_page(url, config, pool)).await {
        Ok(acquired) => acquired?,
        Err(_) => return Err(overall_deadline_error(overall_timeout)),
    };

    // ~keep The deadline is applied to each stage here rather than by wrapping this whole
    // ~keep function in `tokio::time::timeout` at the call site. Wrapping dropped this future
    // ~keep the instant the deadline expired, which skipped the `release_pooled_page` below;
    // ~keep `chromiumoxide::Page` has no closing `Drop`, so every pooled fetch that hit its
    // ~keep overall deadline left its CDP target open in the shared browser for the rest of the
    // ~keep process's life. xberg-io/crawlberg#179.
    let watched = match tokio::time::timeout_at(deadline, watch_pooled_page(pool, &page, config)).await {
        Ok(watched) => watched,
        Err(_) => Err(overall_deadline_error(overall_timeout)),
    };
    let watch = match watched {
        Ok(watch) => watch,
        Err(error) => {
            let _ = tokio::time::timeout(config.browser.shutdown_timeout, page.close()).await;
            drop(permit);
            return Err(error);
        }
    };
    let result = match tokio::time::timeout_at(
        deadline,
        page_fetch(url, config, &page, &watch, prior_cookies, want_screenshot),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(overall_deadline_error(overall_timeout)),
    };

    release_pooled_page(url, config, page, watch, permit, result.is_ok()).await;

    result
}

/// Take a page and its semaphore permit for this fetch, reusing a parked session-affinity
/// page when one exists for this URL.
///
/// ~keep The page and its permit are returned as a pair for the caller to hold across
/// ~keep `page_fetch`: letting a `PooledPage` guard drop as the tail expression of the
/// ~keep acquisition released the semaphore permit AND spawned a `Target.closeTarget` race
/// ~keep against the navigation that was about to start on the very same CDP target.
async fn acquire_pooled_page(
    url: &str,
    config: &CrawlConfig,
    pool: &BrowserPool,
) -> Result<(chromiumoxide::Page, Option<OwnedSemaphorePermit>), CrawlError> {
    if config.browser.session_affinity {
        let session_key = crate::browser_session_pool::SessionKey::from_url(
            url,
            config.browser.proxy.as_ref().map(|p| p.url.as_str()),
        )?;
        let session_pool = config
            .browser_session_pool
            .as_deref()
            .ok_or_else(|| CrawlError::browser_error("session_affinity enabled but session pool is not configured"))?;

        if let Some(reused) = session_pool.acquire(&session_key).await {
            return Ok(reused);
        }
    }

    Ok(pool.acquire_page().await?.into_parts())
}

/// Put a pooled page under its browser's SSRF check.
async fn watch_pooled_page(
    pool: &BrowserPool,
    page: &chromiumoxide::Page,
    config: &CrawlConfig,
) -> Result<Watch, CrawlError> {
    pool.firewall().await?.watch(page, config, config.max_redirects).await
}

/// Park `page` for reuse when session affinity wants it and the fetch succeeded, otherwise
/// close its CDP target and release the permit. Either way its watch ends: parking closes the
/// popups it opened, closing closes them and the page.
///
/// ~keep This runs on the overall-deadline path too, which is the whole reason `pooled_fetch`
/// ~keep bounds its stages individually, so the close here must itself be bounded: an
/// ~keep unbounded close against a browser already wedged enough to blow the overall deadline
/// ~keep would reintroduce exactly the hang that deadline exists to cut short.
async fn release_pooled_page(
    url: &str,
    config: &CrawlConfig,
    page: chromiumoxide::Page,
    watch: Watch,
    permit: Option<OwnedSemaphorePermit>,
    reusable: bool,
) {
    if config.browser.session_affinity
        && reusable
        && let Ok(session_key) = crate::browser_session_pool::SessionKey::from_url(
            url,
            config.browser.proxy.as_ref().map(|p| p.url.as_str()),
        )
        && let Some(session_pool) = config.browser_session_pool.as_deref()
    {
        tracing::debug!("parking a pooled browser page for session reuse");
        watch.park().await;
        session_pool.insert(session_key, page, permit).await;
        return;
    }

    let shutdown_timeout = config.browser.shutdown_timeout;
    tracing::debug!(reusable, "releasing a pooled browser page");
    drop(page);
    if tokio::time::timeout(shutdown_timeout, watch.close()).await.is_err() {
        tracing::warn!(
            timeout_secs = shutdown_timeout.as_secs_f64(),
            "a pooled page did not close before the shutdown timeout; its CDP target is left to Chrome"
        );
    }
    drop(permit);
}

/// Launch (or connect to) a browser for this single fetch and tear it down again.
///
/// `BrowserConfig::overall_timeout` bounds launch, page creation, navigation,
/// rendering, and screenshot capture as a single deadline. Shutdown runs
/// afterward in the background: a completed page result is returned to the
/// caller without waiting for a Chrome process that refuses to exit. A browser
/// closed normally gets `BrowserConfig::shutdown_timeout` to exit before it is
/// killed. A browser launched with a throwaway profile is killed at once, and
/// the wait for its processes to go gets what is left of that timeout. Ending
/// those processes and removing the profile are not bounded by it.
///
/// Teardown is owned by [`OneShotSession`]'s `Drop`, so a caller that drops this future while
/// the fetch runs gets the same teardown as a fetch that ran to completion.
async fn one_shot_fetch(
    url: &str,
    config: &CrawlConfig,
    prior_cookies: Option<&[CookieInfo]>,
    want_screenshot: bool,
) -> Result<BrowserPage, CrawlError> {
    let overall_timeout = config.browser.overall_timeout;
    let deadline = tokio::time::Instant::now() + overall_timeout;

    let (browser, mut handler, data_dir) = match tokio::time::timeout_at(deadline, launch_or_connect(config)).await {
        Ok(Ok(launched)) => launched,
        Ok(Err(error)) => return Err(error),
        Err(_) => return Err(overall_deadline_error(overall_timeout)),
    };

    let handler_handle = tokio::spawn(async move { while handler.next().await.is_some() {} });
    let origin = BrowserOrigin::of_session(config.browser.endpoint.as_deref(), data_dir.is_some());
    let mut session = OneShotSession {
        browser: Some(Arc::new(browser)),
        firewall: None,
        open_tab: None,
        handler_handle: Some(handler_handle),
        data_dir,
        shutdown_timeout: config.browser.shutdown_timeout,
        origin,
    };

    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    let fetch_outcome = tokio::time::timeout(remaining, async {
        let (page, watch) = session.open_watched_page(config).await?;
        let result = page_fetch(url, config, &page, &watch, prior_cookies, want_screenshot).await;
        watch.close().await;
        result
    })
    .await;

    // ~keep `session` is dropped as this function returns, after the result below is computed,
    // ~keep and its `Drop` spawns the teardown rather than awaiting it: a Chrome process stuck
    // ~keep behind a blocking OS dialog (the originally reported case: a macOS keychain prompt)
    // ~keep must not hold up delivery of a result that was already computed. `release_browser`
    // ~keep gives Chrome `shutdown_timeout` to close and then force-kills a launched one.
    // ~keep `kill_browser` gives its wait for the Chrome family what is left of that timeout;
    // ~keep collecting and killing the family, reaping the main process and removing the profile
    // ~keep end on their own but are not bounded by it (a reap of 6.6 s and a removal of 11.9 s
    // ~keep at loads of 720 to 1064). So that background task always finishes, later than
    // ~keep `shutdown_timeout` on a busy host.
    fetch_outcome.unwrap_or_else(|_| Err(overall_deadline_error(overall_timeout)))
}

/// Everything one [`one_shot_fetch`] has to tear down, owned by a single value whose `Drop`
/// runs that teardown.
///
/// ~keep Teardown used to be straight-line code after the fetch, reached only once the fetch
/// ~keep had finished, so a caller that dropped the future while it ran got none of it: the CDP
/// ~keep connection to a `browser.endpoint` Chrome stayed open with the tab crawlberg had
/// ~keep opened, and a launched Chrome's `--user-data-dir` was left on disk
/// ~keep (xberg-io/crawlberg#131). Owning it in a value makes cancellation and completion the
/// ~keep same path by construction, instead of two blocks that have to be kept in step by hand.
struct OneShotSession {
    browser: Option<Arc<Browser>>,
    /// The SSRF check of the browser. It holds the other reference to `browser` until it is
    /// stopped.
    firewall: Option<BrowserFirewall>,
    /// The tab this fetch opened, recorded as soon as it exists so teardown can close it in a
    /// caller's Chrome even when the fetch never reaches its own cleanup.
    open_tab: Option<TargetId>,
    handler_handle: Option<JoinHandle<()>>,
    data_dir: Option<std::path::PathBuf>,
    shutdown_timeout: Duration,
    origin: BrowserOrigin,
}

impl OneShotSession {
    /// Start the browser's SSRF check, open this fetch's tab, recording it for teardown, and
    /// hand back a handle to it with its watch.
    async fn open_watched_page(&mut self, config: &CrawlConfig) -> Result<(chromiumoxide::Page, Watch), CrawlError> {
        let browser = self.browser.as_ref().expect("browser is taken only by Drop");
        let firewall = BrowserFirewall::start(Arc::clone(browser), self.origin, PageContext::of(config)).await?;
        let firewall = self.firewall.insert(firewall);
        let page = firewall.handle().new_page().await?;
        self.open_tab = Some(page.target_id().clone());
        let watch = firewall.handle().watch(&page, config, config.max_redirects).await?;
        Ok((page, watch))
    }
}

impl Drop for OneShotSession {
    // ~keep `tokio::spawn` panics when no runtime is active on the current thread, and these
    // ~keep futures cross an FFI boundary into host GC/finalizer threads, so an unguarded spawn
    // ~keep here turns a late drop into a panic that aborts the embedding process -- the same
    // ~keep reasoning as `PooledPage`'s Drop in browser_pool.rs.
    fn drop(&mut self) {
        let (Some(browser), Some(handler_handle)) = (self.browser.take(), self.handler_handle.take()) else {
            return;
        };
        let cleanup = ExternalTabCleanup {
            open_tab: self.open_tab.take(),
            ..ExternalTabCleanup::default()
        };
        let firewall = self.firewall.take();
        let data_dir = self.data_dir.take();
        let shutdown_timeout = self.shutdown_timeout;
        let origin = self.origin;

        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    if let Some(firewall) = firewall {
                        firewall.stop().await;
                    }
                    // ~keep The stopped firewall held the only other reference, so this is the
                    // ~keep browser itself. One launched with a throwaway profile is killed with
                    // ~keep interception still on, as `interact` does (xberg-io/crawlberg#468).
                    match (Arc::into_inner(browser), data_dir) {
                        (Some(browser), Some(profile)) if origin == BrowserOrigin::Killed => {
                            kill_browser(browser, handler_handle, profile, shutdown_timeout).await;
                        }
                        (browser, profile) => {
                            match browser {
                                Some(browser) => {
                                    release_browser(browser, handler_handle, cleanup, shutdown_timeout).await
                                }
                                None => handler_handle.abort(),
                            }
                            if let Some(dir) = profile {
                                let _ = tokio::fs::remove_dir_all(&dir).await;
                            }
                        }
                    }
                });
            }
            Err(_) => {
                tracing::warn!(
                    "dropping a one-shot browser session outside a Tokio runtime; its Chrome \
                     teardown is left to the process"
                );
            }
        }
    }
}

#[cfg(feature = "browser-native")]
async fn native_fetch(
    url: &str,
    config: &CrawlConfig,
    prior_cookies: Option<&[CookieInfo]>,
    native_executor: Option<&crawlberg_browser::adapter::NativeBrowserExecutor>,
) -> Result<(HttpResponse, Vec<String>), CrawlError> {
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
) -> Result<(HttpResponse, Vec<String>), CrawlError> {
    Err(CrawlError::invalid_config(
        "browser.backend = native requires the browser-native feature",
    ))
}
