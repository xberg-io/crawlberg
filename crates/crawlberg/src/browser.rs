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
        .map_err(|e| CrawlError::ssrf_violation(url.to_string(), e.to_string()))?;

    if let Some(pool) = pool {
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

/// Launch a new managed browser or connect to an external CDP endpoint.
///
/// Each launch creates a unique user data directory to avoid Chrome's
/// `SingletonLock` conflicts when multiple instances run concurrently
/// or a previous instance crashed without cleanup.
async fn launch_or_connect(config: &CrawlConfig) -> Result<(Browser, Handler, Option<std::path::PathBuf>), CrawlError> {
    if let Some(ref endpoint) = config.browser.endpoint {
        let (browser, handler) = Browser::connect(endpoint)
            .await
            .map_err(|e| CrawlError::BrowserError(format!("failed to connect to {endpoint}: {e}")))?;
        Ok((browser, handler, None))
    } else {
        use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
        static LAUNCH_COUNTER: AtomicU64 = AtomicU64::new(0);
        let user_data_dir = std::env::temp_dir().join(format!(
            "crawlberg-browser-{}-{}",
            std::process::id(),
            LAUNCH_COUNTER.fetch_add(1, AtomicOrdering::Relaxed),
        ));

        let mut builder = ChromeBrowserConfig::builder()
            .no_sandbox()
            .new_headless_mode()
            .user_data_dir(&user_data_dir)
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
            Ok((browser, handler)) => Ok((browser, handler, Some(user_data_dir))),
            Err(e) => {
                let _ = std::fs::remove_dir_all(&user_data_dir);
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
