//! Headless Chrome/CDP browser fallback for fetching JavaScript-rendered pages.
//!
//! This module is only compiled when the `browser` feature is enabled.

use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chromiumoxide::Handler;
use chromiumoxide::browser::{Browser, BrowserConfig as ChromeBrowserConfig};
use chromiumoxide::cdp::browser_protocol::emulation::SetDeviceMetricsOverrideParams;
use chromiumoxide::cdp::browser_protocol::fetch::{
    ContinueRequestParams, DisableParams as FetchDisableParams, EnableParams as FetchEnableParams, EventRequestPaused,
    FailRequestParams,
};
use chromiumoxide::cdp::browser_protocol::network::{ErrorReason, Headers, SetCookieParams, SetExtraHttpHeadersParams};
use tokio_stream::StreamExt;
use tracing::Instrument as _;

use crate::browser_pool::BrowserPool;
use crate::error::CrawlError;
use crate::http::HttpResponse;
use crate::net::ssrf::{SsrfPolicy, validate_url};
use crate::telemetry::attributes::{CRAWL_BROWSER_BACKEND, CRAWL_BROWSER_SESSION_ID, CRAWL_PAGES_RENDERED};
use crate::telemetry::metrics::registry;
use crate::types::{AuthConfig, BrowserBackend, BrowserWait, CookieInfo, CrawlConfig};

/// Process-wide monotonic session counter for `crawl.browser.session_id`.
static BROWSER_SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Fetch a URL using a headless Chrome browser via CDP.
///
/// When `pool` is `Some`, acquires a page from the pool, uses it, and returns
/// it on completion. When `pool` is `None`, launches a one-shot browser
/// instance and tears it down afterwards.
///
/// Returns an `HttpResponse` compatible with the existing scrape pipeline.
pub(crate) async fn browser_fetch(
    url: &str,
    config: &CrawlConfig,
    prior_cookies: Option<&[CookieInfo]>,
    pool: Option<&BrowserPool>,
    #[cfg(feature = "browser-native")] native_executor: Option<&crawlberg_browser::adapter::NativeBrowserExecutor>,
) -> Result<HttpResponse, CrawlError> {
    match config.browser.backend {
        BrowserBackend::Chromiumoxide => chromiumoxide_fetch(url, config, prior_cookies, pool).await,
        BrowserBackend::Native => {
            #[cfg(feature = "browser-native")]
            {
                native_fetch(url, config, prior_cookies, native_executor).await
            }
            #[cfg(not(feature = "browser-native"))]
            {
                native_fetch(url, config, prior_cookies).await
            }
        }
    }
}

async fn chromiumoxide_fetch(
    url: &str,
    config: &CrawlConfig,
    prior_cookies: Option<&[CookieInfo]>,
    pool: Option<&BrowserPool>,
) -> Result<HttpResponse, CrawlError> {
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

    chromiumoxide_fetch_inner(url, config, prior_cookies, pool)
        .instrument(span)
        .await
}

async fn chromiumoxide_fetch_inner(
    url: &str,
    config: &CrawlConfig,
    prior_cookies: Option<&[CookieInfo]>,
    pool: Option<&BrowserPool>,
) -> Result<HttpResponse, CrawlError> {
    let target = url::Url::parse(url).map_err(|e| CrawlError::ssrf_violation(url, format!("invalid URL: {e}")))?;
    validate_url(&target, &config.ssrf)
        .await
        .map_err(|e| CrawlError::ssrf_violation(url, e.to_string()))?;

    if let Some(pool) = pool {
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
            let session_pool = config.browser_session_pool.as_deref().ok_or_else(|| {
                CrawlError::BrowserError("session_affinity enabled but session pool is not configured".into())
            })?;

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

        let result = page_fetch(url, config, &page, prior_cookies).await;

        if config.browser.session_affinity
            && result.is_ok()
            && let Ok(session_key) = crate::browser_session_pool::SessionKey::from_url(
                url,
                config.browser.proxy.as_ref().map(|p| p.url.as_str()),
            )
            && let Some(session_pool) = config.browser_session_pool.as_deref()
        {
            session_pool.insert(session_key, page, permit).await;
        } else {
            let _ = page.close().await;
            drop(permit);
        }

        result
    } else {
        let (mut browser, mut handler, data_dir) = launch_or_connect(config).await?;
        let handler_handle = tokio::spawn(async move { while handler.next().await.is_some() {} });

        let page = browser
            .new_page("about:blank")
            .await
            .map_err(|e| CrawlError::BrowserError(format!("failed to create page: {e}")))?;

        let result = page_fetch(url, config, &page, prior_cookies).await;

        let _ = page.close().await;
        let _ = browser.close().await;
        let _ = browser.wait().await;
        drop(browser);
        let _ = tokio::time::timeout(Duration::from_secs(5), handler_handle).await;

        if let Some(dir) = data_dir {
            let _ = std::fs::remove_dir_all(&dir);
        }

        result
    }
}

#[cfg(feature = "browser-native")]
async fn native_fetch(
    url: &str,
    config: &CrawlConfig,
    prior_cookies: Option<&[CookieInfo]>,
    native_executor: Option<&crawlberg_browser::adapter::NativeBrowserExecutor>,
) -> Result<HttpResponse, CrawlError> {
    let native_executor = native_executor.ok_or_else(|| {
        CrawlError::BrowserError("native browser executor is not available for BrowserBackend::Native".into())
    })?;
    crate::native_browser::native_browser_fetch(url, config, prior_cookies, native_executor).await
}

#[cfg(not(feature = "browser-native"))]
async fn native_fetch(
    _url: &str,
    _config: &CrawlConfig,
    _prior_cookies: Option<&[CookieInfo]>,
) -> Result<HttpResponse, CrawlError> {
    Err(CrawlError::InvalidConfig(
        "browser.backend = native requires the browser-native feature".into(),
    ))
}

/// Active CDP Fetch-domain interception that re-validates every browser-issued
/// request against the SSRF policy. Held alive across a navigation; consuming it
/// via [`SsrfInterceptGuard::finish`] disables interception, stops the listener,
/// and reports the first request that was blocked.
struct SsrfInterceptGuard {
    page: chromiumoxide::Page,
    listener: tokio::task::JoinHandle<()>,
    blocked: Arc<Mutex<Option<(String, String)>>>,
}

impl SsrfInterceptGuard {
    /// Disable interception, stop the listener, and return the first blocked
    /// `(url, reason)` observed during the navigation, if any.
    async fn finish(self) -> Option<(String, String)> {
        let _ = self.page.execute(FetchDisableParams::default()).await;
        self.listener.abort();
        match self.blocked.lock() {
            Ok(mut slot) => slot.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        }
    }
}

/// Enable CDP Fetch interception on `page`, validating every intercepted request
/// URL against `policy` before Chrome connects. Requests resolving to blocked
/// addresses (loopback, RFC1918, link-local, cloud metadata, non-http(s)
/// schemes) are failed with `BlockedByClient` and the first one is recorded so
/// the caller can surface a precise [`CrawlError::SsrfPolicyViolation`].
///
/// This closes the residual gap left by the pre-navigation seed check: the
/// browser follows 3xx redirects and client-side navigations internally, so
/// without per-request interception a redirect or `location` change to a
/// private/metadata address would reach the network unchecked.
/// Decide whether an intercepted request URL is permitted by the SSRF policy.
/// Returns `Err(reason)` when the request must be failed at the CDP layer. This
/// is the per-request decision applied to every browser-issued request.
async fn ssrf_verdict(request_url: &str, policy: &SsrfPolicy) -> Result<(), String> {
    match url::Url::parse(request_url) {
        Ok(parsed) => validate_url(&parsed, policy).await.map_err(|e| e.to_string()),
        Err(e) => Err(format!("invalid URL: {e}")),
    }
}

async fn start_ssrf_interception(
    page: &chromiumoxide::Page,
    policy: &SsrfPolicy,
) -> Result<SsrfInterceptGuard, CrawlError> {
    let mut events = page
        .event_listener::<EventRequestPaused>()
        .await
        .map_err(|e| CrawlError::BrowserError(format!("failed to register intercept listener: {e}")))?;

    page.execute(FetchEnableParams::default())
        .await
        .map_err(|e| CrawlError::BrowserError(format!("failed to enable request interception: {e}")))?;

    let blocked: Arc<Mutex<Option<(String, String)>>> = Arc::new(Mutex::new(None));
    let listener_page = page.clone();
    let listener_policy = policy.clone();
    let listener_blocked = Arc::clone(&blocked);

    let listener = tokio::spawn(async move {
        while let Some(event) = events.next().await {
            let request_id = event.request_id.clone();
            let request_url = event.request.url.clone();

            match ssrf_verdict(&request_url, &listener_policy).await {
                Ok(()) => {
                    let _ = listener_page.execute(ContinueRequestParams::new(request_id)).await;
                }
                Err(reason) => {
                    if let Ok(mut slot) = listener_blocked.lock()
                        && slot.is_none()
                    {
                        *slot = Some((request_url, reason));
                    }
                    let _ = listener_page
                        .execute(FailRequestParams::new(request_id, ErrorReason::BlockedByClient))
                        .await;
                }
            }
        }
    });

    Ok(SsrfInterceptGuard {
        page: page.clone(),
        listener,
        blocked,
    })
}

/// Navigate a pre-existing CDP page to `url`, wait for rendering, and extract
/// the final HTML. The caller provides the page; this function does not
/// create or close it.
async fn page_fetch(
    url: &str,
    config: &CrawlConfig,
    page: &chromiumoxide::Page,
    prior_cookies: Option<&[CookieInfo]>,
) -> Result<HttpResponse, CrawlError> {
    let stealth = matches!(config.browser.mode, crate::types::BrowserMode::Stealth);

    if stealth {
        crate::stealth::apply_stealth_patches(page).await;
    }

    let resolved_ua = if let Some(ref ua) = config.user_agent {
        ua.clone()
    } else if stealth {
        resolve_default_user_agent().to_string()
    } else {
        "".to_string()
    };

    if !resolved_ua.is_empty() {
        page.set_user_agent(&resolved_ua)
            .await
            .map_err(|e| CrawlError::BrowserError(format!("failed to set user agent: {e}")))?;
    }

    if stealth && let Err(e) = set_viewport(page, 1920, 1080).await {
        return Err(CrawlError::BrowserError(format!("failed to set viewport: {e}")));
    }

    if let Some(cookies) = prior_cookies {
        for cookie in cookies {
            let mut builder = SetCookieParams::builder().name(&cookie.name).value(&cookie.value);
            if let Some(ref domain) = cookie.domain {
                builder = builder.domain(domain);
            }
            if let Some(ref path) = cookie.path {
                builder = builder.path(path);
            }
            if let Ok(params) = builder.build() {
                let _ = page.execute(params).await;
            }
        }
    }

    let mut extra_headers = serde_json::Map::new();
    for (k, v) in &config.custom_headers {
        extra_headers.insert(k.clone(), serde_json::Value::String(v.clone()));
    }
    match config.auth {
        Some(AuthConfig::Bearer { ref token }) => {
            extra_headers.insert(
                "Authorization".to_owned(),
                serde_json::Value::String(format!("Bearer {token}")),
            );
        }
        Some(AuthConfig::Header { ref name, ref value }) => {
            extra_headers.insert(name.clone(), serde_json::Value::String(value.clone()));
        }
        _ => {}
    }
    if !extra_headers.is_empty() {
        let params = SetExtraHttpHeadersParams::new(Headers::new(serde_json::Value::Object(extra_headers)));
        page.execute(params)
            .await
            .map_err(|e| CrawlError::BrowserError(format!("failed to set headers: {e}")))?;
    }

    let timeout = config.browser.timeout;

    let interceptor = start_ssrf_interception(page, &config.ssrf).await?;

    let navigation = tokio::time::timeout(timeout, async {
        page.goto(url)
            .await
            .map_err(|e| CrawlError::BrowserError(format!("navigation failed: {e}")))?;

        wait_for_ready(page, config)
            .await
            .map_err(|e| CrawlError::BrowserError(format!("wait failed: {e}")))?;

        Ok::<(), CrawlError>(())
    })
    .await;

    let blocked = interceptor.finish().await;
    match navigation {
        Ok(Ok(())) => {}
        Ok(Err(navigation_error)) => {
            if let Some((blocked_url, reason)) = blocked {
                return Err(CrawlError::SsrfPolicyViolation {
                    url: blocked_url,
                    reason,
                });
            }
            return Err(navigation_error);
        }
        Err(_) => {
            if let Some((blocked_url, reason)) = blocked {
                return Err(CrawlError::SsrfPolicyViolation {
                    url: blocked_url,
                    reason,
                });
            }
            return Err(CrawlError::BrowserTimeout(format!(
                "browser timed out after {timeout:?}"
            )));
        }
    }

    if let Some(extra) = config.browser.extra_wait {
        tokio::time::sleep(extra).await;
    }

    let html = page
        .content()
        .await
        .map_err(|e| CrawlError::BrowserError(format!("failed to extract HTML: {e}")))?;

    let body_bytes = html.as_bytes().to_vec();

    // ~keep CDP `page.content()` does not expose HTTP status; rendered pages report synthetic 200 here.
    Ok(HttpResponse {
        status: 200,
        content_type: "text/html".to_owned(),
        body: html,
        body_bytes,
        headers: std::collections::HashMap::new(),
        browser_extras: None,
        // ~keep CDP final URL is unavailable here; this path only feeds browser backends, not wasm final_url tracking.
        final_url: url.to_owned(),
    })
}

/// Wait for the page to be ready based on the configured wait strategy.
async fn wait_for_ready(
    page: &chromiumoxide::Page,
    config: &CrawlConfig,
) -> Result<(), chromiumoxide::error::CdpError> {
    match config.browser.wait {
        BrowserWait::NetworkIdle => {
            // ~keep `NetworkIdle` is a settle delay here, not true CDP zero-in-flight detection.
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        BrowserWait::Selector => {
            if let Some(ref selector) = config.browser.wait_selector {
                page.find_element(selector).await?;
            } else {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
        BrowserWait::Fixed => {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
    Ok(())
}

/// The Chrome `--user-data-dir` to launch with, and what to do with it once the
/// browser session ends.
struct UserDataDir {
    path: std::path::PathBuf,
    /// When `true`, the directory is deleted after the session (ephemeral launch,
    /// or a scratch copy of a named profile whose changes should not be saved).
    /// When `false`, the directory is left in place so its contents persist
    /// (a named profile launched with `save_browser_profile: true`).
    cleanup_on_exit: bool,
}

/// Unique-per-launch temp directory name, avoiding Chrome `SingletonLock` collisions
/// when multiple browsers launch concurrently or a previous instance crashed uncleanly.
fn unique_temp_dir(prefix: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
    static LAUNCH_COUNTER: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "{prefix}-{}-{}",
        std::process::id(),
        LAUNCH_COUNTER.fetch_add(1, AtomicOrdering::Relaxed),
    ))
}

/// Resolve the `--user-data-dir` for a one-shot Chrome launch from `config`.
///
/// - No `browser_profile`: a fresh ephemeral temp directory, deleted on exit.
/// - `browser_profile` set and `save_browser_profile: true`: the named profile's
///   own directory (created if missing), used and written to in place.
/// - `browser_profile` set and `save_browser_profile: false`: the named profile's
///   directory (created if missing) is copied into a scratch temp directory so
///   the session starts from existing profile state but any changes made during
///   the session are discarded rather than written back.
fn resolve_user_data_dir(config: &CrawlConfig) -> Result<UserDataDir, CrawlError> {
    let Some(name) = config.browser_profile.as_deref() else {
        return Ok(UserDataDir {
            path: unique_temp_dir("crawlberg-browser"),
            cleanup_on_exit: true,
        });
    };

    let profile = crate::browser_profile::BrowserProfile::new(name)?;
    if !profile.exists() {
        profile.create()?;
    }

    if config.save_browser_profile {
        Ok(UserDataDir {
            path: profile.user_data_dir,
            cleanup_on_exit: false,
        })
    } else {
        let scratch = unique_temp_dir(&format!("crawlberg-profile-{name}"));
        copy_dir_recursive(&profile.user_data_dir, &scratch)?;
        Ok(UserDataDir {
            path: scratch,
            cleanup_on_exit: true,
        })
    }
}

/// Recursively copy `src` into `dst`, creating `dst` if needed. Symlinks inside
/// `src` are skipped rather than followed or copied as links.
fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> Result<(), CrawlError> {
    std::fs::create_dir_all(dst)
        .map_err(|e| CrawlError::Other(format!("failed to create profile scratch directory: {e}")))?;
    let entries =
        std::fs::read_dir(src).map_err(|e| CrawlError::Other(format!("failed to read profile directory: {e}")))?;
    for entry in entries {
        let entry = entry.map_err(|e| CrawlError::Other(format!("failed to read profile entry: {e}")))?;
        let file_type = entry
            .file_type()
            .map_err(|e| CrawlError::Other(format!("failed to stat profile entry: {e}")))?;
        let dest_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &dest_path)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), &dest_path)
                .map_err(|e| CrawlError::Other(format!("failed to copy profile file: {e}")))?;
        }
    }
    Ok(())
}

/// Launch a new managed browser or connect to an external CDP endpoint.
///
/// Each ephemeral launch creates a unique user data directory to avoid Chrome's
/// `SingletonLock` conflicts when multiple instances run concurrently or a
/// previous instance crashed without cleanup. When `config.browser_profile` is
/// set, the launch uses that named profile's directory instead (see
/// [`resolve_user_data_dir`]).
async fn launch_or_connect(config: &CrawlConfig) -> Result<(Browser, Handler, Option<std::path::PathBuf>), CrawlError> {
    if let Some(ref endpoint) = config.browser.endpoint {
        if config.browser_profile.is_some() {
            tracing::warn!(
                profile = config.browser_profile.as_deref().unwrap_or_default(),
                "browser_profile is ignored when connecting to an external browser.endpoint; \
                 the remote Chrome process's profile is managed externally"
            );
        }
        let (browser, handler) = Browser::connect(endpoint)
            .await
            .map_err(|e| CrawlError::BrowserError(format!("failed to connect to {endpoint}: {e}")))?;
        Ok((browser, handler, None))
    } else {
        let user_data = resolve_user_data_dir(config)?;

        let mut builder = ChromeBrowserConfig::builder()
            .no_sandbox()
            .new_headless_mode()
            .user_data_dir(&user_data.path)
            .disable_default_args();
        // ~keep Mirror browser_pool's fork-safety env vars so one-shot and pooled Chrome launch paths match.
        builder = builder
            .env("OBJC_DISABLE_INITIALIZE_FORK_SAFETY", "YES")
            .env("OS_ACTIVITY_MODE", "disable");
        for arg in crate::browser_pool::safe_default_args() {
            builder = builder.arg(arg);
        }
        let browser_config = builder
            .build()
            .map_err(|e| CrawlError::BrowserError(format!("invalid browser config: {e}")))?;

        match Browser::launch(browser_config).await {
            Ok((browser, handler)) => Ok((browser, handler, user_data.cleanup_on_exit.then_some(user_data.path))),
            Err(e) => {
                if user_data.cleanup_on_exit {
                    let _ = std::fs::remove_dir_all(&user_data.path);
                }
                Err(CrawlError::BrowserError(format!("failed to launch browser: {e}")))
            }
        }
    }
}

/// Returns a modern Chrome user-agent string suitable for the runtime environment.
/// Used as the default UA when stealth mode is enabled.
fn resolve_default_user_agent() -> &'static str {
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36"
}

/// Set the viewport (device metrics) via CDP Emulation.setDeviceMetricsOverride.
async fn set_viewport(page: &chromiumoxide::Page, width: u32, height: u32) -> Result<(), Box<dyn std::error::Error>> {
    let params = SetDeviceMetricsOverrideParams::builder()
        .width(width)
        .height(height)
        .device_scale_factor(1.0)
        .build()?;

    page.execute(params).await?;
    Ok(())
}

#[cfg(test)]
mod ssrf_interception_tests {
    //! Unit tests for the per-request SSRF decision applied by browser-tier
    //! Fetch interception. These cover the security-critical verdict (the CDP
    //! plumbing around it is thin glue) and stay hermetic by using literal-IP
    //! and scheme rejections that require no DNS resolution or network.
    use super::ssrf_verdict;
    use crate::net::ssrf::SsrfPolicy;

    fn deny_policy() -> SsrfPolicy {
        SsrfPolicy::default()
    }

    fn allow_private_policy() -> SsrfPolicy {
        SsrfPolicy {
            deny_private: false,
            ..SsrfPolicy::default()
        }
    }

    #[tokio::test]
    async fn rejects_loopback_navigation() {
        let verdict = ssrf_verdict("http://127.0.0.1/admin", &deny_policy()).await;
        assert!(verdict.is_err(), "loopback must be rejected: {verdict:?}");
    }

    #[tokio::test]
    async fn rejects_cloud_metadata_address() {
        let verdict = ssrf_verdict("http://169.254.169.254/latest/meta-data/", &deny_policy()).await;
        assert!(verdict.is_err(), "cloud metadata IP must be rejected: {verdict:?}");
    }

    #[tokio::test]
    async fn rejects_non_http_scheme() {
        let verdict = ssrf_verdict("file:///etc/passwd", &deny_policy()).await;
        assert!(verdict.is_err(), "file:// scheme must be rejected: {verdict:?}");
    }

    #[tokio::test]
    async fn rejects_malformed_url() {
        let verdict = ssrf_verdict("not a url", &deny_policy()).await;
        assert!(verdict.is_err(), "malformed URL must be rejected: {verdict:?}");
    }

    #[tokio::test]
    async fn allows_loopback_when_private_networks_permitted() {
        let verdict = ssrf_verdict("http://127.0.0.1/", &allow_private_policy()).await;
        assert!(
            verdict.is_ok(),
            "loopback must pass when deny_private=false: {verdict:?}"
        );
    }
}

#[cfg(test)]
mod user_data_dir_tests {
    //! Hermetic, Chrome-free unit tests for the `browser_profile` /
    //! `save_browser_profile` wiring in [`resolve_user_data_dir`] and
    //! [`copy_dir_recursive`]. End-to-end proof that Chrome actually launches
    //! against these resolved directories lives in
    //! `crates/crawlberg/tests/test_browser_profile.rs`.
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::browser_profile::BrowserProfile;
    use crate::types::CrawlConfig;

    static NAME_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A collision-free profile name so parallel test runs never race on the
    /// same on-disk profile directory.
    fn unique_profile_name(tag: &str) -> String {
        format!(
            "crawlberg-unit-test-{tag}-{}-{}",
            std::process::id(),
            NAME_COUNTER.fetch_add(1, Ordering::Relaxed)
        )
    }

    /// Deletes the backing profile directory on drop, regardless of outcome.
    struct ProfileGuard(BrowserProfile);
    impl Drop for ProfileGuard {
        fn drop(&mut self) {
            let _ = self.0.delete();
        }
    }

    #[test]
    fn no_profile_resolves_to_ephemeral_temp_dir_marked_for_cleanup() {
        let config = CrawlConfig::default();
        let resolved = resolve_user_data_dir(&config).expect("resolve must succeed without a profile configured");
        assert!(
            resolved.cleanup_on_exit,
            "ephemeral (no browser_profile) launches must be cleaned up after the session"
        );
    }

    #[test]
    fn missing_named_profile_is_created_and_used_directly_when_saved() {
        let name = unique_profile_name("create-save");
        let profile = BrowserProfile::new(&name).expect("profile name must be valid");
        assert!(!profile.exists(), "precondition: profile must not exist yet");
        let _guard = ProfileGuard(profile.clone());

        let config = CrawlConfig {
            browser_profile: Some(name.clone()),
            save_browser_profile: true,
            ..CrawlConfig::default()
        };
        let resolved = resolve_user_data_dir(&config).expect("resolve must succeed");

        assert!(
            profile.exists(),
            "resolve_user_data_dir must create the named profile directory when missing"
        );
        assert_eq!(
            resolved.path, profile.user_data_dir,
            "save_browser_profile: true must launch directly against the profile's own directory"
        );
        assert!(
            !resolved.cleanup_on_exit,
            "save_browser_profile: true must not mark the profile directory for cleanup"
        );
    }

    #[test]
    fn unsaved_profile_launches_from_a_scratch_copy_that_preserves_the_original() {
        let name = unique_profile_name("no-save");
        let profile = BrowserProfile::new(&name).expect("profile name must be valid");
        profile.create().expect("profile directory must be creatable");
        let _guard = ProfileGuard(profile.clone());
        std::fs::write(profile.user_data_dir.join("marker.txt"), b"original").expect("marker file must be writable");

        let config = CrawlConfig {
            browser_profile: Some(name.clone()),
            save_browser_profile: false,
            ..CrawlConfig::default()
        };
        let resolved = resolve_user_data_dir(&config).expect("resolve must succeed");

        assert_ne!(
            resolved.path, profile.user_data_dir,
            "save_browser_profile: false must launch from a scratch copy, never the profile dir itself"
        );
        assert!(
            resolved.cleanup_on_exit,
            "the scratch copy must be marked for cleanup after the session"
        );
        assert_eq!(
            std::fs::read(resolved.path.join("marker.txt")).expect("scratch copy must contain the marker file"),
            b"original",
            "the scratch copy must start from the existing profile state"
        );

        std::fs::write(resolved.path.join("marker.txt"), b"mutated-in-session")
            .expect("writing into the scratch copy must succeed");
        assert_eq!(
            std::fs::read(profile.user_data_dir.join("marker.txt")).expect("original marker file must still exist"),
            b"original",
            "writes into the scratch copy must never be reflected back into the saved profile"
        );

        let _ = std::fs::remove_dir_all(&resolved.path);
    }

    #[test]
    fn copy_dir_recursive_copies_nested_files_and_skips_symlinks() {
        let root = std::env::temp_dir().join(unique_profile_name("copy"));
        let src = root.join("src");
        let dst = root.join("dst");
        std::fs::create_dir_all(src.join("nested")).expect("nested src dir must be creatable");
        std::fs::write(src.join("top.txt"), b"top").expect("top-level file must be writable");
        std::fs::write(src.join("nested").join("deep.txt"), b"deep").expect("nested file must be writable");

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let _ = symlink(src.join("top.txt"), src.join("link.txt"));
        }

        copy_dir_recursive(&src, &dst).expect("recursive copy must succeed");

        assert_eq!(std::fs::read(dst.join("top.txt")).unwrap(), b"top");
        assert_eq!(std::fs::read(dst.join("nested").join("deep.txt")).unwrap(), b"deep");
        assert!(
            !dst.join("link.txt").exists(),
            "symlinks in the source directory must not be copied"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
