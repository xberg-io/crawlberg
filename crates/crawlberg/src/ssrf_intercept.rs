//! CDP Fetch-domain interception that re-validates every browser-issued request
//! against the SSRF policy, closing the gap the pre-navigation seed check leaves
//! open: a browser follows redirects and client-side navigations internally, so
//! without per-request interception a redirect to a private/metadata address
//! would reach the network unchecked.
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

use std::sync::{Arc, Mutex};

use chromiumoxide::cdp::browser_protocol::fetch::{
    ContinueRequestParams, DisableParams as FetchDisableParams, EnableParams as FetchEnableParams, EventRequestPaused,
    FailRequestParams, HeaderEntry,
};
use chromiumoxide::cdp::browser_protocol::network::{ErrorReason, Headers};
use tokio_stream::StreamExt;

use crate::error::CrawlError;
use crate::net::credentials::credential_header;
use crate::net::ssrf::{SsrfPolicy, validate_url};
use crate::net::userinfo;
use crate::types::CrawlConfig;

/// What an intercepted request is recorded as when it does not parse, so its text is never echoed.
const UNPARSEABLE_URL: &str = "(unparseable URL)";

/// Active CDP Fetch-domain interception that re-validates every browser-issued
/// request against the SSRF policy. Held alive across a navigation; consuming it
/// via [`SsrfInterceptGuard::finish`] disables interception, stops the listener,
/// and reports the first request that was blocked.
pub(crate) struct SsrfInterceptGuard {
    page: chromiumoxide::Page,
    listener: tokio::task::JoinHandle<()>,
    blocked: Arc<Mutex<Option<(String, String)>>>,
}

impl SsrfInterceptGuard {
    /// Disable interception, stop the listener, and return the first blocked
    /// `(url, reason)` observed during the navigation, if any.
    pub(crate) async fn finish(self) -> Option<(String, String)> {
        let _ = self.page.execute(FetchDisableParams::default()).await;
        self.listener.abort();
        match self.blocked.lock() {
            Ok(mut slot) => slot.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        }
    }
}

/// Decide whether an intercepted request URL may go out.
///
/// Returns the parsed URL, or `Err((recorded_url, reason))` when the request must be failed
/// at the CDP layer. A URL with userinfo is refused, as the Fetch standard does for
/// subresources, and is recorded without it. This is the per-request decision applied to
/// every browser-issued request.
async fn ssrf_verdict(request_url: &str, policy: &SsrfPolicy) -> Result<url::Url, (String, String)> {
    let parsed = url::Url::parse(request_url).map_err(|e| (UNPARSEABLE_URL.to_owned(), format!("invalid URL: {e}")))?;
    if userinfo::has_userinfo(&parsed) {
        let mut clean = parsed;
        userinfo::strip(&mut clean);
        return Err((clean.into(), "a URL with credentials in it is refused".to_owned()));
    }
    validate_url(&parsed, policy)
        .await
        .map_err(|e| (parsed.to_string(), e.to_string()))?;
    Ok(parsed)
}

/// The request's own headers plus the credential header `url` gets, if it gets one.
///
/// ~keep The header goes on this one request only, never through
/// ~keep `Network.setExtraHTTPHeaders`, which would give it to every host the page loads
/// ~keep from. A redirect hop is paused again and gets its own decision.
fn headers_with_credentials(config: &CrawlConfig, url: &url::Url, headers: &Headers) -> Option<Vec<HeaderEntry>> {
    let (name, value) = credential_header(config, url)?;
    let mut entries: Vec<HeaderEntry> = headers
        .inner()
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(existing, _)| !existing.eq_ignore_ascii_case(&name))
        .filter_map(|(existing, value)| value.as_str().map(|value| HeaderEntry::new(existing.clone(), value)))
        .collect();
    entries.push(HeaderEntry::new(name, value));
    Some(entries)
}

/// Enable CDP Fetch interception on `page`, validating every intercepted request
/// URL against `config.ssrf` before Chrome connects. Requests resolving to blocked
/// addresses (loopback, RFC1918, link-local, cloud metadata, non-http(s)
/// schemes) or carrying userinfo are failed with `BlockedByClient` and the first one is
/// recorded so the caller can surface a precise [`CrawlError::SsrfPolicyViolation`].
/// A request to the seed's host is continued with the caller's credential header.
pub(crate) async fn start_ssrf_interception(
    page: &chromiumoxide::Page,
    config: &CrawlConfig,
) -> Result<SsrfInterceptGuard, CrawlError> {
    let mut events = page
        .event_listener::<EventRequestPaused>()
        .await
        .map_err(|e| CrawlError::browser_error(format!("failed to register intercept listener: {e}")))?;

    page.execute(FetchEnableParams::default())
        .await
        .map_err(|e| CrawlError::browser_error(format!("failed to enable request interception: {e}")))?;

    let blocked: Arc<Mutex<Option<(String, String)>>> = Arc::new(Mutex::new(None));
    let listener_page = page.clone();
    let listener_config = config.clone();
    let listener_blocked = Arc::clone(&blocked);

    let listener = tokio::spawn(async move {
        while let Some(event) = events.next().await {
            let request_id = event.request_id.clone();
            let request_url = event.request.url.clone();

            match ssrf_verdict(&request_url, &listener_config.ssrf).await {
                Ok(parsed) => {
                    let mut params = ContinueRequestParams::new(request_id);
                    params.headers = headers_with_credentials(&listener_config, &parsed, &event.request.headers);
                    let _ = listener_page.execute(params).await;
                }
                Err((recorded_url, reason)) => {
                    if let Ok(mut slot) = listener_blocked.lock()
                        && slot.is_none()
                    {
                        *slot = Some((recorded_url, reason));
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

    #[tokio::test]
    async fn a_url_with_userinfo_is_refused_and_recorded_without_it() {
        let Err((recorded, reason)) = ssrf_verdict("http://user:s3cret@example.com/a", &deny_policy()).await else {
            panic!("a URL with userinfo must be refused");
        };
        assert_eq!(recorded, "http://example.com/a");
        assert!(reason.contains("credentials"), "{reason}");
    }

    #[tokio::test]
    async fn a_malformed_url_is_recorded_without_its_text() {
        let Err((recorded, reason)) = ssrf_verdict("http://user:s3cret@exa mple/", &deny_policy()).await else {
            panic!("a malformed URL must be refused");
        };
        assert_eq!(recorded, "(unparseable URL)");
        assert!(reason.contains("invalid URL"), "{reason}");
    }

    #[test]
    fn the_credential_replaces_a_page_header_of_the_same_name_and_keeps_the_rest() {
        use chromiumoxide::cdp::browser_protocol::network::Headers;

        use super::headers_with_credentials;
        use crate::types::{AuthConfig, CrawlConfig};

        let seed = url::Url::parse("http://example.com/").expect("test URL must parse");
        let config = CrawlConfig {
            auth: Some(AuthConfig::Bearer {
                token: "tok".to_owned(),
            }),
            credential_scope: crate::net::CredentialScope::for_seed(&seed, None),
            ..CrawlConfig::default()
        };
        let headers = Headers::new(serde_json::json!({"Cookie": "a=b", "authorization": "page-value"}));

        let entries = headers_with_credentials(&config, &seed, &headers).expect("the seed host gets the credential");
        let pairs: Vec<(&str, &str)> = entries
            .iter()
            .map(|entry| (entry.name.as_str(), entry.value.as_str()))
            .collect();
        assert!(
            pairs.contains(&("Cookie", "a=b")),
            "the page's headers are kept: {pairs:?}"
        );
        let authorization: Vec<&(&str, &str)> = pairs
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("authorization"))
            .collect();
        assert_eq!(authorization, vec![&("Authorization", "Bearer tok")], "{pairs:?}");

        let other = url::Url::parse("http://other.test/").expect("test URL must parse");
        assert!(headers_with_credentials(&config, &other, &headers).is_none());
    }
}
