//! Headless Chrome/CDP browser fallback for fetching JavaScript-rendered pages.
//!
//! This module is only compiled when the `browser` feature is enabled.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::time::Duration;

use chromiumoxide::browser::Browser;
use chromiumoxide::cdp::browser_protocol::target::TargetId;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;
use tracing::Instrument as _;

use self::launch::{Launched, UserDataDir, launch_or_connect};
use self::navigation::page_fetch;
use crate::browser_pool::{
    BrowserPool, ExternalTabCleanup, HandlerEnd, kill_browser, release_browser, spawn_watched_handler,
};
use crate::error::CrawlError;
use crate::http::HttpResponse;
use crate::net::egress::Egress;
use crate::net::ssrf::validate_url;
use crate::ssrf_intercept::{BrowserFirewall, BrowserOrigin, PageContext, Watch};
use crate::telemetry::attributes::{CRAWL_BROWSER_BACKEND, CRAWL_BROWSER_SESSION_ID, CRAWL_PAGES_RENDERED};
use crate::telemetry::metrics::registry;
#[cfg(feature = "browser-native")]
use crate::types::CookieInfo;
use crate::types::{BrowserBackend, BrowserCookie, CrawlConfig};

mod cookies;
mod launch;
mod navigation;

/// ~keep A cancelled caller leaves its supervised launch running to completion. This global cap
/// ~keep deliberately applies backpressure across profile-lock waits, launches and endpoint
/// ~keep connects so repeated cancellations cannot accumulate unbounded detached work.
const MAX_SUPERVISED_LAUNCHES: usize = 4;
static ONE_SHOT_LAUNCH_GATE: std::sync::LazyLock<Arc<Semaphore>> =
    std::sync::LazyLock::new(|| Arc::new(Semaphore::new(MAX_SUPERVISED_LAUNCHES)));

#[cfg(test)]
struct CompletedLaunchHook {
    marker: String,
    completed: tokio::sync::oneshot::Sender<Result<std::path::PathBuf, String>>,
    release: tokio::sync::oneshot::Receiver<()>,
}

#[cfg(test)]
static COMPLETED_LAUNCH_HOOK: std::sync::LazyLock<std::sync::Mutex<Option<CompletedLaunchHook>>> =
    std::sync::LazyLock::new(Default::default);

#[cfg(test)]
struct PendingClaimHook {
    marker: String,
    release: tokio::sync::oneshot::Receiver<()>,
}

#[cfg(test)]
static PENDING_CLAIM_HOOK: std::sync::LazyLock<std::sync::Mutex<Option<PendingClaimHook>>> =
    std::sync::LazyLock::new(Default::default);

#[cfg(test)]
type CompletedLaunchNotification = (
    tokio::sync::oneshot::Sender<Result<std::path::PathBuf, String>>,
    Result<std::path::PathBuf, String>,
);

#[cfg(test)]
fn prepare_completed_launch_notification(
    config: &CrawlConfig,
    launched: &Result<Launched, CrawlError>,
) -> Option<CompletedLaunchNotification> {
    let hook = {
        let mut slot = COMPLETED_LAUNCH_HOOK.lock().unwrap_or_else(|error| error.into_inner());
        match slot.as_ref() {
            Some(hook) if config.browser.chrome_args.iter().any(|arg| arg == &hook.marker) => slot.take(),
            _ => None,
        }
    };
    let hook = hook?;
    let completed = match launched {
        Ok((_, _, Some(UserDataDir::Scratch(profile)), _, _)) => Ok(profile.path().to_path_buf()),
        Ok(_) => Err("the supervised test launch did not create a scratch profile".to_owned()),
        Err(error) => Err(error.to_string()),
    };
    let CompletedLaunchHook {
        marker,
        completed: completed_tx,
        release,
    } = hook;
    *PENDING_CLAIM_HOOK.lock().unwrap_or_else(|error| error.into_inner()) = Some(PendingClaimHook { marker, release });
    Some((completed_tx, completed))
}

#[cfg(test)]
async fn hold_before_claim(config: &CrawlConfig) {
    let hook = {
        let mut slot = PENDING_CLAIM_HOOK.lock().unwrap_or_else(|error| error.into_inner());
        match slot.as_ref() {
            Some(hook) if config.browser.chrome_args.iter().any(|arg| arg == &hook.marker) => slot.take(),
            _ => None,
        }
    };
    if let Some(hook) = hook {
        let _ = hook.release.await;
    }
}

/// Process-wide monotonic session counter for `crawl.browser.session_id`.
static BROWSER_SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

/// A page a browser backend fetched, and the redirects it followed to reach it.
pub(crate) struct BrowserPage {
    pub(crate) response: HttpResponse,
    /// Redirects the browser followed within `max_redirects`: HTTP redirects, and the
    /// navigations the page started (a meta refresh or a script), one each.
    pub(crate) redirects: usize,
    /// Whether the navigation that received the response followed an HTTP redirect to it. For a
    /// page that navigated itself after the load, that is the late navigation, not the seed.
    pub(crate) redirected: bool,
    /// The URLs the SSRF policy refused for requests the page sent, credential-redacted.
    pub(crate) refused: Vec<String>,
    /// Cookies in Chromium's jar after this page finished rendering. ~keep
    pub(crate) cookies: Vec<BrowserCookie>,
    /// Whether the body of `response` is text the backend decoded, or the bytes of a response
    /// the navigation stopped on.
    pub(crate) text: crate::tower::BodyText,
}

/// Fetch a URL using a headless Chrome browser via CDP.
///
/// When `pool` is `Some`, acquires a page from the pool, uses it, and returns
/// it on completion. When `pool` is `None`, launches a one-shot browser
/// instance and tears it down afterwards.
///
/// Returns the rendered page, in the `HttpResponse` shape the scrape pipeline reads, and
/// the redirects the browser followed to reach it. The page's status is handled the way
/// HTTP mode handles it: a 404 or 500 page is the error the HTTP fetch returns.
pub(crate) async fn browser_fetch(
    url: &str,
    config: &CrawlConfig,
    prior_cookies: Option<&[BrowserCookie]>,
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
            let (response, refused, redirects, charset) =
                native_fetch(url, config, prior_cookies, native_executor).await?;
            #[cfg(not(feature = "browser-native"))]
            let (response, refused, redirects, charset) = native_fetch(url, config, prior_cookies).await?;
            BrowserPage {
                redirected: response.final_url != url,
                response,
                refused,
                redirects,
                cookies: Vec::new(),
                // ~keep The native backend decoded the document, with the character set it reports.
                text: crate::tower::BodyText::Decoded { charset },
            }
        }
    };
    Ok(BrowserPage {
        response: crate::http::rendered_status_outcome(page.response, page.redirected, config)?,
        redirects: page.redirects,
        redirected: page.redirected,
        refused: page.refused,
        cookies: page.cookies,
        text: page.text,
    })
}

async fn chromiumoxide_fetch(
    url: &str,
    config: &CrawlConfig,
    prior_cookies: Option<&[BrowserCookie]>,
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
    prior_cookies: Option<&[BrowserCookie]>,
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
    prior_cookies: Option<&[BrowserCookie]>,
    pool: &BrowserPool,
    want_screenshot: bool,
) -> Result<BrowserPage, CrawlError> {
    let overall_timeout = config.browser.overall_timeout;
    let deadline = tokio::time::Instant::now() + overall_timeout;

    if !pool.uses_launch_options(&config.browser) {
        let reason = if config.browser.endpoint.is_some() {
            "connecting to an external browser.endpoint, whose Chrome process is launched externally"
        } else {
            "a shared browser_pool is configured; the pool launches Chrome from its own BrowserPoolConfig"
        };
        crate::types::warn_ignored_launch_options(&config.browser, reason);
    }
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

    let (page, permit, acquired_watch) =
        match tokio::time::timeout_at(deadline, acquire_pooled_page(url, config, pool)).await {
            Ok(acquired) => acquired?,
            Err(_) => return Err(overall_deadline_error(overall_timeout)),
        };

    // ~keep The deadline is applied to each stage here rather than by wrapping this whole
    // ~keep function in `tokio::time::timeout` at the call site. Wrapping dropped this future
    // ~keep the instant the deadline expired, which skipped the `release_pooled_page` below;
    // ~keep `chromiumoxide::Page` has no closing `Drop`, so every pooled fetch that hit its
    // ~keep overall deadline left its CDP target open in the shared browser for the rest of the
    // ~keep process's life. xberg-io/crawlberg#179.
    let watched = match acquired_watch {
        Some(watch) => Ok(watch),
        None => match tokio::time::timeout_at(deadline, watch_pooled_page(pool, &page, config)).await {
            Ok(watched) => watched,
            Err(_) => Err(overall_deadline_error(overall_timeout)),
        },
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

    let reusable = result.is_ok() && watch.page_reusable();
    release_pooled_page(url, config, page, watch, permit, reusable).await;

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
) -> Result<(chromiumoxide::Page, Option<OwnedSemaphorePermit>, Option<Watch>), CrawlError> {
    let proxy = crate::proxy::chrome_proxy_for(config)?;
    if config.browser.session_affinity {
        let session_key = session_key(url, proxy.as_ref())?;
        let session_pool = config
            .browser_session_pool
            .as_deref()
            .ok_or_else(|| CrawlError::browser_error("session_affinity enabled but session pool is not configured"))?;

        if let Some(reused) = session_pool.acquire_with_policy(&session_key, &config.ssrf).await {
            return Ok((reused.0, reused.1, None));
        }
    }

    let (page, watch, permit) = pool.acquire_page_through(proxy.as_ref(), config).await?.into_parts();
    Ok((page, permit, Some(watch)))
}

/// The session-affinity key of a page for `url` opened through `proxy`.
///
/// ~keep Keyed on the proxy the page's browser context uses, so a parked page is never handed
/// ~keep to a crawl with a different proxy.
fn session_key(
    url: &str,
    proxy: Option<&crate::proxy::ChromeProxy>,
) -> Result<crate::browser_session_pool::SessionKey, CrawlError> {
    crate::browser_session_pool::SessionKey::from_url(url, proxy.map(|p| p.server.as_str()))
}

/// Put a pooled page under its browser's SSRF check.
async fn watch_pooled_page(
    pool: &BrowserPool,
    page: &chromiumoxide::Page,
    config: &CrawlConfig,
) -> Result<Watch, CrawlError> {
    pool.firewall().await?.watch(page, config, config.max_redirects).await
}

/// Park `page` for reuse when session affinity wants it and the page is `reusable` (the fetch
/// succeeded and a new navigation can start on the page), otherwise close its CDP target and
/// release the permit. Either way its watch ends: parking closes the popups it opened, closing
/// closes them and the page.
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
        && let Ok(proxy) = crate::proxy::chrome_proxy_for(config)
        && let Ok(session_key) = session_key(url, proxy.as_ref())
        && let Some(session_pool) = config.browser_session_pool.as_deref()
    {
        tracing::debug!("parking a pooled browser page for session reuse");
        watch.park().await;
        session_pool
            .insert_with_policy(session_key, &config.ssrf, page, permit)
            .await;
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

struct UnclaimedLaunch {
    launched: Option<Launched>,
    shutdown_timeout: Duration,
}

impl UnclaimedLaunch {
    fn new(launched: Launched, shutdown_timeout: Duration) -> Self {
        Self {
            launched: Some(launched),
            shutdown_timeout,
        }
    }

    fn claim(mut self) -> Launched {
        self.launched.take().expect("an unclaimed launch is taken only once")
    }
}

impl Drop for UnclaimedLaunch {
    fn drop(&mut self) {
        let Some(launched) = self.launched.take() else {
            return;
        };
        let shutdown_timeout = self.shutdown_timeout;
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => drop(handle.spawn(teardown_unclaimed_launch(launched, shutdown_timeout))),
            Err(_) => tracing::warn!(
                "an unclaimed one-shot browser launch dropped outside its Tokio runtime; runtime shutdown cleanup is unchanged"
            ),
        }
    }
}

/// ~keep Supervise ordinary future cancellation while the Tokio runtime remains active. Runtime
/// ~keep shutdown still cancels runtime-owned tasks and is outside this ownership boundary.
async fn launch_one_shot(config: &CrawlConfig) -> Result<Launched, CrawlError> {
    let launch_permit = Arc::clone(&ONE_SHOT_LAUNCH_GATE)
        .acquire_owned()
        .await
        .map_err(|error| CrawlError::browser_error(format!("browser launch capacity closed: {error}")))?;
    let config = config.clone();
    #[cfg(test)]
    let claim_config = config.clone();
    let shutdown_timeout = config.browser.shutdown_timeout;
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let launched = launch_or_connect(&config).await;
        #[cfg(test)]
        let completed_notification = prepare_completed_launch_notification(&config, &launched);
        let guarded = launched.map(|launched| UnclaimedLaunch::new(launched, shutdown_timeout));
        let send_succeeded = result_tx.send(guarded).is_ok();
        #[cfg(test)]
        if send_succeeded && let Some((completed_tx, completed)) = completed_notification {
            let _ = completed_tx.send(completed);
        }
        #[cfg(not(test))]
        let _ = send_succeeded;
        drop(launch_permit);
    });
    let received = result_rx
        .await
        .map_err(|error| CrawlError::browser_error(format!("browser launch task ended without a result: {error}")))?;
    #[cfg(test)]
    hold_before_claim(&claim_config).await;
    let guarded = received?;
    Ok(guarded.claim())
}

async fn teardown_unclaimed_launch(
    (browser, handler, data_dir, egress, profile_hold): Launched,
    shutdown_timeout: Duration,
) {
    let handler_handle = spawn_watched_handler(handler).0;
    match data_dir {
        Some(UserDataDir::Scratch(profile)) => {
            let path = profile.path().to_path_buf();
            kill_browser(browser, handler_handle, path, shutdown_timeout).await;
            drop(profile);
        }
        profile => {
            release_browser(browser, handler_handle, ExternalTabCleanup::default(), shutdown_timeout).await;
            drop(profile);
        }
    }
    drop(egress);
    drop(profile_hold);
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
    prior_cookies: Option<&[BrowserCookie]>,
    want_screenshot: bool,
) -> Result<BrowserPage, CrawlError> {
    let overall_timeout = config.browser.overall_timeout;
    let deadline = tokio::time::Instant::now() + overall_timeout;
    let _ = crate::net::egress::socket_policy(
        &config.ssrf,
        config.browser.endpoint.as_deref(),
        &std::sync::Once::new(),
    )?;

    let launched = match tokio::time::timeout_at(deadline, launch_one_shot(config)).await {
        Ok(Ok(launched)) => launched,
        Ok(Err(error)) => return Err(error),
        Err(_) => return Err(overall_deadline_error(overall_timeout)),
    };
    fetch_launched(launched, deadline, url, config, prior_cookies, want_screenshot).await
}

/// Fetch `url` in a browser [`launch_or_connect`] returned, before `deadline`, then tear the
/// browser down.
async fn fetch_launched(
    (browser, handler, data_dir, egress, profile_hold): Launched,
    deadline: tokio::time::Instant,
    url: &str,
    config: &CrawlConfig,
    prior_cookies: Option<&[BrowserCookie]>,
    want_screenshot: bool,
) -> Result<BrowserPage, CrawlError> {
    let overall_timeout = config.browser.overall_timeout;
    let (handler_handle, handler_end) = spawn_watched_handler(handler);
    let origin = BrowserOrigin::of_session(
        config.browser.endpoint.as_deref(),
        matches!(data_dir, Some(UserDataDir::Scratch(_))),
    );
    let mut session = OneShotSession {
        browser: Some(Arc::new(browser)),
        firewall: None,
        open_tab: None,
        handler_handle: Some(handler_handle),
        data_dir,
        egress,
        profile_hold,
        shutdown_timeout: config.browser.shutdown_timeout,
        origin,
    };

    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    let fetch_outcome = tokio::time::timeout(remaining, async {
        let (page, watch) = session.open_watched_page(config).await?;
        let result = page_fetch(url, config, &page, &watch, prior_cookies, want_screenshot).await;
        watch.close().await;
        let mut result = result?;
        crate::net::egress::add_refused(&mut result.refused, session.egress_refused().await);
        Ok(result)
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
    fetch_outcome
        .unwrap_or_else(|_| Err(overall_deadline_error(overall_timeout)))
        .map_err(|error| connection_closed_error(error, &handler_end))
}

/// `error`, saying that the browser's CDP connection closed when that is why the fetch failed.
///
/// ~keep A command that waits when the handler ends fails with chromiumoxide's channel error
/// ~keep ("oneshot canceled", "send failed because receiver is gone"), which names no cause. The
/// ~keep handler's end is set before any command fails for it, so it is read here without a race.
fn connection_closed_error(error: CrawlError, handler_end: &HandlerEnd) -> CrawlError {
    match error {
        CrawlError::BrowserError { message, source } if handler_end.has_ended() => {
            let message = match handler_end.cause() {
                Some(cause) => format!("the browser's CDP connection closed ({cause}): {message}"),
                None => format!("the browser's CDP connection closed: {message}"),
            };
            CrawlError::BrowserError { message, source }
        }
        other => other,
    }
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
    data_dir: Option<UserDataDir>,
    /// The SSRF proxy a `browser_profile` Chrome was launched through. It stops after the
    /// browser, so a late connection meets a refusal, never a closed port.
    egress: Option<crate::net::egress::Egress>,
    /// The hold on a saved `browser_profile`, released only once teardown has reaped Chrome.
    profile_hold: Option<launch::ProfileHold>,
    shutdown_timeout: Duration,
    origin: BrowserOrigin,
}

impl OneShotSession {
    /// Start the browser's SSRF check, open this fetch's tab, recording it for teardown, and
    /// hand back a handle to it with its watch. With `browser.endpoint`, the tab opens in a
    /// browser context made with the crawl's proxy.
    async fn open_watched_page(&mut self, config: &CrawlConfig) -> Result<(chromiumoxide::Page, Watch), CrawlError> {
        let browser = self.browser.as_ref().expect("browser is taken only by Drop");
        let sockets = crate::net::egress::socket_policy(
            &config.ssrf,
            config.browser.endpoint.as_deref(),
            &std::sync::Once::new(),
        )?;
        let handler_abort = self
            .handler_handle
            .as_ref()
            .expect("the handler is taken only by Drop")
            .abort_handle();
        let firewall =
            BrowserFirewall::start_supervised(Arc::clone(browser), self.origin, PageContext::of(config), handler_abort)
                .await?;
        let firewall = self.firewall.insert(firewall);
        // ~keep A launched Chrome has the proxy from `--proxy-server`; a connected one never got
        // ~keep that flag, so there the page's own browser context is made with the proxy. Under
        // ~keep Under IP-level SSRF denial the context goes through the SSRF proxy, which leaves through it.
        let proxy = if config.browser.endpoint.is_some() || config.ssrf.enforces_ip_denials() {
            crate::proxy::chrome_proxy_for(config)?
        } else {
            None
        };
        let page = firewall
            .handle()
            .new_page_with_policy(proxy.as_ref(), sockets, &config.ssrf)
            .await?;
        self.open_tab = Some(page.target_id().clone());
        let watch = firewall.handle().watch(&page, config, config.max_redirects).await?;
        Ok((page, watch))
    }

    /// Every `host:port` the SSRF proxies of this session refused. The session has one page,
    /// so each is that page's.
    async fn egress_refused(&self) -> Vec<String> {
        let mut refused = self.egress.as_ref().map(Egress::refused).unwrap_or_default();
        if let Some(ref firewall) = self.firewall {
            refused.extend(firewall.handle().egress_refused().await);
        }
        refused
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
        let egress = self.egress.take();
        let profile_hold = self.profile_hold.take();
        let shutdown_timeout = self.shutdown_timeout;
        let origin = self.origin;

        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                // ~keep The task owns `data_dir`, so a runtime that shuts down before the task
                // ~keep finishes still removes a scratch directory when it drops the task.
                handle.spawn(async move {
                    if let Some(firewall) = firewall {
                        firewall.stop().await;
                    }
                    // ~keep The stopped firewall held the only other reference, so this is the
                    // ~keep browser itself. One launched with a throwaway profile is killed with
                    // ~keep interception still on, as `interact` does (xberg-io/crawlberg#468).
                    match (Arc::into_inner(browser), data_dir) {
                        (Some(browser), Some(UserDataDir::Scratch(profile))) if origin == BrowserOrigin::Killed => {
                            kill_browser(browser, handler_handle, profile.path().to_path_buf(), shutdown_timeout).await;
                        }
                        (browser, profile) => {
                            match browser {
                                Some(browser) => {
                                    release_browser(browser, handler_handle, cleanup, shutdown_timeout).await
                                }
                                None => handler_handle.abort(),
                            }
                            drop(profile);
                        }
                    }
                    drop(egress);
                    // ~keep `release_browser` returns once Chrome has exited and been reaped, or
                    // ~keep been killed and reaped after `shutdown_timeout`, and `kill_browser` once
                    // ~keep every process of that Chrome is gone or the timeout has passed; only
                    // ~keep then may the next session on the profile start.
                    drop(profile_hold);
                });
            }
            Err(_) => {
                tracing::warn!(
                    "dropping a one-shot browser session outside a Tokio runtime; a launched Chrome \
                     is killed without closing, a tab opened in a connected Chrome stays open, and \
                     the profile directory is removed on a background thread"
                );
            }
        }
    }
}

#[cfg(feature = "browser-native")]
async fn native_fetch(
    url: &str,
    config: &CrawlConfig,
    prior_cookies: Option<&[BrowserCookie]>,
    native_executor: Option<&crawlberg_browser::adapter::NativeBrowserExecutor>,
) -> Result<(HttpResponse, Vec<String>, usize, Option<String>), CrawlError> {
    let native_executor = native_executor.ok_or_else(|| {
        CrawlError::browser_error("native browser executor is not available for BrowserBackend::Native")
    })?;
    let prior_cookies: Option<Vec<CookieInfo>> = prior_cookies.map(|cookies| {
        cookies
            .iter()
            .map(|cookie| CookieInfo {
                name: cookie.params.name.clone(),
                value: cookie.params.value.clone(),
                domain: cookie.params.domain.clone(),
                path: cookie.params.path.clone(),
            })
            .collect()
    });
    crate::native_browser::native_browser_fetch(url, config, prior_cookies.as_deref(), native_executor).await
}

#[cfg(not(feature = "browser-native"))]
async fn native_fetch(
    _url: &str,
    _config: &CrawlConfig,
    _prior_cookies: Option<&[BrowserCookie]>,
) -> Result<(HttpResponse, Vec<String>, usize, Option<String>), CrawlError> {
    Err(CrawlError::invalid_config(
        "browser.backend = native requires the browser-native feature",
    ))
}

#[cfg(all(test, feature = "browser-native"))]
mod tests {
    use std::time::Duration;

    use crawlberg_browser::adapter::{NativeBrowserExecutor, NativeBrowserExecutorConfig};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::{BrowserPage, browser_fetch};
    use crate::error::CrawlError;
    use crate::types::{BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig};

    async fn native_fetch_route(
        site: &MockServer,
        route: &str,
        executor: &NativeBrowserExecutor,
    ) -> Result<BrowserPage, CrawlError> {
        let config = CrawlConfig {
            browser: BrowserConfig {
                backend: BrowserBackend::Native,
                mode: BrowserMode::Always,
                timeout: Duration::from_secs(10),
                ..BrowserConfig::default()
            },
            ..CrawlConfig::builder().allow_private_networks(true).build()
        };
        browser_fetch(
            &format!("{}{route}", site.uri()),
            &config,
            None,
            None,
            false,
            Some(executor),
        )
        .await
    }

    /// A native fetch that followed a redirect to a 404 is a page with status 404 and no body,
    /// as in HTTP mode. The same 404 without the redirect is `NotFound`.
    #[tokio::test]
    async fn a_native_fetch_reports_a_404_after_a_redirect_as_a_page() {
        let site = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/missing"))
            .respond_with(ResponseTemplate::new(404).set_body_raw("<html><body>gone</body></html>", "text/html"))
            .mount(&site)
            .await;
        Mock::given(method("GET"))
            .and(path("/moved"))
            .respond_with(ResponseTemplate::new(302).append_header("location", "/missing"))
            .mount(&site)
            .await;
        let executor = NativeBrowserExecutor::new(NativeBrowserExecutorConfig::with_workers(1))
            .expect("single-worker executor should start");
        let page = native_fetch_route(&site, "/moved", &executor)
            .await
            .expect("a 404 at the end of a redirect is a page");
        assert_eq!(
            (page.response.status, page.response.body.as_str(), page.redirected),
            (404, "", true)
        );
        let direct = native_fetch_route(&site, "/missing", &executor).await;
        assert!(matches!(direct, Err(CrawlError::NotFound { .. })), "{:?}", direct.err());
    }

    /// A native fetch reads a page with the character set of the page, and says that its text is
    /// decoded, so nothing decodes it a second time.
    #[tokio::test]
    async fn a_native_fetch_decodes_the_page_and_says_which_character_set_it_used() {
        let body = b"<html><head><meta charset=\"iso-8859-1\"></head><body><p>caf\xe9</p></body></html>";
        let site = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/latin1"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body.to_vec(), "text/html"))
            .mount(&site)
            .await;
        let executor = NativeBrowserExecutor::new(NativeBrowserExecutorConfig::with_workers(1))
            .expect("single-worker executor should start");
        let page = native_fetch_route(&site, "/latin1", &executor)
            .await
            .expect("the page must load");
        assert!(
            page.response.body.contains("<p>caf\u{e9}</p>"),
            "the Latin-1 letter must be read: {}",
            page.response.body
        );
        assert_eq!(
            page.text,
            crate::tower::BodyText::Decoded {
                charset: Some("iso-8859-1".to_owned()),
            }
        );
    }
}

#[cfg(test)]
mod launch_supervision_tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::print_stderr, reason = "test-only skip announcement")]
    async fn cancelling_one_shot_after_launch_reaps_its_unclaimed_chrome() {
        let test_name = "cancelling_one_shot_after_launch_reaps_its_unclaimed_chrome";
        let Some(chrome) = crate::browser_pool::chrome_executable(None) else {
            eprintln!("skipping {test_name} because no usable Chrome was found");
            return;
        };
        let marker = format!("--crawlberg-test-launch-supervision={}", std::process::id());
        let (completed_tx, completed_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        *COMPLETED_LAUNCH_HOOK.lock().unwrap_or_else(|error| error.into_inner()) = Some(CompletedLaunchHook {
            marker: marker.clone(),
            completed: completed_tx,
            release: release_rx,
        });
        let config = CrawlConfig {
            browser: crate::types::BrowserConfig {
                chrome_path: Some(chrome),
                chrome_args: vec![marker],
                ..Default::default()
            },
            ..Default::default()
        };
        let fetch = tokio::spawn(async move { one_shot_fetch("about:blank", &config, None, false).await });
        let completed = tokio::time::timeout(crate::browser_pool::tests::PROCESS_TEST_WAIT, completed_rx)
            .await
            .expect("the supervised Chrome launch must finish within the process-test bound")
            .expect("the launch hook must report its result");
        let profile = match completed {
            Ok(profile) => profile,
            Err(error) => {
                let _ = release_tx.send(());
                let _ = fetch.await;
                eprintln!("skipping {test_name} because Chrome failed to launch: {error}");
                return;
            }
        };

        fetch.abort();
        match fetch.await {
            Err(error) if error.is_cancelled() => {}
            Err(_) => panic!("the public one-shot future must end by cancellation"),
            Ok(_) => panic!("the public one-shot future must not complete after abort"),
        }
        let _ = release_tx.send(());

        crate::browser_pool::tests::assert_profile_directory_is_gone_for_good(&profile);
        assert_eq!(
            crate::browser_pool::tests::chrome_process_count_for_profile(&profile),
            0,
            "no Chrome process may keep using the cancelled fetch's profile"
        );
    }
}
