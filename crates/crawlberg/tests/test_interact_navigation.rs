//! `interact` navigates under the same rules as `scrape`: `max_redirects` bounds the redirects
//! Chrome follows, and a seed that answers without a document returns at once. Where the
//! navigation ends on a response with no document, `interact` reports the URL `scrape` reports,
//! an empty page, and one failed result per action.
//!
//! Requires a real Chrome binary; skipped (not failed) when Chrome is unavailable, matching the
//! other browser tests.

#![cfg(feature = "browser")]

use std::time::{Duration, Instant};

use crawlberg::{
    BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig, CrawlError, HostMatcher, InteractionResult, PageAction,
    create_engine, interact, scrape,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::{announce_chrome_skip, is_missing_chrome_message};

/// The browser timeout of the reported defect. A result well inside it proves no wait.
const BROWSER_TIMEOUT: Duration = Duration::from_secs(20);
const PROMPT: Duration = Duration::from_secs(10);

fn config(mode: BrowserMode, max_redirects: usize) -> CrawlConfig {
    CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            mode,
            timeout: BROWSER_TIMEOUT,
            ..BrowserConfig::default()
        },
        max_redirects,
        respect_robots_txt: false,
        ..CrawlConfig::builder().allow_private_networks(true).build()
    }
}

async fn mount_page(mock: &MockServer, route: &str) {
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(r#"<html><body><p id="landed">landed</p></body></html>"#, "text/html"),
        )
        .mount(mock)
        .await;
}

/// `/` answers 301 to `/r1`, `/r1` to `/r2`, and so on for `hops` redirects; the last hop
/// lands on a 200 page.
async fn redirect_chain(hops: usize) -> MockServer {
    let mock = MockServer::start().await;
    for hop in 0..hops {
        let from = if hop == 0 { "/".to_owned() } else { format!("/r{hop}") };
        Mock::given(method("GET"))
            .and(path(from))
            .respond_with(ResponseTemplate::new(301).append_header("location", format!("/r{}", hop + 1)))
            .mount(&mock)
            .await;
    }
    mount_page(&mock, &format!("/r{hops}")).await;
    mock
}

/// `/` answers `status` with no body.
async fn no_document_site(status: u16) -> MockServer {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(status))
        .mount(&mock)
        .await;
    mock
}

fn actions() -> Vec<PageAction> {
    vec![
        PageAction::ExecuteJs {
            script: "return document.title".to_owned(),
        },
        PageAction::Scrape,
    ]
}

/// Interact with `url`, timing it, or `None` when no usable Chrome exists on this host.
async fn timed_interact(test_name: &str, config: CrawlConfig, url: &str) -> Option<(InteractionResult, Duration)> {
    let engine = create_engine(Some(config)).expect("engine must build");
    let started = Instant::now();
    match interact(&engine, url, actions()).await {
        Ok(result) => Some((result, started.elapsed())),
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
            None
        }
        Err(error) => panic!("{test_name}: interact must succeed: {error:?}"),
    }
}

/// The URL and body HTTP-mode `scrape` reports for `url`, relative to `base`.
async fn scrape_outcome(max_redirects: usize, url: &str, base: &str) -> (String, String) {
    let engine = create_engine(Some(config(BrowserMode::Never, max_redirects))).expect("engine must build");
    let result = scrape(&engine, url).await.expect("HTTP mode needs no Chrome");
    (result.final_url.trim_start_matches(base).to_owned(), result.html)
}

async fn requested_paths(mock: &MockServer) -> Vec<String> {
    mock.received_requests()
        .await
        .expect("request recording is on")
        .iter()
        .map(|request| request.url.path().to_owned())
        .collect()
}

/// Assert `result` is the empty page a response without a document gives: the URL `scrape`
/// reports, no HTML, and a failed result per action naming `status`.
fn assert_no_document(
    test_name: &str,
    result: &InteractionResult,
    base: &str,
    expected: &(String, String),
    status: u16,
) {
    assert_eq!(
        (
            result.final_url.trim_start_matches(base).to_owned(),
            result.final_html.clone()
        ),
        *expected,
        "{test_name}: interact must report the URL and body scrape reports"
    );
    assert_eq!(
        result.action_results.len(),
        actions().len(),
        "{test_name}: one result per action"
    );
    for action in &result.action_results {
        assert!(
            !action.success,
            "{test_name}: no action runs without a document: {action:?}"
        );
        let error = action.error.as_deref().unwrap_or_default();
        assert!(
            error.contains(&status.to_string()),
            "{test_name}: the action error must name the status {status}: {error}"
        );
    }
}

#[tokio::test]
async fn interact_stops_a_chain_longer_than_max_redirects_where_scrape_does() {
    let test_name = "interact_stops_a_chain_longer_than_max_redirects_where_scrape_does";
    let http_site = redirect_chain(5).await;
    let expected = scrape_outcome(2, &format!("{}/", http_site.uri()), &http_site.uri()).await;
    assert_eq!(
        expected,
        ("/r2".to_owned(), String::new()),
        "scrape stops on the 3xx at the limit"
    );

    let site = redirect_chain(5).await;
    let Some((result, _)) =
        timed_interact(test_name, config(BrowserMode::Always, 2), &format!("{}/", site.uri())).await
    else {
        return;
    };
    assert_no_document(test_name, &result, &site.uri(), &expected, 301);
    let requested = requested_paths(&site).await;
    assert!(
        !requested.iter().any(|p| p == "/r3"),
        "the redirect past the limit must not be requested: {requested:?}"
    );
}

#[tokio::test]
async fn interact_follows_a_chain_exactly_at_max_redirects() {
    let test_name = "interact_follows_a_chain_exactly_at_max_redirects";
    let site = redirect_chain(2).await;
    let Some((result, _)) =
        timed_interact(test_name, config(BrowserMode::Always, 2), &format!("{}/", site.uri())).await
    else {
        return;
    };
    assert_eq!(result.final_url.trim_start_matches(&site.uri()), "/r2");
    assert!(
        result.final_html.contains("landed"),
        "the page at the end of the chain must load: {}",
        result.final_html
    );
    assert!(
        result.action_results.iter().all(|action| action.success),
        "every action must run on the landed page: {:?}",
        result.action_results
    );
}

async fn assert_no_document_seed(test_name: &str, status: u16) {
    let http_site = no_document_site(status).await;
    let expected = scrape_outcome(10, &format!("{}/", http_site.uri()), &http_site.uri()).await;
    assert_eq!(
        expected,
        ("/".to_owned(), String::new()),
        "scrape reports the seed with no body"
    );

    let site = no_document_site(status).await;
    let Some((result, elapsed)) =
        timed_interact(test_name, config(BrowserMode::Always, 10), &format!("{}/", site.uri())).await
    else {
        return;
    };
    assert!(
        elapsed < PROMPT,
        "{test_name}: interact must not wait for the {BROWSER_TIMEOUT:?} timeout, took {elapsed:?}"
    );
    assert_no_document(test_name, &result, &site.uri(), &expected, status);
}

#[tokio::test]
async fn interact_returns_a_204_seed_without_waiting_for_the_browser_timeout() {
    assert_no_document_seed("interact_204_seed", 204).await;
}

#[tokio::test]
async fn interact_returns_a_304_seed_without_waiting_for_the_browser_timeout() {
    assert_no_document_seed("interact_304_seed", 304).await;
}

/// Control: a normal page still loads and every action runs on it.
#[tokio::test]
async fn interact_runs_actions_on_a_normal_page() {
    let test_name = "interact_runs_actions_on_a_normal_page";
    let site = MockServer::start().await;
    mount_page(&site, "/").await;
    let Some((result, _)) =
        timed_interact(test_name, config(BrowserMode::Always, 10), &format!("{}/", site.uri())).await
    else {
        return;
    };
    assert_eq!(result.final_url.trim_start_matches(&site.uri()), "/");
    assert!(result.final_html.contains("landed"), "{}", result.final_html);
    assert!(
        result.action_results.iter().all(|action| action.success),
        "{:?}",
        result.action_results
    );
}

/// A redirect to a blocked address is still refused by the SSRF check while redirects are
/// counted. Only the seed's loopback address is allowlisted; private addresses stay denied.
#[tokio::test]
async fn interact_still_refuses_a_redirect_to_a_blocked_address() {
    let test_name = "interact_still_refuses_a_redirect_to_a_blocked_address";
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(302).append_header("location", "http://169.254.169.254/latest/meta-data/"))
        .mount(&site)
        .await;
    let config = CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            mode: BrowserMode::Always,
            timeout: BROWSER_TIMEOUT,
            ..BrowserConfig::default()
        },
        respect_robots_txt: false,
        ..CrawlConfig::builder()
            .ssrf_allowlist_host(HostMatcher::cidr("127.0.0.1/32").expect("valid CIDR"))
            .build()
    };
    let engine = create_engine(Some(config)).expect("engine must build");
    match interact(&engine, &format!("{}/", site.uri()), actions()).await {
        Err(CrawlError::SsrfPolicyViolation { url, .. }) => {
            assert!(url.starts_with("http://169.254.169.254/"), "blocked URL: {url}");
        }
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
        }
        other => panic!("{test_name}: the redirect target must be refused, got {other:?}"),
    }
}
