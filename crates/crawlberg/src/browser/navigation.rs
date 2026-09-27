//! Navigate a pre-existing CDP page, wait for rendering, and extract the final
//! HTML (plus an optional screenshot). This is the per-page work shared by both
//! the pooled and one-shot chromiumoxide fetch paths in the parent module.

use std::collections::HashMap;
use std::time::Duration;

use chromiumoxide::cdp::browser_protocol::emulation::SetDeviceMetricsOverrideParams;
use chromiumoxide::cdp::browser_protocol::network::{Headers, SetCookieParams, SetExtraHttpHeadersParams};
use chromiumoxide::cdp::browser_protocol::page::{CaptureScreenshotFormat, Frame, GetFrameTreeParams};
use chromiumoxide::page::ScreenshotParams;

use super::BrowserPage;
use super::launch::resolve_default_user_agent;
use crate::error::CrawlError;
use crate::http::{HttpResponse, status_error};
use crate::ssrf_intercept::{DocumentResponse, SsrfInterceptGuard, StoppedResponse, start_ssrf_interception};
use crate::types::{AuthConfig, BrowserWait, CookieInfo, CrawlConfig};

/// Viewport a stealth session presents, chosen to match a common desktop display
/// so the reported metrics are unremarkable.
const STEALTH_VIEWPORT_WIDTH: u32 = 1920;
const STEALTH_VIEWPORT_HEIGHT: u32 = 1080;

/// Status reported for a CDP-rendered page when no main-frame response was intercepted,
/// and the content type reported for every rendered page.
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

    let interceptor = start_ssrf_interception(page, &config.ssrf, config.max_redirects).await?;
    let rendered = render(url, config, page, &interceptor, want_screenshot).await;
    let late = interceptor.finish().await;
    // ~keep A main-frame navigation the policy refused leaves Chrome's error page in place of
    // ~keep the page, so it fails the fetch even when the navigation `goto` waited for succeeded:
    // ~keep the refused one can come during the load or after it, during `extra_wait`. It is
    // ~keep checked before the render's own result, which fails on that same error page.
    if let Some((blocked_url, reason)) = late.blocked_navigation {
        return Err(CrawlError::ssrf_violation(blocked_url, reason));
    }
    rendered
}

/// Navigate `page` to `url` under `interceptor` and read the rendered page.
///
/// ~keep The interception stays on until the HTML is read, so a page that navigates during
/// ~keep `extra_wait` (a challenge page that moves to the real page, for example) reports the
/// ~keep status and headers of the new document. They are those of the main-frame document
/// ~keep committed when they are read. A response Chrome does not commit (a 204, a 2xx download)
/// ~keep leaves the previous document in place, and its status with it. The read is a separate
/// ~keep CDP call from reading the HTML: a navigation that commits between the two calls pairs
/// ~keep them with a different document.
async fn render(
    url: &str,
    config: &CrawlConfig,
    page: &chromiumoxide::Page,
    interceptor: &SsrfInterceptGuard,
    want_screenshot: bool,
) -> Result<BrowserPage, CrawlError> {
    let timeout = config.browser.timeout;
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

    let intercepted = interceptor.navigation_outcome();
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
    let frame = committed_frame(page).await?;
    let document = interceptor.document(frame.loader_id.as_ref());
    if let Some(failed_url) = frame.unreachable_url {
        return error_page_outcome(failed_url, document, intercepted.redirects_followed);
    }
    let (status, headers) = document.map_or_else(
        || (RENDERED_PAGE_STATUS, HashMap::new()),
        |doc| (doc.status, doc.headers),
    );

    // ~keep Chrome follows redirects itself, so the page it landed on is the base its links
    // ~keep resolve against. An unreadable URL falls back to the requested one.
    let final_url = page.url().await.ok().flatten().unwrap_or_else(|| url.to_owned());

    let body_bytes = html.as_bytes().to_vec();
    let screenshot = capture_screenshot(page, config, want_screenshot).await;

    Ok(BrowserPage {
        response: HttpResponse {
            status,
            content_type: RENDERED_PAGE_CONTENT_TYPE.to_owned(),
            body: html,
            body_bytes,
            headers,
            browser_extras: None,
            final_url,
            screenshot,
        },
        redirects: intercepted.redirects_followed,
    })
}

/// The main frame and the document it has committed.
async fn committed_frame(page: &chromiumoxide::Page) -> Result<Frame, CrawlError> {
    page.execute(GetFrameTreeParams::default())
        .await
        .map(|tree| tree.result.frame_tree.frame.clone())
        .map_err(|e| CrawlError::browser_error(format!("failed to read the committed document: {e}")))
}

/// The outcome of a main frame that committed Chrome's error page for `failed_url`, whose
/// response, if one arrived, is `document`.
///
/// ~keep The error page is Chrome's, never the server's content. A status HTTP mode raises as an
/// ~keep error (404, 500, 403, ...) is reported with no body, so it raises the same error, and a
/// ~keep soft 404 or 403 is the same bodiless page HTTP mode reports. For any other status HTTP
/// ~keep mode returns the server's body, which Chrome never rendered, so the fetch fails as a
/// ~keep seed that Chrome answers with its error page fails: with a browser error.
fn error_page_outcome(
    failed_url: String,
    document: Option<DocumentResponse>,
    redirects: usize,
) -> Result<BrowserPage, CrawlError> {
    let shown_url = crate::net::redact_url_credentials(&failed_url);
    let Some(document) = document else {
        return Err(CrawlError::browser_error(format!(
            "Chrome could not load {shown_url} and showed its own error page"
        )));
    };
    if document.status != 403 && status_error(document.status, &failed_url).is_none() {
        return Err(CrawlError::browser_error(format!(
            "{shown_url} answered HTTP {}, and Chrome showed its own error page instead of a document",
            document.status
        )));
    }
    Ok(BrowserPage {
        response: stopped_response(StoppedResponse {
            url: failed_url,
            status: document.status,
            headers: document.headers,
        }),
        redirects,
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
        return Err(CrawlError::ssrf_violation(blocked_url, reason));
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
    fn a_blocked_request_is_reported_without_the_credentials_of_its_url() {
        let navigation = Ok(Err(CrawlError::browser_error("navigation failed: net::ERR_FAILED")));
        let blocked = Some((
            "http://user:s3cretpw@169.254.169.254/latest/".to_owned(),
            "cloud metadata address".to_owned(),
        ));
        let error = resolve_navigation_outcome(navigation, blocked, TEST_TIMEOUT)
            .expect_err("a blocked request must surface as an error");

        match error {
            CrawlError::SsrfPolicyViolation { url, .. } => {
                assert!(
                    !url.contains("s3cretpw") && url.contains("169.254.169.254/latest/"),
                    "{url}"
                );
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
