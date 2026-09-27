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
    FailRequestParams,
};
use chromiumoxide::cdp::browser_protocol::network::{ErrorReason, ResourceType};
use chromiumoxide::cdp::browser_protocol::page::FrameId;
use tokio_stream::StreamExt;

use crate::error::CrawlError;
use crate::net::ssrf::{SsrfPolicy, validate_url};

/// Active CDP Fetch-domain interception that re-validates every browser-issued
/// request against the SSRF policy. Held alive across a navigation; consuming it
/// via [`SsrfInterceptGuard::finish`] disables interception, stops the listener,
/// and reports the requests that were blocked.
pub(crate) struct SsrfInterceptGuard {
    page: chromiumoxide::Page,
    listener: tokio::task::JoinHandle<()>,
    state: Arc<Mutex<InterceptOutcome>>,
}

/// The requests the SSRF policy blocked during one navigation.
#[derive(Debug, Default)]
pub(crate) struct InterceptOutcome {
    /// The first request the SSRF policy blocked, as `(url, reason)`.
    pub(crate) blocked: Option<(String, String)>,
    /// The first main-frame document request the SSRF policy blocked, as `(url, reason)`.
    /// The page then shows Chrome's error page, not a document from the server.
    pub(crate) blocked_navigation: Option<(String, String)>,
}

impl SsrfInterceptGuard {
    /// Disable interception, stop the listener, and return the requests blocked
    /// during the navigation.
    pub(crate) async fn finish(self) -> InterceptOutcome {
        let _ = self.page.execute(FetchDisableParams::default()).await;
        self.listener.abort();
        match self.state.lock() {
            Ok(mut state) => std::mem::take(&mut *state),
            Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
        }
    }
}

/// Whether a request of `resource_type` from `frame` loads the document of the main frame.
fn is_main_frame_document(frame: &FrameId, resource_type: &ResourceType, main_frame: &FrameId) -> bool {
    *resource_type == ResourceType::Document && main_frame == frame
}

/// The main frame a refused navigation is told apart by, or an error naming why it is unknown.
///
/// ~keep Without it, a refused navigation of the page cannot be told from a refused iframe
/// ~keep document. Counting every document as a navigation would fail a page for a refused iframe,
/// ~keep and counting none would keep Chrome's error page, so an unknown main frame fails instead.
fn require_main_frame(resolved: Result<Option<FrameId>, String>) -> Result<FrameId, CrawlError> {
    match resolved {
        Ok(Some(frame)) => Ok(frame),
        Ok(None) => Err(CrawlError::browser_error(
            "cannot check navigations against the SSRF policy: the page reports no main frame".to_owned(),
        )),
        Err(error) => Err(CrawlError::browser_error(format!(
            "cannot check navigations against the SSRF policy: failed to read the page's main frame: {error}"
        ))),
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
pub(crate) async fn start_ssrf_interception(
    page: &chromiumoxide::Page,
    policy: &SsrfPolicy,
) -> Result<SsrfInterceptGuard, CrawlError> {
    let main_frame = require_main_frame(page.mainframe().await.map_err(|e| e.to_string()))?;

    let mut events = page
        .event_listener::<EventRequestPaused>()
        .await
        .map_err(|e| CrawlError::browser_error(format!("failed to register intercept listener: {e}")))?;

    page.execute(FetchEnableParams::default())
        .await
        .map_err(|e| CrawlError::browser_error(format!("failed to enable request interception: {e}")))?;

    let state = Arc::new(Mutex::new(InterceptOutcome::default()));
    let listener_page = page.clone();
    let listener_policy = policy.clone();
    let listener_state = Arc::clone(&state);

    let listener = tokio::spawn(async move {
        while let Some(event) = events.next().await {
            let request_id = event.request_id.clone();
            let request_url = event.request.url.clone();

            match ssrf_verdict(&request_url, &listener_policy).await {
                Ok(()) => {
                    let _ = listener_page.execute(ContinueRequestParams::new(request_id)).await;
                }
                Err(reason) => {
                    if let Ok(mut state) = listener_state.lock() {
                        if state.blocked_navigation.is_none()
                            && is_main_frame_document(&event.frame_id, &event.resource_type, &main_frame)
                        {
                            state.blocked_navigation = Some((request_url.clone(), reason.clone()));
                        }
                        if state.blocked.is_none() {
                            state.blocked = Some((request_url, reason));
                        }
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

#[cfg(test)]
mod tests {
    //! Unit tests for the per-request SSRF decision applied by browser-tier
    //! Fetch interception. These cover the security-critical verdict (the CDP
    //! plumbing around it is thin glue) and stay hermetic by using literal-IP
    //! and scheme rejections that require no DNS resolution or network.
    use super::{
        FrameId, ResourceType, is_main_frame_document, require_main_frame, ssrf_verdict, start_ssrf_interception,
    };
    use crate::net::ssrf::SsrfPolicy;
    use tokio_stream::StreamExt;

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

    #[test]
    fn a_main_frame_document_is_a_navigation() {
        let main = FrameId::new("MAIN");
        assert!(is_main_frame_document(&main, &ResourceType::Document, &main));
    }

    #[test]
    fn a_subresource_of_the_main_frame_is_not_a_navigation() {
        let main = FrameId::new("MAIN");
        assert!(!is_main_frame_document(&main, &ResourceType::Image, &main));
    }

    #[test]
    fn the_document_of_a_child_frame_is_not_a_navigation() {
        let main = FrameId::new("MAIN");
        assert!(!is_main_frame_document(
            &FrameId::new("CHILD"),
            &ResourceType::Document,
            &main
        ));
    }

    #[test]
    fn a_resolved_main_frame_is_the_frame_navigations_are_told_apart_by() {
        let frame = require_main_frame(Ok(Some(FrameId::new("MAIN")))).expect("a resolved frame must be returned");
        assert_eq!(frame, FrameId::new("MAIN"));
    }

    #[test]
    fn a_page_that_reports_no_main_frame_is_refused() {
        let error = require_main_frame(Ok(None))
            .expect_err("without a main frame every refused document would count as a navigation");
        assert_eq!(
            error.to_string(),
            "browser: cannot check navigations against the SSRF policy: the page reports no main frame"
        );
    }

    #[test]
    fn a_main_frame_that_cannot_be_read_is_refused() {
        let error = require_main_frame(Err("channel closed".to_owned()))
            .expect_err("a failed main-frame read must not count every document as a navigation");
        assert_eq!(
            error.to_string(),
            "browser: cannot check navigations against the SSRF policy: failed to read the page's main frame: \
             channel closed"
        );
    }

    /// Launches a minimal headless Chrome for the interception test below, returning `None`
    /// when this machine has no usable Chrome. `tests/common::is_missing_chrome_message` lives
    /// in a separate compilation unit (each file under `tests/` is its own binary) and is not
    /// reachable from a unit test in `src/`, so this mirrors `browser_pool_tests.rs`'s own
    /// launch-and-skip convention instead. The builder chain below (`no_sandbox`,
    /// `new_headless_mode`, `user_data_dir`, `disable_default_args`) is `browser_pool.rs`'s own
    /// `build_pool_launch_builder`, which is private to that module; this reuses its already
    /// `pub(crate)` `apply_default_args` for the long snap-chromium-safe flag list, rather than
    /// also copying that list here.
    #[allow(
        clippy::print_stderr,
        reason = "test-only skip announcement, matching browser_pool_tests.rs's convention"
    )]
    async fn launch_test_page() -> Option<(
        chromiumoxide::browser::Browser,
        tokio::task::JoinHandle<()>,
        chromiumoxide::Page,
        std::path::PathBuf,
    )> {
        use chromiumoxide::browser::{Browser, BrowserConfig as ChromeBrowserConfig};

        const TEST_NAME: &str = "the_first_refused_navigation_is_kept_over_a_second";

        let user_data_dir = std::env::temp_dir().join(format!(
            "crawlberg-ssrf-intercept-first-wins-test-{}",
            std::process::id()
        ));
        let mut builder = ChromeBrowserConfig::builder()
            .no_sandbox()
            .new_headless_mode()
            .user_data_dir(&user_data_dir)
            .disable_default_args();
        builder = crate::browser_pool::apply_default_args(builder);
        let browser_config = match builder.build() {
            Ok(config) => config,
            Err(error) => {
                eprintln!("skipping {TEST_NAME}: no usable Chrome found: {error}");
                return None;
            }
        };
        let (mut browser, mut handler) = match Browser::launch(browser_config).await {
            Ok(pair) => pair,
            Err(error) => {
                eprintln!("skipping {TEST_NAME}: no usable Chrome found: {error}");
                return None;
            }
        };
        let handler_task = tokio::spawn(async move { while handler.next().await.is_some() {} });
        match browser.new_page("about:blank").await {
            Ok(page) => Some((browser, handler_task, page, user_data_dir)),
            Err(error) => {
                handler_task.abort();
                let _ = browser.close().await;
                eprintln!("skipping {TEST_NAME}: could not open a tab: {error}");
                None
            }
        }
    }

    /// The first refused main-frame navigation is the one the caller learns about, even when a
    /// second refused navigation follows before `finish` is called. Two sequential `goto()` calls
    /// on the one guard, not two navigations racing inside one page load: the ordering the
    /// first-wins check depends on has to be certain for the assertion to mean anything, and two
    /// navigations a page starts itself race each other.
    #[tokio::test]
    async fn the_first_refused_navigation_is_kept_over_a_second() {
        const TEST_NAME: &str = "the_first_refused_navigation_is_kept_over_a_second";
        let Some((mut browser, handler_task, page, user_data_dir)) = launch_test_page().await else {
            return;
        };

        let guard = start_ssrf_interception(&page, &deny_policy())
            .await
            .unwrap_or_else(|error| panic!("{TEST_NAME}: interception must start: {error}"));

        let first = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            page.goto("http://169.254.169.254/first"),
        )
        .await;
        let second =
            tokio::time::timeout(std::time::Duration::from_secs(10), page.goto("http://10.0.0.1/second")).await;

        let outcome = guard.finish().await;
        handler_task.abort();
        let _ = browser.close().await;
        let _ = std::fs::remove_dir_all(&user_data_dir);

        assert!(first.is_ok(), "{TEST_NAME}: the first goto must not hang: {first:?}");
        assert!(second.is_ok(), "{TEST_NAME}: the second goto must not hang: {second:?}");

        let (url, _) = outcome.blocked_navigation.unwrap_or_else(|| {
            panic!("{TEST_NAME}: a refused main-frame navigation must be recorded: no navigation was blocked")
        });
        assert!(
            url.contains("169.254.169.254") && url.contains("/first"),
            "{TEST_NAME}: the first refused navigation must be kept over the second: {url}"
        );
    }

    /// A failed main-frame read must fail `start_ssrf_interception` promptly, at the main-frame
    /// step itself, before `event_listener`/`Fetch.enable` are ever reached. If it read the main
    /// frame after enabling interception instead, a failed read would leave interception on with
    /// no listener to answer it, hanging every request the page makes next.
    ///
    /// `page.mainframe()`, `page.event_listener()` and `page.execute()` (used for `Fetch.enable`)
    /// all route through the one command channel a page holds to Chrome; aborting the browser's
    /// handler task drops that channel's receiver, so every one of those calls fails the same way
    /// once it is reached. That makes the ordering observable from outside: whichever call runs
    /// first is the one whose error text comes back. Reproduces the reviewer's own reorder
    /// mutation (rev365d arm 4, main-frame read moved after `event_listener`/`Fetch.enable`): under
    /// that mutation `event_listener` runs first and fails with a different message, turning this
    /// test red.
    #[tokio::test]
    async fn a_failed_main_frame_read_fails_promptly_before_interception_starts() {
        const TEST_NAME: &str = "a_failed_main_frame_read_fails_promptly_before_interception_starts";
        let Some((mut browser, handler_task, page, user_data_dir)) = launch_test_page().await else {
            return;
        };

        // Stop the task that services this page's command channel. Every command sent through it
        // from here on fails with a channel-closed error, so which call fails first is now
        // observable in the error text alone.
        handler_task.abort();
        tokio::task::yield_now().await;

        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            start_ssrf_interception(&page, &deny_policy()),
        )
        .await;

        let _ = browser.close().await;
        let _ = std::fs::remove_dir_all(&user_data_dir);

        let result =
            outcome.unwrap_or_else(|_| panic!("{TEST_NAME}: a failed main-frame read must not hang, must fail"));
        let error = match result {
            Err(error) => error,
            Ok(_) => {
                panic!("{TEST_NAME}: a page with no working command channel must not resolve to a running interceptor")
            }
        };
        let message = error.to_string();
        assert!(
            message.contains("failed to read the page's main frame"),
            "{TEST_NAME}: the main-frame read must be the first thing that fails, before request \
             interception is ever switched on, so no request is left paused: {message}"
        );
    }
}
