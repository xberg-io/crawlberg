//! Navigate a pre-existing CDP page, wait for rendering, and extract the final
//! HTML (plus an optional screenshot). This is the per-page work shared by both
//! the pooled and one-shot chromiumoxide fetch paths in the parent module.

use std::time::Duration;

use chromiumoxide::cdp::browser_protocol::emulation::SetDeviceMetricsOverrideParams;
use chromiumoxide::cdp::browser_protocol::network::{Headers, SetCookieParams, SetExtraHttpHeadersParams};
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::page::ScreenshotParams;

use super::BrowserPage;
use super::launch::resolve_default_user_agent;
use crate::error::CrawlError;
use crate::http::HttpResponse;
use crate::ssrf_intercept::{StoppedResponse, start_ssrf_interception};
use crate::types::{AuthConfig, BrowserWait, CookieInfo, CrawlConfig};

/// Viewport a stealth session presents, chosen to match a common desktop display
/// so the reported metrics are unremarkable.
const STEALTH_VIEWPORT_WIDTH: u32 = 1920;
const STEALTH_VIEWPORT_HEIGHT: u32 = 1080;

/// Synthetic status and content type reported for a CDP-rendered page.
const RENDERED_PAGE_STATUS: u16 = 200;
const RENDERED_PAGE_CONTENT_TYPE: &str = "text/html";

/// Navigate a pre-existing CDP page to `url`, wait for rendering, and extract
/// the final HTML. The caller provides the page; this function does not
/// create or close it.
///
/// Chrome follows at most `config.max_redirects` HTTP redirects. A chain longer than
/// that ends on the redirect response at the limit, the way the HTTP fetch path ends.
/// A response Chrome does not commit (204, 205, 304) ends the fetch the same way.
pub(super) async fn page_fetch(
    url: &str,
    config: &CrawlConfig,
    page: &chromiumoxide::Page,
    prior_cookies: Option<&[CookieInfo]>,
    want_screenshot: bool,
) -> Result<BrowserPage, CrawlError> {
    let stealth = matches!(config.browser.mode, crate::types::BrowserMode::Stealth);

    if stealth {
        crate::stealth::apply_stealth_patches(page).await;
    }

    apply_user_agent(page, config, stealth).await?;

    if stealth && let Err(e) = set_viewport(page, STEALTH_VIEWPORT_WIDTH, STEALTH_VIEWPORT_HEIGHT).await {
        return Err(CrawlError::browser_error(format!("failed to set viewport: {e}")));
    }

    apply_prior_cookies(page, prior_cookies).await;
    apply_extra_headers(page, config).await?;

    let timeout = config.browser.timeout;

    let interceptor = start_ssrf_interception(page, &config.ssrf, config.max_redirects).await?;

    let navigation = tokio::time::timeout(timeout, async {
        page.goto(url)
            .await
            .map_err(|e| CrawlError::browser_error(format!("navigation failed: {e}")))?;

        wait_for_ready(page, config)
            .await
            .map_err(|e| CrawlError::browser_error(format!("wait failed: {e}")))?;

        Ok::<(), CrawlError>(())
    })
    .await;

    let intercepted = interceptor.finish().await;
    if intercepted.blocked.is_none()
        && let Some(stop) = intercepted.stopped_response
    {
        return Ok(BrowserPage {
            response: stopped_response(stop),
            redirects: intercepted.redirects_followed,
        });
    }
    resolve_navigation_outcome(navigation, intercepted.blocked, timeout)?;

    if let Some(extra) = config.browser.extra_wait {
        tokio::time::sleep(extra).await;
    }

    let html = page
        .content()
        .await
        .map_err(|e| CrawlError::browser_error(format!("failed to extract HTML: {e}")))?;

    // ~keep Chrome follows redirects itself, so the page it landed on is the base its links
    // ~keep resolve against. An unreadable URL falls back to the requested one.
    let final_url = page.url().await.ok().flatten().unwrap_or_else(|| url.to_owned());

    let body_bytes = html.as_bytes().to_vec();
    let screenshot = capture_screenshot(page, config, want_screenshot).await;

    // ~keep CDP `page.content()` does not expose HTTP status; rendered pages report synthetic 200 here.
    Ok(BrowserPage {
        response: HttpResponse {
            status: RENDERED_PAGE_STATUS,
            content_type: RENDERED_PAGE_CONTENT_TYPE.to_owned(),
            body: html,
            body_bytes,
            headers: std::collections::HashMap::new(),
            browser_extras: None,
            final_url,
            screenshot,
        },
        redirects: intercepted.redirects_followed,
    })
}

/// The response a navigation stopped on without a document, with no body, as the HTTP
/// fetch path reports it.
fn stopped_response(stop: StoppedResponse) -> HttpResponse {
    let content_type = stop
        .headers
        .get("content-type")
        .and_then(|values| values.first())
        .cloned()
        .unwrap_or_default();
    HttpResponse {
        status: stop.status,
        content_type,
        body: String::new(),
        body_bytes: Vec::new(),
        headers: stop.headers,
        browser_extras: None,
        final_url: stop.url,
        screenshot: None,
    }
}

/// Set the page's user agent, if one is configured or implied by stealth mode.
async fn apply_user_agent(page: &chromiumoxide::Page, config: &CrawlConfig, stealth: bool) -> Result<(), CrawlError> {
    let resolved_ua = if let Some(ref ua) = config.user_agent {
        ua.clone()
    } else if stealth {
        resolve_default_user_agent().to_string()
    } else {
        "".to_string()
    };

    if resolved_ua.is_empty() {
        return Ok(());
    }
    page.set_user_agent(&resolved_ua)
        .await
        .map_err(|e| CrawlError::browser_error(format!("failed to set user agent: {e}")))?;
    Ok(())
}

/// Seed the page with cookies carried over from a previous fetch.
///
/// ~keep A cookie that cannot be built or set is skipped rather than failing the
/// fetch: a partial session is still worth attempting, and CDP rejects cookies
/// whose domain does not match the target.
async fn apply_prior_cookies(page: &chromiumoxide::Page, prior_cookies: Option<&[CookieInfo]>) {
    let Some(cookies) = prior_cookies else {
        return;
    };
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

/// Install the configured custom headers plus any `auth`-derived header on the page.
async fn apply_extra_headers(page: &chromiumoxide::Page, config: &CrawlConfig) -> Result<(), CrawlError> {
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
    if extra_headers.is_empty() {
        return Ok(());
    }
    let params = SetExtraHttpHeadersParams::new(Headers::new(serde_json::Value::Object(extra_headers)));
    page.execute(params)
        .await
        .map_err(|e| CrawlError::browser_error(format!("failed to set headers: {e}")))
        .map(|_| ())
}

/// Turn the navigation result and the interceptor's verdict into one error.
///
/// ~keep A request the SSRF interceptor blocked takes precedence over both the
/// navigation error and the timeout: Chrome reports a blocked request as a generic
/// navigation failure, so reporting that would hide the policy violation that
/// actually caused it.
fn resolve_navigation_outcome(
    navigation: Result<Result<(), CrawlError>, tokio::time::error::Elapsed>,
    blocked: Option<(String, String)>,
    timeout: Duration,
) -> Result<(), CrawlError> {
    let navigation_error = match navigation {
        Ok(Ok(())) => return Ok(()),
        Ok(Err(error)) => error,
        Err(_) => CrawlError::browser_timeout(format!("browser timed out after {timeout:?}")),
    };
    if let Some((blocked_url, reason)) = blocked {
        return Err(CrawlError::SsrfPolicyViolation {
            url: blocked_url,
            reason,
            source: None,
        });
    }
    Err(navigation_error)
}

/// Capture a PNG of the current viewport, when the caller asked for one.
async fn capture_screenshot(
    page: &chromiumoxide::Page,
    config: &CrawlConfig,
    want_screenshot: bool,
) -> Option<Vec<u8>> {
    // ~keep Gated on the CALLER wanting the bytes, not merely on the config flag. Only
    // scrape()'s dedicated path consumes a screenshot; every other caller converts through
    // browser_http_to_crawl, which has nowhere to put it. Reading the flag alone meant a
    // crawl paid a full CDP screenshot round-trip per page and then discarded every one.
    if !(want_screenshot && config.capture_screenshot) {
        return None;
    }
    let params = ScreenshotParams::builder()
        .format(CaptureScreenshotFormat::Png)
        .full_page(false)
        .build();
    match page.screenshot(params).await {
        Ok(bytes) => Some(bytes),
        Err(e) => {
            // ~keep A failed screenshot must not fail an otherwise-successful page fetch;
            // ~keep the caller still gets HTML, just no image.
            tracing::warn!(error = %e, "failed to capture page screenshot; continuing without one");
            None
        }
    }
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
mod tests {
    use super::*;

    const TEST_TIMEOUT: Duration = Duration::from_secs(7);

    fn elapsed() -> tokio::time::error::Elapsed {
        // ~keep `Elapsed` has no public constructor, so the only way to obtain one is to
        // ~keep let a zero-duration timeout actually expire.
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime")
            .block_on(async {
                tokio::time::timeout(Duration::ZERO, std::future::pending::<()>())
                    .await
                    .expect_err("a zero timeout over a pending future must elapse")
            })
    }

    fn blocked() -> Option<(String, String)> {
        Some((
            "http://169.254.169.254/".to_owned(),
            "cloud metadata address".to_owned(),
        ))
    }

    #[test]
    fn a_successful_navigation_with_nothing_blocked_is_ok() {
        assert!(resolve_navigation_outcome(Ok(Ok(())), None, TEST_TIMEOUT).is_ok());
    }

    #[test]
    fn a_successful_navigation_wins_even_if_a_subresource_was_blocked() {
        assert!(
            resolve_navigation_outcome(Ok(Ok(())), blocked(), TEST_TIMEOUT).is_ok(),
            "a blocked subresource must not fail a navigation that otherwise succeeded"
        );
    }

    #[test]
    fn a_blocked_request_is_reported_instead_of_the_navigation_error() {
        let navigation = Ok(Err(CrawlError::browser_error("navigation failed: net::ERR_FAILED")));
        let error = resolve_navigation_outcome(navigation, blocked(), TEST_TIMEOUT)
            .expect_err("a blocked request must surface as an error");

        match error {
            CrawlError::SsrfPolicyViolation { url, reason, .. } => {
                assert_eq!(url, "http://169.254.169.254/");
                assert_eq!(reason, "cloud metadata address");
            }
            other => panic!("expected an SSRF policy violation, got: {other:?}"),
        }
    }

    #[test]
    fn a_blocked_request_is_reported_instead_of_the_timeout() {
        let error = resolve_navigation_outcome(Err(elapsed()), blocked(), TEST_TIMEOUT)
            .expect_err("a blocked request must surface as an error");

        assert!(
            matches!(error, CrawlError::SsrfPolicyViolation { .. }),
            "a navigation that timed out because a request was blocked must report the block, got: {error:?}"
        );
    }

    #[test]
    fn a_navigation_error_with_nothing_blocked_is_reported_verbatim() {
        let navigation = Ok(Err(CrawlError::browser_error("navigation failed: boom")));
        let error = resolve_navigation_outcome(navigation, None, TEST_TIMEOUT).expect_err("must be an error");

        assert!(
            error.to_string().contains("navigation failed: boom"),
            "the original navigation error must be preserved, got: {error}"
        );
    }

    #[test]
    fn a_timeout_with_nothing_blocked_reports_the_browser_timeout() {
        let error = resolve_navigation_outcome(Err(elapsed()), None, TEST_TIMEOUT).expect_err("must be an error");

        assert!(
            matches!(error, CrawlError::BrowserTimeout { .. }),
            "expected a browser timeout, got: {error:?}"
        );
        assert!(
            error.to_string().contains("7s"),
            "the timeout message must name the configured timeout, got: {error}"
        );
    }
}
