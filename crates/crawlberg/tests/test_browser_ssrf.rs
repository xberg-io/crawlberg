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

/// A page whose own navigation is refused must fail render with the SSRF policy error, and
/// Chrome must never reach the refused address. The seed is allowlisted; a server-side redirect
/// carries the navigation to a server on the IPv6 loopback, which the policy refuses. The test
/// watches that server, so it can tell a request refused before Chrome sends it from a response
/// fetched and then discarded by the engine's later check of the landed URL.
#[tokio::test]
async fn render_fails_with_the_ssrf_error_when_its_own_navigation_is_refused() {
    let test_name = "render_fails_with_the_ssrf_error_when_its_own_navigation_is_refused";
    let refused_listener = std::net::TcpListener::bind("[::1]:0")
        .unwrap_or_else(|error| panic!("{test_name}: the refused server must bind the IPv6 loopback: {error}"));
    let refused = MockServer::builder().listener(refused_listener).start().await;
    // ~keep The refused server answers like the metadata service it stands for, so a request that
    // ~keep reaches it loads a page and only the engine's later check of the landed URL refuses it.
    Mock::given(method("GET"))
        .and(path("/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<html><body>metadata</body></html>", "text/html"))
        .mount(&refused)
        .await;
    let refused_url = format!("http://{}/latest", refused.address());
    let seed = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(302).append_header("location", refused_url.as_str()))
        .mount(&seed)
        .await;

    let engine = create_engine(Some(narrow_policy_config())).expect("engine must build");
    let result = scrape(&engine, &seed.uri()).await;

    match result {
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
            return;
        }
        Err(CrawlError::SsrfPolicyViolation { url, reason, .. }) => {
            assert!(
                url.contains("[::1]") && url.contains("/latest"),
                "{test_name}: the error must name the refused address: {url}"
            );
            assert!(
                !reason.is_empty(),
                "{test_name}: the error must carry a reason, got: {reason:?}"
            );
        }
        other => panic!("{test_name}: the refused navigation must fail with the SSRF policy error: {other:?}"),
    }

    let seed_requests = seed
        .received_requests()
        .await
        .expect("wiremock records requests by default");
    assert!(
        !seed_requests.is_empty(),
        "{test_name}: Chrome must have loaded the allowlisted seed that redirects"
    );
    let refused_requests = refused
        .received_requests()
        .await
        .expect("wiremock records requests by default");
    assert!(
        refused_requests.is_empty(),
        "{test_name}: Chrome must never send the refused navigation's request: {:?}",
        refused_requests
            .iter()
            .map(|request| request.url.as_str())
            .collect::<Vec<_>>()
    );

    // ~keep Positive control: the refused server records a request that does reach it, so the
    // ~keep zero above is the refusal and not a server that cannot see requests.
    let mut control = tokio::net::TcpStream::connect(refused.address())
        .await
        .unwrap_or_else(|error| panic!("{test_name}: the control request must connect: {error}"));
    tokio::io::AsyncWriteExt::write_all(
        &mut control,
        b"GET /control HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    )
    .await
    .unwrap_or_else(|error| panic!("{test_name}: the control request must be sent: {error}"));
    let mut response = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut control, &mut response)
        .await
        .unwrap_or_else(|error| panic!("{test_name}: the control response must be read: {error}"));
    let control_requests = refused
        .received_requests()
        .await
        .expect("wiremock records requests by default");
    assert_eq!(
        control_requests.len(),
        1,
        "{test_name}: the refused server must record the control request"
    );
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
