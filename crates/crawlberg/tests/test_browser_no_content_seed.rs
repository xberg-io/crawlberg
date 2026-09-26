//! A seed that answers without a document (204, 304, or a redirect that ends in 204) is
//! reported in browser mode the way HTTP mode reports it, without waiting for the browser
//! timeout.
//!
//! Requires a real Chrome binary and the `browser` feature; skipped (not failed) when Chrome
//! is unavailable, matching the other browser tests.

#![cfg(feature = "browser")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use crawlberg::{
    BrowserBackend, BrowserConfig, BrowserMode, BrowserPool, BrowserPoolConfig, BrowserSessionPool, CrawlConfig,
    CrawlError, ScrapeResult, create_engine, scrape,
};
use wiremock::matchers::{header_exists, method, path};
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

/// A 304 Chrome asked for itself, holding a cache entry to satisfy it, still renders that
/// entry: the cached page is reported, not an empty 304.
///
/// ~keep A GUARD, not evidence for #121: treating 304 as a no-document status could plausibly
/// ~keep have emptied a page Chrome could render from cache, and this pins that it does not.
/// ~keep The reason it does not is that Chrome resolves a revalidation 304 against its cache
/// ~keep entry BEFORE the Fetch response-stage pause sees it, so the interception is handed the
/// ~keep merged 200 and never the 304. A 304 that does reach the interception is therefore one
/// ~keep no cache entry can satisfy, which genuinely carries no document.
/// ~keep The `if-none-match` assertion below is load-bearing: without it the test passes
/// ~keep vacuously whenever Chrome does not revalidate at all, which is what a browser launched
/// ~keep per fetch does, having no cache to reuse.
#[tokio::test]
async fn a_304_chrome_can_serve_from_its_cache_reports_the_cached_page() {
    let test_name = "a_304_chrome_can_serve_from_its_cache_reports_the_cached_page";
    let mock = MockServer::start().await;
    // ~keep Mounted before the 200 because wiremock takes the first mount that matches, and
    // ~keep the 200's matcher would otherwise swallow the conditional request too.
    Mock::given(method("GET"))
        .and(path("/"))
        .and(header_exists("if-none-match"))
        .respond_with(ResponseTemplate::new(304).append_header("etag", "\"v1\""))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("etag", "\"v1\"")
                .append_header("cache-control", "max-age=0, must-revalidate")
                .set_body_raw(
                    r#"<html><body><p id="cached">cached body</p></body></html>"#,
                    "text/html",
                ),
        )
        .mount(&mock)
        .await;

    let mut config = config(BrowserMode::Always, BrowserBackend::Chromiumoxide);
    // ~keep The pool is what gives Chrome a cache to revalidate against: it keeps one Chrome
    // ~keep process across fetches, and the HTTP cache belongs to the process, not the tab.
    // ~keep Dropping `browser_pool` makes the `if-none-match` assertion below fail (verified),
    // ~keep because a browser launched per fetch starts with an empty cache. Session affinity
    // ~keep is not needed for it and is set only to exercise the reused-page path too.
    config.browser.session_affinity = true;
    config.browser_pool = Some(BrowserPool::new(BrowserPoolConfig::default()));
    config.browser_session_pool = Some(Arc::new(BrowserSessionPool::new()));

    let engine = create_engine(Some(config)).expect("engine must build");
    let url = format!("{}/", mock.uri());

    match scrape(&engine, &url).await {
        Ok(first) => assert_eq!(first.status_code, 200, "the first fetch must fill the cache"),
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
            return;
        }
        Err(error) => panic!("{test_name}: the first scrape must succeed: {error:?}"),
    }

    let second = scrape(&engine, &url).await.expect("the revalidated fetch must succeed");

    let conditional_requests = mock
        .received_requests()
        .await
        .expect("request recording is on")
        .iter()
        .filter(|request| request.url.path() == "/" && request.headers.contains_key("if-none-match"))
        .count();
    assert_eq!(
        conditional_requests, 1,
        "Chrome must actually have revalidated, or this test proves nothing"
    );

    assert_eq!(
        second.status_code, 200,
        "a revalidated page must keep the cached status, not report the 304"
    );
    assert!(
        second.html.contains("cached body"),
        "the cached body must be reported, not an empty 304: {:?}",
        second.html
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
