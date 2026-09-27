//! Chrome-level render tests for the per-request SSRF policy (`ssrf_intercept.rs`, reached
//! through `browser/navigation.rs`'s `page_fetch`), not merely the pre-navigation seed check.
//! xberg-io/crawlberg#384.
//!
//! Requires a real Chrome binary (chromiumoxide auto-detects it) and is gated behind the
//! `browser` feature; skipped (not failed) when Chrome is unavailable, matching the other
//! browser tests.

#![cfg(feature = "browser")]

use std::time::Duration;

use crawlberg::{
    BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig, CrawlError, HostMatcher, SsrfPolicy, create_engine, scrape,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::{announce_chrome_skip, is_missing_chrome_message};

/// A narrow SSRF policy: the mock server's own loopback address is allowlisted, every other
/// private address stays refused (the default policy's `deny_private` is untouched).
fn narrow_policy_config() -> CrawlConfig {
    CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            mode: BrowserMode::Always,
            timeout: Duration::from_secs(15),
            ..BrowserConfig::default()
        },
        ssrf: SsrfPolicy {
            allowlist: vec![HostMatcher::cidr("127.0.0.0/8").expect("literal CIDR is valid")],
            ..SsrfPolicy::default()
        },
        ..CrawlConfig::builder().build()
    }
}

/// A page whose own navigation is refused must fail render with the SSRF policy error, naming
/// the refused address. The seed itself is allowlisted; a server-side redirect carries the
/// navigation to the refused address, so this exercises `ssrf_intercept.rs`'s per-request Fetch
/// interception (the `blocked_navigation` field `browser/navigation.rs:69` reads), not the
/// upfront seed check `validate_url` already covers on its own.
#[tokio::test]
async fn render_fails_with_the_ssrf_error_when_its_own_navigation_is_refused() {
    let test_name = "render_fails_with_the_ssrf_error_when_its_own_navigation_is_refused";
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(302).append_header("location", "http://169.254.169.254/latest"))
        .mount(&mock)
        .await;

    let engine = create_engine(Some(narrow_policy_config())).expect("engine must build");
    let result = scrape(&engine, &mock.uri()).await;

    match result {
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
        }
        Err(CrawlError::SsrfPolicyViolation { url, reason, .. }) => {
            assert!(
                url.contains("169.254.169.254"),
                "{test_name}: the error must name the refused address: {url}"
            );
            assert!(
                !reason.is_empty(),
                "{test_name}: the error must carry a reason, got: {reason:?}"
            );
        }
        other => panic!("{test_name}: the refused navigation must fail with the SSRF policy error: {other:?}"),
    }
}

/// A page with only a refused image must still render successfully: only a refused main-frame
/// navigation fails the page, the same `blocked` vs `blocked_navigation` split `ssrf_intercept.rs`
/// keeps for `interact()`.
#[tokio::test]
async fn render_succeeds_with_only_a_refused_image() {
    let test_name = "render_succeeds_with_only_a_refused_image";
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "<html><body><p>start page</p><img src=\"http://169.254.169.254/img.png\"></body></html>",
            "text/html",
        ))
        .mount(&mock)
        .await;

    let engine = create_engine(Some(narrow_policy_config())).expect("engine must build");
    let result = scrape(&engine, &mock.uri()).await;

    match result {
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
        }
        Ok(page) => {
            assert!(
                page.html.contains("start page"),
                "{test_name}: a refused image must not fail the page: {}",
                page.html
            );
        }
        other => panic!("{test_name}: a refused image must not fail the page: {other:?}"),
    }
}
