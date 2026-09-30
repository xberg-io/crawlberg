//! The one-shot fetch starts its SSRF check for the browser it launched. Requires a real Chrome;
//! skipped (not failed) when none is found.

use std::time::Duration;

use crate::error::CrawlError;
use crate::ssrf_intercept::{SendingSite, with_session_page_left_open};

/// How long the stopped check keeps the browser up before the teardown.
const HOLD: Duration = Duration::from_secs(1);

/// A one-shot fetch in a Chrome it launched with a throwaway profile keeps refusing its page
/// while the page is still sending after the check stops.
///
/// ~keep The page's context is left in place at the watch's end, as when Chrome fails the
/// ~keep dispose, and the browser stays up for `HOLD` after the stop. A stop that turns
/// ~keep interception off, as it does for a browser that is closed rather than killed, lets the
/// ~keep page's requests out in that time (xberg-io/crawlberg#468).
#[allow(
    clippy::print_stderr,
    reason = "test-only skip announcement, matching tests/common/mod.rs's convention"
)]
#[tokio::test(flavor = "multi_thread")]
async fn a_one_shot_fetch_keeps_refusing_a_page_still_sending_after_its_check_stops() {
    let test_name = "a_one_shot_fetch_keeps_refusing_a_page_still_sending_after_its_check_stops";
    let site = SendingSite::start().await;
    let (result, stop_hold) =
        with_session_page_left_open(HOLD, super::one_shot_fetch(&site.seed, &site.config, None, false)).await;
    match result {
        Ok(_) | Err(CrawlError::SsrfPolicyViolation { .. }) => {}
        Err(CrawlError::BrowserError { message, .. })
            if message.contains("failed to launch") || message.contains("chrome executable") =>
        {
            eprintln!("skipping {test_name}: no usable Chrome: {message}");
            return;
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
    let hits = site.denied_hits().await;
    assert_eq!(
        hits, 0,
        "{test_name}: a page still sending after the check stopped must not reach the denied address, \
         got {hits} requests"
    );
}
