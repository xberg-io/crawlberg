//! CDP Fetch-domain interception that re-validates every browser-issued request
//! against the SSRF policy, closing the gap the pre-navigation seed check leaves
//! open: a browser follows redirects and client-side navigations internally, so
//! without per-request interception a redirect to a private/metadata address
//! would reach the network unchecked. The same interception counts the redirects
//! the main frame follows, so `max_redirects` can bound them.
//!
//! ~keep A top-level module rather than nested under `browser`, so both chromiumoxide
//! ~keep navigation call sites can reach it: `browser::navigation::page_fetch` (scrape/crawl)
//! ~keep and `interact::chromiumoxide::navigate_and_wait` (xberg-io/crawlberg#74). `browser.rs`
//! ~keep is gated on the wider `browser` feature (it pulls in `browser_profile`/
//! ~keep `browser_session_pool`, which are `browser`-gated too), but `interact/chromiumoxide.rs`
//! ~keep is gated on the narrower `browser-chromiumoxide`, so nesting this under `browser`
//! ~keep would make it unreachable from a `browser-chromiumoxide`-only build. This module has
//! ~keep no dependency on anything `browser`-gated, so it is gated on `browser-chromiumoxide`
//! ~keep alone in `lib.rs`, matching both callers' actual requirement.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chromiumoxide::cdp::browser_protocol::fetch::{
    ContinueRequestParams, DisableParams as FetchDisableParams, EnableParams as FetchEnableParams, EventRequestPaused,
    FailRequestParams, RequestPattern, RequestStage,
};
use chromiumoxide::cdp::browser_protocol::network::{ErrorReason, ResourceType};
use chromiumoxide::cdp::browser_protocol::page::FrameId;
use tokio_stream::StreamExt;

use crate::error::CrawlError;
use crate::http::REDIRECT_STATUSES;
use crate::net::ssrf::{SsrfPolicy, validate_url};

/// Active CDP Fetch-domain interception that re-validates every browser-issued
/// request against the SSRF policy. Held alive across a navigation; consuming it
/// via [`SsrfInterceptGuard::finish`] disables interception, stops the listener,
/// and reports what it saw.
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
    /// The redirect response that would have taken the main frame past the redirect limit.
    pub(crate) redirect_stop: Option<RedirectStop>,
    /// Whether the main frame has received a document that is not a redirect. Redirects
    /// after it belong to a navigation the page started itself.
    first_document_arrived: bool,
}

/// A main-frame redirect response that was not followed because the limit was reached.
///
/// ~keep Read by `browser::navigation`, which needs the `browser` feature; a
/// ~keep `browser-chromiumoxide`-only build has just `interact`, which sets no limit.
#[derive(Debug)]
#[cfg_attr(not(feature = "browser"), allow(dead_code))]
pub(crate) struct RedirectStop {
    /// The URL that answered with the redirect.
    pub(crate) url: String,
    pub(crate) status: u16,
    /// Response headers, keyed by lowercase name.
    pub(crate) headers: HashMap<String, Vec<String>>,
}

impl SsrfInterceptGuard {
    /// Disable interception, stop the listener, and return what it observed.
    pub(crate) async fn finish(self) -> InterceptOutcome {
        let _ = self.page.execute(FetchDisableParams::default()).await;
        self.listener.abort();
        match self.state.lock() {
            Ok(mut state) => std::mem::take(&mut *state),
            Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
        }
    }
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
/// With `redirect_limit` set, main-frame document responses are also paused so
/// the HTTP redirects Chrome follows are counted, and the redirect that would
/// exceed the limit is failed before its target is requested.
pub(crate) async fn start_ssrf_interception(
    page: &chromiumoxide::Page,
    policy: &SsrfPolicy,
    redirect_limit: Option<usize>,
) -> Result<SsrfInterceptGuard, CrawlError> {
    let mut events = page
        .event_listener::<EventRequestPaused>()
        .await
        .map_err(|e| CrawlError::browser_error(format!("failed to register intercept listener: {e}")))?;

    let main_frame = match redirect_limit {
        Some(_) => page.mainframe().await.ok().flatten(),
        None => None,
    };

    page.execute(FetchEnableParams {
        patterns: redirect_limit.map(|_| intercept_patterns()),
        handle_auth_requests: None,
    })
    .await
    .map_err(|e| CrawlError::browser_error(format!("failed to enable request interception: {e}")))?;

    let state = Arc::new(Mutex::new(InterceptOutcome::default()));
    let listener_page = page.clone();
    let listener_policy = policy.clone();
    let listener_state = Arc::clone(&state);

    let listener = tokio::spawn(async move {
        while let Some(event) = events.next().await {
            let request_id = event.request_id.clone();

            if let Some(limit) = redirect_limit
                && is_response_stage(&event)
            {
                if redirect_verdict(&event, main_frame.as_ref(), limit, &listener_state) {
                    let _ = listener_page.execute(ContinueRequestParams::new(request_id)).await;
                } else {
                    let _ = listener_page
                        .execute(FailRequestParams::new(request_id, ErrorReason::BlockedByClient))
                        .await;
                }
                continue;
            }

            let request_url = event.request.url.clone();
            match ssrf_verdict(&request_url, &listener_policy).await {
                Ok(()) => {
                    let _ = listener_page.execute(ContinueRequestParams::new(request_id)).await;
                }
                Err(reason) => {
                    if let Ok(mut state) = listener_state.lock()
                        && state.blocked.is_none()
                    {
                        state.blocked = Some((request_url, reason));
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
        state,
    })
}

/// Every request at the request stage, plus document responses so redirects can be counted.
fn intercept_patterns() -> Vec<RequestPattern> {
    vec![
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
    ]
}

/// CDP marks a paused response by setting its status or error reason.
fn is_response_stage(event: &EventRequestPaused) -> bool {
    event.response_status_code.is_some() || event.response_error_reason.is_some()
}

/// Whether a paused document response may proceed. A main-frame redirect of the requested
/// navigation is counted while it is within `limit`; the one past it is recorded and must be
/// failed.
///
/// ~keep The requested navigation ends at the first main-frame response that is not a
/// ~keep redirect. A page's script cannot run before that response arrives, so every
/// ~keep redirect after it belongs to a navigation the page started, and it is not counted.
fn redirect_verdict(
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
    let redirect_status = event
        .response_status_code
        .and_then(|code| u16::try_from(code).ok())
        .filter(|code| REDIRECT_STATUSES.contains(code));
    let Some(status) = redirect_status.filter(|_| headers.iter().any(|h| h.name.eq_ignore_ascii_case("location")))
    else {
        state.first_document_arrived = true;
        return true;
    };

    if state.redirects_followed < limit {
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
    state.redirect_stop = Some(RedirectStop {
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
