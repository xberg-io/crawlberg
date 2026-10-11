//! With a browser pool, a session pool and `session_affinity` on, the page a fetch keeps for its
//! site serves the next fetch of that site.
//!
//! Requires a real Chrome binary and the `browser` feature; skipped (not failed) when Chrome
//! is unavailable, matching the other browser tests.

#![cfg(feature = "browser")]

use std::sync::Arc;

use crawlberg::{
    BrowserBackend, BrowserConfig, BrowserMode, BrowserPool, BrowserPoolConfig, BrowserSessionPool, CrawlConfig,
    CrawlError, create_engine, scrape,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::{announce_chrome_skip, is_missing_chrome_message};

/// A site with two pages, `/` and `/next`.
async fn site() -> MockServer {
    let mock = MockServer::start().await;
    for (route, text) in [("/", "first page"), ("/next", "the next page")] {
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!("<!doctype html><html><body><p>{text}</p></body></html>"),
                "text/html; charset=utf-8",
            ))
            .mount(&mock)
            .await;
    }
    mock
}

#[tokio::test(flavor = "multi_thread")]
async fn a_kept_page_serves_the_next_fetch_of_its_site() {
    let test_name = "a_kept_page_serves_the_next_fetch_of_its_site";
    let mock = site().await;
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    let sessions = Arc::new(BrowserSessionPool::new());
    let mut config = CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            mode: BrowserMode::Always,
            ..BrowserConfig::default()
        },
        respect_robots_txt: false,
        ..CrawlConfig::builder().allow_private_networks(true).build()
    };
    config.browser.session_affinity = true;
    config.browser_pool = Some(Arc::clone(&pool));
    config.browser_session_pool = Some(Arc::clone(&sessions));
    let engine = create_engine(Some(config)).expect("engine must build");

    let first = match scrape(&engine, &mock.uri()).await {
        Ok(result) => result,
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
            return;
        }
        Err(error) => panic!("{test_name}: the first fetch must succeed: {error:?}"),
    };
    let kept = sessions.size().await;
    let mut later = Vec::new();
    for _ in 0..3 {
        later.push(scrape(&engine, &format!("{}/next", mock.uri())).await);
    }
    let kept_at_end = sessions.size().await;
    pool.shutdown().await;

    assert!(first.html.contains("first page"), "{test_name}: {}", first.html);
    assert_eq!(
        kept, 1,
        "{test_name}: the session pool must keep the page of the first fetch"
    );
    for (index, result) in later.iter().enumerate() {
        match result {
            Ok(page) => assert!(
                page.html.contains("the next page"),
                "{test_name}: fetch {index} on the kept page: {}",
                page.html
            ),
            Err(error) => panic!("{test_name}: fetch {index} on the kept page must succeed: {error:?}"),
        }
    }
    assert_eq!(kept_at_end, 1, "{test_name}: one site keeps one page");
}
