//! Navigate a pre-existing CDP page, wait for rendering, and extract the final
//! HTML (plus an optional screenshot). This is the per-page work shared by both
//! the pooled and one-shot chromiumoxide fetch paths in the parent module.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chromiumoxide::cdp::browser_protocol::emulation::SetDeviceMetricsOverrideParams;
use chromiumoxide::cdp::browser_protocol::network::{
    EventResponseReceived, EventResponseReceivedExtraInfo, Headers, RequestId, ResourceType, SetCookieParams,
    SetExtraHttpHeadersParams,
};
use chromiumoxide::cdp::browser_protocol::page::{CaptureScreenshotFormat, FrameId};
use chromiumoxide::page::ScreenshotParams;
use tokio_stream::StreamExt;

use super::launch::resolve_default_user_agent;
use crate::error::CrawlError;
use crate::http::HttpResponse;
use crate::ssrf_intercept::start_ssrf_interception;
use crate::types::{AuthConfig, BrowserWait, CookieInfo, CrawlConfig};

/// Viewport a stealth session presents, chosen to match a common desktop display
/// so the reported metrics are unremarkable.
const STEALTH_VIEWPORT_WIDTH: u32 = 1920;
const STEALTH_VIEWPORT_HEIGHT: u32 = 1080;

/// Status and content type reported when the navigation produced no observable main-frame
/// document response at all — an `about:` or `data:` URL, or a response the Network domain
/// never reported. A real response supersedes both; see [`reported_metadata`].
const RENDERED_PAGE_STATUS: u16 = 200;
const RENDERED_PAGE_CONTENT_TYPE: &str = "text/html";

/// How many times to ask the page for its HTML, and how long to wait between attempts.
const CONTENT_ATTEMPTS: u32 = 3;
const CONTENT_RETRY_DELAY: Duration = Duration::from_millis(100);

/// How many `responseReceivedExtraInfo` statuses to hold while waiting to learn which request
/// carried the document.
///
/// ~keep That event names no resource type, so the document's request cannot be recognised until
/// ~keep the matching `responseReceived` arrives afterwards; until then every request's status has
/// ~keep to be kept. The document is among a navigation's first requests, so a small cap suffices
/// ~keep and stops a page with thousands of subresources growing this map without bound.
const MAX_TRACKED_WIRE_STATUSES: usize = 64;

/// Navigate a pre-existing CDP page to `url`, wait for rendering, and extract
/// the final HTML. The caller provides the page; this function does not
/// create or close it.
pub(super) async fn page_fetch(
    url: &str,
    config: &CrawlConfig,
    page: &chromiumoxide::Page,
    prior_cookies: Option<&[CookieInfo]>,
    want_screenshot: bool,
) -> Result<HttpResponse, CrawlError> {
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

    let interceptor = start_ssrf_interception(page, &config.ssrf).await?;
    let document_capture = start_document_response_capture(page).await?;

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

    let blocked = interceptor.finish().await;
    resolve_navigation_outcome(navigation, blocked, timeout)?;

    if let Some(extra) = config.browser.extra_wait {
        tokio::time::sleep(extra).await;
    }

    let html = extract_html(page).await?;

    // ~keep Chrome follows redirects itself, so the page it landed on is the base its links
    // ~keep resolve against. An unreadable URL falls back to the requested one.
    let final_url = page.url().await.ok().flatten().unwrap_or_else(|| url.to_owned());

    let body_bytes = html.as_bytes().to_vec();
    let screenshot = capture_screenshot(page, config, want_screenshot).await;

    // ~keep Read after the HTML rather than before `extra_wait`: a navigation that commits inside
    // ~keep that window replaces the document, and the status has to describe the markup
    // ~keep serialised above, not whatever the first navigation happened to land on.
    let (status, content_type, headers) = reported_metadata(document_capture.take_document());

    Ok(HttpResponse {
        status,
        content_type,
        body: html,
        body_bytes,
        headers,
        browser_extras: None,
        final_url,
        screenshot,
    })
}

/// Extract the page's HTML, re-asking when the execution context is replaced mid-call.
///
/// ~keep `page.content()` runs `Runtime.evaluate` against an execution context id chromiumoxide
/// ~keep pinned when the page loaded. A navigation starting after load — a meta refresh, a
/// ~keep client-side router, anything inside `extra_wait` — destroys that context and the call
/// ~keep comes back "Cannot find context with specified id" (crawlberg#170). The replacement
/// ~keep document commits within milliseconds, so simply asking again resolves against it.
///
/// ~keep Retries on any error rather than matching Chrome's wording, which would silently stop
/// ~keep working when Chrome rewords it: `content()` is a pure read with no side effects, so a
/// ~keep retry is always safe, the cost on a genuinely fatal error is bounded, and the last error
/// ~keep is still reported verbatim. `DOM.getDocument` plus `DOM.getOuterHTML` would need no
/// ~keep execution context at all, but it serialises through Chrome's DOM agent instead of
/// ~keep `XMLSerializer` + `documentElement.outerHTML` and would change the extracted bytes of
/// ~keep every rendered page — a far wider change than this bug warrants.
async fn extract_html(page: &chromiumoxide::Page) -> Result<String, CrawlError> {
    let mut last_error = None;
    for attempt in 0..CONTENT_ATTEMPTS {
        match page.content().await {
            Ok(html) => return Ok(html),
            Err(e) => {
                tracing::debug!(
                    attempt = attempt + 1,
                    error = %e,
                    "failed to extract page HTML; the execution context may have been replaced"
                );
                last_error = Some(e);
            }
        }
        if attempt + 1 < CONTENT_ATTEMPTS {
            tokio::time::sleep(CONTENT_RETRY_DELAY).await;
        }
    }
    let error = last_error.expect("every exhausted attempt recorded its error");
    Err(CrawlError::browser_error(format!("failed to extract HTML: {error}")))
}

/// Status, content type and headers the server actually sent for the main-frame document.
struct DocumentResponse {
    status: u16,
    /// The document's own `content-type` header, else the mime type Chrome determined. Empty
    /// when neither said anything.
    content_type: String,
    /// Response headers, keyed by lowercase name, as [`HttpResponse::headers`] expects.
    headers: HashMap<String, Vec<String>>,
}

/// What the network listener has seen so far for one navigation.
#[derive(Default)]
struct DocumentObservation {
    /// The most recent main-frame document response, with the request it arrived under.
    document: Option<(RequestId, DocumentResponse)>,
    /// Statuses reported by `responseReceivedExtraInfo`, keyed by request.
    ///
    /// ~keep Held separately because for a revalidated cache entry this is the only place the
    /// ~keep real 304 appears: `responseReceived` reports the 200 Chrome served from its cache,
    /// ~keep and CDP documents this field as the correct status in exactly that case. That 304,
    /// ~keep and the `ETag` alongside it, is what crawlberg#148 loses.
    wire_statuses: HashMap<RequestId, u16>,
}

impl DocumentObservation {
    /// The document response for the navigation, preferring the status that went over the wire.
    fn resolve(mut self) -> Option<DocumentResponse> {
        let (request_id, mut document) = self.document.take()?;
        if let Some(wire_status) = self.wire_statuses.remove(&request_id) {
            document.status = wire_status;
        }
        Some(document)
    }
}

/// Records the main-frame document response of one navigation, so a rendered page can report
/// the status, content type and headers the server actually sent.
struct DocumentResponseGuard {
    listener: tokio::task::JoinHandle<()>,
    observed: Arc<Mutex<DocumentObservation>>,
}

impl DocumentResponseGuard {
    /// Take the main-frame document response observed so far, when there is one.
    fn take_document(&self) -> Option<DocumentResponse> {
        let mut guard = match self.observed.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        std::mem::take(&mut *guard).resolve()
    }
}

impl Drop for DocumentResponseGuard {
    /// ~keep Stops the listener on every exit from `page_fetch`, including the navigation-error
    /// ~keep and timeout paths that return before the document is ever read.
    fn drop(&mut self) {
        self.listener.abort();
    }
}

/// Start recording the main-frame document response for the next navigation on `page`.
///
/// ~keep Enables nothing: chromiumoxide's own `NetworkManager` sends `Network.enable` in its
/// ~keep page-init command chain, so these events already arrive. Registering a listener does not
/// ~keep intercept or delay anything, so this cannot alter the navigation it observes.
async fn start_document_response_capture(page: &chromiumoxide::Page) -> Result<DocumentResponseGuard, CrawlError> {
    let main_frame = page
        .mainframe()
        .await
        .map_err(|e| CrawlError::browser_error(format!("failed to resolve the main frame: {e}")))?;

    let mut responses = page
        .event_listener::<EventResponseReceived>()
        .await
        .map_err(|e| CrawlError::browser_error(format!("failed to register a response listener: {e}")))?;
    let mut extra_info = page
        .event_listener::<EventResponseReceivedExtraInfo>()
        .await
        .map_err(|e| CrawlError::browser_error(format!("failed to register a response listener: {e}")))?;

    let observed = Arc::new(Mutex::new(DocumentObservation::default()));
    let listener_observed = Arc::clone(&observed);

    let listener = tokio::spawn(async move {
        loop {
            tokio::select! {
                Some(event) = responses.next() => {
                    record_document_response(&event, main_frame.as_ref(), &listener_observed);
                }
                Some(event) = extra_info.next() => record_wire_status(&event, &listener_observed),
                else => break,
            }
        }
    });

    Ok(DocumentResponseGuard { listener, observed })
}

/// Record a response as this navigation's document when it is the main frame's document.
fn record_document_response(
    event: &EventResponseReceived,
    main_frame: Option<&FrameId>,
    observed: &Mutex<DocumentObservation>,
) {
    if event.r#type != ResourceType::Document {
        return;
    }
    // ~keep An iframe's document must never be mistaken for the page's.
    let attributed_to_main_frame = match (main_frame, event.frame_id.as_ref()) {
        (Some(main_frame), Some(frame_id)) => {
            if main_frame != frame_id {
                return;
            }
            true
        }
        // ~keep Chrome named no frame, or the main frame was unknown before the navigation began,
        // ~keep so this response cannot be attributed either way.
        _ => false,
    };
    let Ok(status) = u16::try_from(event.response.status) else {
        return;
    };
    let headers = header_map(&event.response.headers);
    let content_type = headers
        .get("content-type")
        .and_then(|values| values.first())
        .cloned()
        .unwrap_or_else(|| event.response.mime_type.clone());

    if let Ok(mut guard) = observed.lock() {
        // ~keep An unattributed response only fills an empty slot. A navigation's first document
        // ~keep response is the main frame's and its subframes load after it, so first-wins cannot
        // ~keep mistake an iframe for the page; an attributed response always wins, so a
        // ~keep client-side navigation that replaces the document still updates the status.
        if attributed_to_main_frame || guard.document.is_none() {
            guard.document = Some((
                event.request_id.clone(),
                DocumentResponse {
                    status,
                    content_type,
                    headers,
                },
            ));
        }
    }
}

/// Record the status `responseReceivedExtraInfo` reports for a request.
fn record_wire_status(event: &EventResponseReceivedExtraInfo, observed: &Mutex<DocumentObservation>) {
    let Ok(status) = u16::try_from(event.status_code) else {
        return;
    };
    if let Ok(mut guard) = observed.lock()
        && guard.wire_statuses.len() < MAX_TRACKED_WIRE_STATUSES
    {
        guard.wire_statuses.insert(event.request_id.clone(), status);
    }
}

/// Convert CDP response headers into the lowercase-keyed multi-value map `HttpResponse` uses.
///
/// ~keep CDP joins headers that repeated on the wire into one newline-separated value, so
/// ~keep splitting on `\n` is what recovers the individual values a `Set-Cookie` pair arrived as.
fn header_map(headers: &Headers) -> HashMap<String, Vec<String>> {
    let Some(fields) = headers.inner().as_object() else {
        return HashMap::new();
    };
    fields
        .iter()
        .filter_map(|(name, value)| {
            let value = value.as_str()?;
            Some((
                name.to_ascii_lowercase(),
                value.split('\n').map(str::to_owned).collect::<Vec<_>>(),
            ))
        })
        .collect()
}

/// The status, content type and headers to report for a rendered page.
///
/// ~keep The fallbacks apply only when the navigation produced no observable document response.
/// ~keep Reporting them as though they were the server's answer — a 200 for a page that 404ed,
/// ~keep `text/html` for a PDF, no headers at all — is what crawlberg#166 was.
fn reported_metadata(document: Option<DocumentResponse>) -> (u16, String, HashMap<String, Vec<String>>) {
    let Some(document) = document else {
        return (
            RENDERED_PAGE_STATUS,
            RENDERED_PAGE_CONTENT_TYPE.to_owned(),
            HashMap::new(),
        );
    };
    let content_type = if document.content_type.is_empty() {
        RENDERED_PAGE_CONTENT_TYPE.to_owned()
    } else {
        document.content_type
    };
    (document.status, content_type, document.headers)
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

    const DOCUMENT_REQUEST: &str = "request-document";

    fn document(status: u16, content_type: &str) -> DocumentResponse {
        DocumentResponse {
            status,
            content_type: content_type.to_owned(),
            headers: HashMap::from([("etag".to_owned(), vec!["\"v1\"".to_owned()])]),
        }
    }

    fn observation(status: u16, wire_statuses: &[(&str, u16)]) -> DocumentObservation {
        DocumentObservation {
            document: Some((RequestId::new(DOCUMENT_REQUEST), document(status, "text/html"))),
            wire_statuses: wire_statuses
                .iter()
                .map(|(request, status)| (RequestId::new(*request), *status))
                .collect(),
        }
    }

    #[test]
    fn a_rendered_page_reports_the_status_the_server_sent() {
        let (status, _, _) = reported_metadata(Some(document(404, "text/html")));
        assert_eq!(status, 404, "a rendered 404 must not be reported as a 200");
    }

    #[test]
    fn a_rendered_page_reports_the_content_type_the_server_sent() {
        let (_, content_type, _) = reported_metadata(Some(document(200, "application/pdf")));
        assert_eq!(
            content_type, "application/pdf",
            "a rendered page must not claim text/html for a body that is not"
        );
    }

    #[test]
    fn a_rendered_page_reports_the_headers_the_server_sent() {
        let (_, _, headers) = reported_metadata(Some(document(200, "text/html")));
        let etag = headers.get("etag").expect("the document's ETag must be reported");
        assert_eq!(etag.as_slice(), ["\"v1\""], "the ETag value must arrive intact");
    }

    #[test]
    fn the_wire_status_supersedes_the_status_reported_for_a_response_served_from_cache() {
        let resolved = observation(200, &[(DOCUMENT_REQUEST, 304)])
            .resolve()
            .expect("a document was observed");
        assert_eq!(
            resolved.status, 304,
            "a revalidated response reports 200 on responseReceived and 304 only on the extra-info \
             event, which is the status the server actually sent"
        );
    }

    #[test]
    fn a_wire_status_belonging_to_another_request_is_ignored() {
        let resolved = observation(200, &[("request-subresource", 304)])
            .resolve()
            .expect("a document was observed");
        assert_eq!(
            resolved.status, 200,
            "a subresource's status must never be attributed to the document"
        );
    }

    #[test]
    fn a_document_with_no_extra_info_event_keeps_its_reported_status() {
        let resolved = observation(500, &[]).resolve().expect("a document was observed");
        assert_eq!(
            resolved.status, 500,
            "the reported status stands when nothing supersedes it"
        );
    }

    #[test]
    fn header_map_lowercases_names_and_splits_values_that_repeated_on_the_wire() {
        let headers = Headers::new(serde_json::json!({
            "ETag": "\"v1\"",
            "Set-Cookie": "a=1\nb=2",
        }));
        let mapped = header_map(&headers);

        let etag = mapped
            .get("etag")
            .expect("a header name must be reachable by its lowercase form");
        assert_eq!(etag.as_slice(), ["\"v1\""], "a single value must not be split");
        let cookies = mapped.get("set-cookie").expect("set-cookie must be present");
        assert_eq!(
            cookies.as_slice(),
            ["a=1", "b=2"],
            "CDP joins repeated headers with a newline; each value must be recovered separately"
        );
    }

    #[test]
    fn an_unobserved_document_falls_back_to_the_rendered_page_defaults() {
        // ~keep A guard, not a regression test: this is the behaviour every rendered page had
        // ~keep before the real response was captured, and it must survive for a navigation that
        // ~keep genuinely has no HTTP response to report, such as an `about:` or `data:` URL.
        let (status, content_type, headers) = reported_metadata(None);

        assert_eq!(status, RENDERED_PAGE_STATUS);
        assert_eq!(content_type, RENDERED_PAGE_CONTENT_TYPE);
        assert!(headers.is_empty(), "no response means no headers to report");
    }

    #[test]
    fn a_document_with_no_content_type_falls_back_to_the_rendered_page_default() {
        // ~keep A guard: the fallback is unchanged for a response that named no content type,
        // ~keep so only a response that did name one changes what callers see.
        let (_, content_type, _) = reported_metadata(Some(document(200, "")));
        assert_eq!(content_type, RENDERED_PAGE_CONTENT_TYPE);
    }

    #[test]
    fn headers_that_are_not_a_json_object_are_reported_as_empty() {
        // ~keep A guard: `Headers` wraps an untyped JSON value, so a malformed payload must yield
        // ~keep an empty map rather than panicking inside a navigation.
        assert!(header_map(&Headers::new(serde_json::json!("not an object"))).is_empty());
    }
}
