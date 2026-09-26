//! A seed that answers without a document (204, 304, or a redirect that ends in 204) is
//! reported in browser mode the way HTTP mode reports it, without waiting for the browser
//! timeout.
//!
//! Requires a real Chrome binary and the `browser` feature; skipped (not failed) when Chrome
//! is unavailable, matching the other browser tests.

#![cfg(feature = "browser")]

use std::time::{Duration, Instant};

use crawlberg::{
    BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig, CrawlError, ScrapeResult, create_engine, scrape,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::{announce_chrome_skip, is_missing_chrome_message};

/// The browser timeout of the reported defect. A result well inside it proves no wait.
const BROWSER_TIMEOUT: Duration = Duration::from_secs(20);
const PROMPT: Duration = Duration::from_secs(10);

fn config(mode: BrowserMode, backend: BrowserBackend) -> CrawlConfig {
    CrawlConfig {
        browser: BrowserConfig {
            backend,
            mode,
            timeout: BROWSER_TIMEOUT,
            ..BrowserConfig::default()
        },
        respect_robots_txt: false,
        ..CrawlConfig::builder().allow_private_networks(true).build()
    }
}

/// `/` answers `status`, or 302 to `/end` which answers `status` when `via_redirect`.
async fn site(status: u16, via_redirect: bool) -> MockServer {
    let mock = MockServer::start().await;
    let end = if via_redirect { "/end" } else { "/" };
    if via_redirect {
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(302).append_header("location", "/end"))
            .mount(&mock)
            .await;
    }
    Mock::given(method("GET"))
        .and(path(end))
        .respond_with(ResponseTemplate::new(status))
        .mount(&mock)
        .await;
    mock
}

/// Scrape `url`, timing it, or `None` when no usable Chrome exists on this host.
async fn timed_scrape(test_name: &str, config: CrawlConfig, url: &str) -> Option<(ScrapeResult, Duration)> {
    let engine = create_engine(Some(config)).expect("engine must build");
    let started = Instant::now();
    match scrape(&engine, url).await {
        Ok(result) => Some((result, started.elapsed())),
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
            None
        }
        Err(error) => panic!("{test_name}: scrape must succeed: {error:?}"),
    }
}

fn outcome(result: &ScrapeResult, base: &str) -> (u16, String, String) {
    (
        result.status_code,
        result.final_url.trim_start_matches(base).to_owned(),
        result.html.clone(),
    )
}

async fn assert_browser_matches_http(test_name: &str, status: u16, via_redirect: bool, backend: BrowserBackend) {
    let http_site = site(status, via_redirect).await;
    let (http, _) = timed_scrape(
        test_name,
        config(BrowserMode::Never, backend.clone()),
        &format!("{}/", http_site.uri()),
    )
    .await
    .expect("HTTP mode needs no Chrome");
    let http_outcome = outcome(&http, &http_site.uri());
    let landed = if via_redirect { "/end" } else { "/" };
    assert_eq!(
        http_outcome,
        (status, landed.to_owned(), String::new()),
        "{test_name}: HTTP mode reports the status, landed URL and empty body"
    );

    let browser_site = site(status, via_redirect).await;
    let Some((browser, elapsed)) = timed_scrape(
        test_name,
        config(BrowserMode::Always, backend),
        &format!("{}/", browser_site.uri()),
    )
    .await
    else {
        return;
    };
    assert!(
        elapsed < PROMPT,
        "{test_name}: browser mode must not wait for the {BROWSER_TIMEOUT:?} timeout, took {elapsed:?}"
    );
    assert_eq!(
        outcome(&browser, &browser_site.uri()),
        http_outcome,
        "{test_name}: browser mode must report what HTTP mode reports"
    );
}

#[tokio::test]
async fn a_204_seed_is_reported_without_waiting_for_the_browser_timeout() {
    assert_browser_matches_http("a_204_seed", 204, false, BrowserBackend::Chromiumoxide).await;
}

#[tokio::test]
async fn a_304_seed_is_reported_without_waiting_for_the_browser_timeout() {
    assert_browser_matches_http("a_304_seed", 304, false, BrowserBackend::Chromiumoxide).await;
}

#[tokio::test]
async fn a_redirect_to_a_204_is_reported_without_waiting_for_the_browser_timeout() {
    assert_browser_matches_http("a_redirect_to_a_204", 204, true, BrowserBackend::Chromiumoxide).await;
}

/// Negative control: a 200 whose body arrives late still waits for that body.
#[tokio::test]
async fn a_slow_200_seed_still_waits_for_its_body() {
    let delay = Duration::from_secs(3);
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(r#"<html><body><p id="slow">slow body</p></body></html>"#, "text/html")
                .set_delay(delay),
        )
        .mount(&mock)
        .await;
    let Some((result, elapsed)) = timed_scrape(
        "a_slow_200_seed_still_waits_for_its_body",
        config(BrowserMode::Always, BrowserBackend::Chromiumoxide),
        &format!("{}/", mock.uri()),
    )
    .await
    else {
        return;
    };
    assert!(
        elapsed >= delay,
        "the fetch must wait for the delayed body, took {elapsed:?}"
    );
    assert_eq!(result.status_code, 200);
    assert!(
        result.html.contains("slow body"),
        "the body must be rendered: {}",
        result.html
    );
}

#[cfg(feature = "browser-native")]
mod native {
    use super::*;

    #[tokio::test]
    async fn a_204_seed_is_reported_without_waiting_on_the_native_backend() {
        assert_browser_matches_http("native_204_seed", 204, false, BrowserBackend::Native).await;
    }

    #[tokio::test]
    async fn a_304_seed_is_reported_without_waiting_on_the_native_backend() {
        assert_browser_matches_http("native_304_seed", 304, false, BrowserBackend::Native).await;
    }

    #[tokio::test]
    async fn a_redirect_to_a_204_is_reported_without_waiting_on_the_native_backend() {
        assert_browser_matches_http("native_redirect_to_a_204", 204, true, BrowserBackend::Native).await;
    }
}
