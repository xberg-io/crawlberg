//! CDP Fetch-domain interception that re-validates every browser-issued request
//! against the SSRF policy, closing the gap the pre-navigation seed check leaves
//! open: a browser follows redirects and client-side navigations internally, so
//! without per-request interception a redirect to a private/metadata address
//! would reach the network unchecked. The same interception counts the redirects
//! the main frame follows, so `max_redirects` can bound them.
//!
//! ~keep A top-level module rather than nested under `browser`, so both chromiumoxide
//! ~keep call sites can reach it: `browser::navigation::page_fetch` (scrape/crawl)
//! ~keep and `interact::chromiumoxide::run_with_browser` (xberg-io/crawlberg#74). `browser.rs`
//! ~keep is gated on the wider `browser` feature (it pulls in `browser_profile`/
//! ~keep `browser_session_pool`, which are `browser`-gated too), but `interact/chromiumoxide.rs`
//! ~keep is gated on the narrower `browser-chromiumoxide`, so nesting this under `browser`
//! ~keep would make it unreachable from a `browser-chromiumoxide`-only build. This module has
//! ~keep no dependency on anything `browser`-gated, so it is gated on `browser-chromiumoxide`
//! ~keep alone in `lib.rs`, matching both callers' actual requirement.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chromiumoxide::cdp::browser_protocol::fetch::{
    ContinueRequestParams, DisableParams as FetchDisableParams, EnableParams as FetchEnableParams, EventRequestPaused,
    FailRequestParams, RequestId, RequestPattern, RequestStage,
};
use chromiumoxide::cdp::browser_protocol::network::{ErrorReason, ResourceType};
use chromiumoxide::cdp::browser_protocol::page::FrameId;
use chromiumoxide::listeners::EventStream;
use tokio_stream::StreamExt;

use crate::error::CrawlError;
use crate::http::{NO_DOCUMENT_STATUSES, REDIRECT_STATUSES};
use crate::net::ssrf::{SsrfPolicy, validate_url};

/// Active CDP Fetch-domain interception that re-validates every browser-issued
/// request against the SSRF policy. Held alive across a navigation; consuming it
/// via [`SsrfInterceptGuard::finish`] disables interception, stops the listener,
/// and reports what it saw.
///
/// ~keep Page-level interception serves `browser::navigation` alone, which needs the `browser`
/// ~keep feature; `interact` intercepts on the browser session (see [`BrowserIntercept`]).
#[cfg(feature = "browser")]
pub(crate) struct SsrfInterceptGuard {
    page: chromiumoxide::Page,
    listener: tokio::task::JoinHandle<()>,
    state: Arc<Mutex<InterceptOutcome>>,
}

/// What the interception observed during one navigation.
#[derive(Debug, Default)]
pub(crate) struct InterceptOutcome {
    /// The first request the SSRF policy blocked, as `(url, reason)`.
    pub(crate) blocked: Option<(String, String)>,
    /// HTTP redirects the main frame followed before its first document arrived.
    pub(crate) redirects_followed: usize,
    /// The main-frame response the navigation ends on without a document: the redirect past
    /// the redirect limit, or a response Chrome does not commit (204, 205, 304).
    pub(crate) stopped_response: Option<StoppedResponse>,
    /// Whether the main frame has received a document that is not a redirect. Redirects
    /// after it belong to a navigation the page started itself.
    first_document_arrived: bool,
    /// When the check last paused a request.
    last_request: Option<Instant>,
}

/// A main-frame response the navigation ends on without a document, reported as is.
///
/// ~keep The headers are read by `browser::navigation`, which needs the `browser` feature;
/// ~keep a `browser-chromiumoxide`-only build has just `interact`, which reads the URL and status.
#[derive(Debug)]
#[cfg_attr(not(feature = "browser"), allow(dead_code))]
pub(crate) struct StoppedResponse {
    /// The URL that answered.
    pub(crate) url: String,
    pub(crate) status: u16,
    /// Response headers, keyed by lowercase name.
    pub(crate) headers: HashMap<String, Vec<String>>,
}

#[cfg(feature = "browser")]
impl SsrfInterceptGuard {
    /// Disable interception, stop the listener, and return what it observed.
    pub(crate) async fn finish(self) -> InterceptOutcome {
        let _ = self.page.execute(FetchDisableParams::default()).await;
        self.listener.abort();
        take_outcome(&self.state)
    }
}

/// Browser-wide interception: the SSRF check and the redirect counting of
/// [`start_ssrf_interception`], applied to every request of every target in the browser,
/// so popups, new tabs, out-of-process frames and workers are checked too.
///
/// ~keep CDP Fetch interception is per session. Enabled on a page's session it pauses only
/// ~keep that page's requests, and chromiumoxide attaches a popup's target without pausing it,
/// ~keep so the popup's first request would leave before a page-level interception could be
/// ~keep enabled on it. Enabled on the browser session, it pauses every target's requests.
/// ~keep On a browser reached through `browser.endpoint`, that includes pages other clients
/// ~keep opened, for as long as the session runs.
pub(crate) struct BrowserIntercept {
    state: Arc<Mutex<InterceptOutcome>>,
}

impl BrowserIntercept {
    /// Return what the interception observed so far, and keep it running. Redirects are not
    /// counted again: once the main frame has its first document, later navigations are free
    /// of the redirect limit.
    pub(crate) fn take_outcome(&self) -> InterceptOutcome {
        take_outcome(&self.state)
    }

    /// Wait until the check has paused no request for a short quiet period, so the requests an
    /// action just started are judged before its outcome is read. Bounded by a limit, for a
    /// page that sends requests without pause.
    pub(crate) async fn settle(&self) {
        const QUIET: Duration = Duration::from_millis(100);
        const LIMIT: Duration = Duration::from_secs(1);
        let started = Instant::now();
        loop {
            tokio::time::sleep(QUIET).await;
            let last_request = match self.state.lock() {
                Ok(state) => state.last_request,
                Err(poisoned) => poisoned.into_inner().last_request,
            };
            if last_request.is_none_or(|at| at.elapsed() >= QUIET) || started.elapsed() >= LIMIT {
                return;
            }
        }
    }

    /// Disable interception on the browser session. Call it once the listener is no longer
    /// polled, since a request paused after that point would never be answered.
    pub(crate) async fn finish(self, browser: &chromiumoxide::Browser) {
        let _ = browser.execute(FetchDisableParams::default()).await;
    }
}

fn take_outcome(state: &Mutex<InterceptOutcome>) -> InterceptOutcome {
    let mut state = match state.lock() {
        Ok(state) => state,
        Err(poisoned) => poisoned.into_inner(),
    };
    let first_document_arrived = state.first_document_arrived;
    let outcome = std::mem::take(&mut *state);
    state.first_document_arrived = first_document_arrived;
    outcome
}

/// The CDP session a paused request is answered on: the one it was paused on.
enum Session<'a> {
    #[cfg(feature = "browser")]
    Page(chromiumoxide::Page),
    Browser(&'a chromiumoxide::Browser),
}

impl Session<'_> {
    async fn answer(&self, request_id: RequestId, allow: bool) {
        if allow {
            self.execute(ContinueRequestParams::new(request_id)).await;
        } else {
            self.execute(FailRequestParams::new(request_id, ErrorReason::BlockedByClient))
                .await;
        }
    }

    async fn execute<C: chromiumoxide::types::Command>(&self, command: C) {
        let _ = match self {
            #[cfg(feature = "browser")]
            Session::Page(page) => page.execute(command).await.map(drop),
            Session::Browser(browser) => browser.execute(command).await.map(drop),
        };
    }
}

/// Enable browser-wide interception and return it with the listener that answers the paused
/// requests. The caller polls the listener for as long as the check must hold, and disables
/// the Fetch domain on the browser session after it.
///
/// `page` names the main frame whose redirects are counted against `redirect_limit`.
pub(crate) async fn start_browser_interception<'a>(
    browser: &'a chromiumoxide::Browser,
    page: &chromiumoxide::Page,
    policy: &SsrfPolicy,
    redirect_limit: usize,
) -> Result<(BrowserIntercept, impl Future<Output = ()> + 'a), CrawlError> {
    let events = browser
        .event_listener::<EventRequestPaused>()
        .await
        .map_err(|e| CrawlError::browser_error(format!("failed to register intercept listener: {e}")))?;
    let main_frame = page.mainframe().await.ok().flatten();
    browser
        .execute(fetch_enable_params())
        .await
        .map_err(|e| CrawlError::browser_error(format!("failed to enable request interception: {e}")))?;

    let state = Arc::new(Mutex::new(InterceptOutcome::default()));
    let listener = answer_paused_requests(
        events,
        Session::Browser(browser),
        policy.clone(),
        main_frame,
        redirect_limit,
        Arc::clone(&state),
    );
    Ok((BrowserIntercept { state }, listener))
}

/// Decide whether an intercepted request URL is permitted by the SSRF policy.
/// Returns `Err(reason)` when the request must be failed at the CDP layer. This
/// is the per-request decision applied to every browser-issued request.
async fn ssrf_verdict(request_url: &str, policy: &SsrfPolicy) -> Result<(), String> {
    match url::Url::parse(request_url) {
        Ok(parsed) => validate_url(&parsed, policy).await.map_err(|e| e.to_string()),
        Err(e) => Err(format!("invalid URL: {e}")),
    }
}

/// Enable CDP Fetch interception on `page`, validating every intercepted request
/// URL against `policy` before Chrome connects. Requests resolving to blocked
/// addresses (loopback, RFC1918, link-local, cloud metadata, non-http(s)
/// schemes) are failed with `BlockedByClient` and the first one is recorded so
/// the caller can surface a precise [`CrawlError::SsrfPolicyViolation`].
///
/// Main-frame document responses are also paused so the HTTP redirects Chrome
/// follows are counted, and the redirect that would exceed `redirect_limit` is
/// failed before its target is requested. A response Chrome does
/// not commit is recorded and failed the same way, so the navigation ends at once.
#[cfg(feature = "browser")]
pub(crate) async fn start_ssrf_interception(
    page: &chromiumoxide::Page,
    policy: &SsrfPolicy,
    redirect_limit: usize,
) -> Result<SsrfInterceptGuard, CrawlError> {
    let events = page
        .event_listener::<EventRequestPaused>()
        .await
        .map_err(|e| CrawlError::browser_error(format!("failed to register intercept listener: {e}")))?;

    let main_frame = page.mainframe().await.ok().flatten();

    page.execute(fetch_enable_params())
        .await
        .map_err(|e| CrawlError::browser_error(format!("failed to enable request interception: {e}")))?;

    let state = Arc::new(Mutex::new(InterceptOutcome::default()));
    let listener = tokio::spawn(answer_paused_requests(
        events,
        Session::Page(page.clone()),
        policy.clone(),
        main_frame,
        redirect_limit,
        Arc::clone(&state),
    ));

    Ok(SsrfInterceptGuard {
        page: page.clone(),
        listener,
        state,
    })
}

/// Answer every paused request on `session` until the event stream ends: a request the SSRF
/// policy refuses is failed and the first one recorded; a main-frame document response is
/// judged by [`main_frame_verdict`].
async fn answer_paused_requests(
    mut events: EventStream<EventRequestPaused>,
    session: Session<'_>,
    policy: SsrfPolicy,
    main_frame: Option<FrameId>,
    redirect_limit: usize,
    state: Arc<Mutex<InterceptOutcome>>,
) {
    while let Some(event) = events.next().await {
        let request_id = event.request_id.clone();

        if is_response_stage(&event) {
            let allow = main_frame_verdict(&event, main_frame.as_ref(), redirect_limit, &state);
            session.answer(request_id, allow).await;
            continue;
        }

        let request_url = event.request.url.clone();
        let verdict = ssrf_verdict(&request_url, &policy).await;
        let allow = verdict.is_ok();
        if let Ok(mut state) = state.lock() {
            state.last_request = Some(Instant::now());
            if let Err(reason) = verdict
                && state.blocked.is_none()
            {
                state.blocked = Some((request_url, reason));
            }
        }
        session.answer(request_id, allow).await;
    }
}

/// Every request at the request stage, plus document responses so redirects can be counted.
fn fetch_enable_params() -> FetchEnableParams {
    FetchEnableParams {
        patterns: Some(vec![
            RequestPattern {
                url_pattern: Some("*".to_owned()),
                resource_type: None,
                request_stage: Some(RequestStage::Request),
            },
            RequestPattern {
                url_pattern: Some("*".to_owned()),
                resource_type: Some(ResourceType::Document),
                request_stage: Some(RequestStage::Response),
            },
        ]),
        handle_auth_requests: None,
    }
}

/// CDP marks a paused response by setting its status or error reason.
fn is_response_stage(event: &EventRequestPaused) -> bool {
    event.response_status_code.is_some() || event.response_error_reason.is_some()
}

/// Whether a paused document response may proceed. A main-frame redirect of the requested
/// navigation is counted while it is within `limit`; the one past it is recorded and must be
/// failed. A main-frame response Chrome does not commit is also recorded and failed.
///
/// ~keep The requested navigation ends at the first main-frame response that is not a
/// ~keep redirect. A page's script cannot run before that response arrives, so every
/// ~keep redirect after it belongs to a navigation the page started, and it is not counted.
/// ~keep Chrome commits no document for a 204, 205 or 304, so no load event fires and
/// ~keep chromiumoxide's `goto` waits for the browser timeout. Failing the response makes
/// ~keep Chrome commit its error page, which ends `goto` at once.
fn main_frame_verdict(
    event: &EventRequestPaused,
    main_frame: Option<&FrameId>,
    limit: usize,
    state: &Mutex<InterceptOutcome>,
) -> bool {
    if main_frame.is_some_and(|frame| *frame != event.frame_id) {
        return true;
    }
    let mut state = match state.lock() {
        Ok(state) => state,
        Err(poisoned) => poisoned.into_inner(),
    };
    if state.first_document_arrived {
        return true;
    }

    let headers = event.response_headers.as_deref().unwrap_or_default();
    let status = event.response_status_code.and_then(|code| u16::try_from(code).ok());
    let is_redirect = status.is_some_and(|code| REDIRECT_STATUSES.contains(&code))
        && headers.iter().any(|h| h.name.eq_ignore_ascii_case("location"));
    let Some(status) = status.filter(|code| is_redirect || NO_DOCUMENT_STATUSES.contains(code)) else {
        state.first_document_arrived = true;
        return true;
    };

    if is_redirect && state.redirects_followed < limit {
        state.redirects_followed += 1;
        return true;
    }
    let mut header_map: HashMap<String, Vec<String>> = HashMap::new();
    for header in headers {
        header_map
            .entry(header.name.to_ascii_lowercase())
            .or_default()
            .push(header.value.clone());
    }
    state.stopped_response = Some(StoppedResponse {
        url: event.request.url.clone(),
        status,
        headers: header_map,
    });
    false
}

#[cfg(test)]
mod tests {
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
