//! The one-shot fetch starts its SSRF check for the browser it launched. Requires a real Chrome;
//! skipped (not failed) when none is found.

use std::time::Duration;

use crate::error::CrawlError;
use crate::ssrf_intercept::{SendingSite, SessionRequestDefense, with_session_page_left_open};

/// How long the stopped check keeps the browser up before the teardown.
const HOLD: Duration = Duration::from_secs(1);

#[allow(
    clippy::print_stderr,
    reason = "test-only skip announcement, matching tests/common/mod.rs's convention"
)]
async fn session_defense_hits(defense: SessionRequestDefense, test_name: &str) -> Option<usize> {
    let site = SendingSite::start().await;
    let (result, stop_hold) = with_session_page_left_open(
        HOLD,
        defense,
        super::one_shot_fetch(&site.seed, &site.config, None, false),
    )
    .await;
    // ~keep A deadline is an ended fetch, not a failure of this teardown property; the forced
    // ~keep post-stop window and denied-server count below remain the pass/fail signal (#570).
    match result {
        Ok(_) | Err(CrawlError::SsrfPolicyViolation { .. } | CrawlError::BrowserTimeout { .. }) => {}
        Err(CrawlError::BrowserError { message, .. })
            if message.contains("failed to launch") || message.contains("chrome executable") =>
        {
            eprintln!("skipping {test_name}: no usable Chrome: {message}");
            return None;
        }
        Err(error) => panic!("{test_name}: the fetch must end: {error:?}"),
    }
    // ~keep The teardown runs in the background after the fetch returns: wait for the stop's
    // ~keep hold to begin, then for it and the kill to end.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while stop_hold.open_pages().is_empty() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(HOLD + Duration::from_secs(1)).await;

    assert!(
        stop_hold.open_pages().iter().any(|url| url.starts_with(&site.seed)),
        "{test_name}: the page must still be open after the stop, or the test proves nothing, open: {:?}",
        stop_hold.open_pages()
    );
    Some(site.denied_hits().await)
}

async fn assert_session_defense(defense: SessionRequestDefense, test_name: &str) {
    let Some(hits) = session_defense_hits(defense, test_name).await else {
        return;
    };
    assert_eq!(
        hits, 0,
        "{test_name}: a page still sending after the check stopped must not reach the denied address, \
         got {hits} requests"
    );
}

/// Fetch interception alone refuses a page that keeps sending after its check stops.
///
/// ~keep The hook removes the socket-level SSRF proxy, leaves the page context in place, and
/// ~keep holds Chrome open after the stop. Disabling interception therefore makes this fail
/// ~keep without relying on host load (xberg-io/crawlberg#570).
#[tokio::test(flavor = "multi_thread")]
async fn fetch_interception_refuses_a_page_still_sending_after_its_check_stops() {
    assert_session_defense(
        SessionRequestDefense::Interception,
        "fetch_interception_refuses_a_page_still_sending_after_its_check_stops",
    )
    .await;
}

/// The socket-level SSRF proxy alone refuses a page that keeps sending after its check stops.
///
/// ~keep The hook turns Fetch interception off at the stop, leaves the page context in place,
/// ~keep and holds Chrome open. Bypassing the proxy therefore makes this fail without relying
/// ~keep on host load (xberg-io/crawlberg#570).
#[tokio::test(flavor = "multi_thread")]
async fn egress_proxy_refuses_a_page_still_sending_after_its_check_stops() {
    assert_session_defense(
        SessionRequestDefense::Egress,
        "egress_proxy_refuses_a_page_still_sending_after_its_check_stops",
    )
    .await;
}

/// The forced window reaches the denied server when neither SSRF defense is active.
///
/// ~keep This is the negative control for both isolated defense tests: it uses the same page,
/// ~keep open-context hook and post-stop hold, changing only the two defenses. A zero count here
/// ~keep means their assertions can pass without exercising the leak window (xberg-io/crawlberg#570).
#[tokio::test(flavor = "multi_thread")]
async fn page_reaches_the_denied_server_without_either_session_defense() {
    let test_name = "page_reaches_the_denied_server_without_either_session_defense";
    let Some(hits) = session_defense_hits(SessionRequestDefense::Neither, test_name).await else {
        return;
    };
    assert!(
        hits > 0,
        "{test_name}: the negative control must reach the denied address or the defense checks prove nothing"
    );
}
