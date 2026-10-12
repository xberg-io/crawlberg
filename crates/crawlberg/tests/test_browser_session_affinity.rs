//! With a browser pool, a session pool and `session_affinity` on, the page a fetch keeps for its
//! site serves the next fetch of that site.
//!
//! Requires a real Chrome binary and the `browser` feature; skipped (not failed) when Chrome
//! is unavailable, matching the other browser tests.

#![cfg(feature = "browser")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use crawlberg::{
    BrowserBackend, BrowserConfig, BrowserMode, BrowserPool, BrowserPoolConfig, BrowserSessionPool, CrawlConfig,
    CrawlEngine, CrawlError, create_engine, scrape,
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

/// A browser-mode config with `session_affinity` on that takes its pages from `pool` and keeps
/// them in `sessions`.
fn affinity_config(pool: &Arc<BrowserPool>, sessions: &Arc<BrowserSessionPool>) -> CrawlConfig {
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
    config.browser_pool = Some(Arc::clone(pool));
    config.browser_session_pool = Some(Arc::clone(sessions));
    config
}

/// The fetch that keeps a new page returns when its page is parked. Chrome 155 makes targets of
/// its own in each new browser context and does not destroy them on a close command; the park
/// waited 5 s for them.
#[tokio::test(flavor = "multi_thread")]
async fn the_first_fetch_that_keeps_its_page_returns_in_under_two_seconds() {
    let test_name = "the_first_fetch_that_keeps_its_page_returns_in_under_two_seconds";
    let mock = site().await;
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    let sessions = Arc::new(BrowserSessionPool::new());
    let engine = create_engine(Some(affinity_config(&pool, &sessions))).expect("engine must build");

    let started = Instant::now();
    let first = scrape(&engine, &mock.uri()).await;
    let elapsed = started.elapsed();
    let kept = sessions.size().await;
    sessions.shutdown().await;
    pool.shutdown().await;

    let first = match first {
        Ok(result) => result,
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
            return;
        }
        Err(error) => panic!("{test_name}: the first fetch must succeed: {error:?}"),
    };
    assert!(first.html.contains("first page"), "{test_name}: {}", first.html);
    assert_eq!(kept, 1, "{test_name}: the session pool must keep the page of the fetch");
    assert!(
        elapsed < Duration::from_secs(2),
        "{test_name}: the first fetch took {} ms",
        elapsed.as_millis()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_kept_page_serves_the_next_fetch_of_its_site() {
    let test_name = "a_kept_page_serves_the_next_fetch_of_its_site";
    let mock = site().await;
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    let sessions = Arc::new(BrowserSessionPool::new());
    let engine = create_engine(Some(affinity_config(&pool, &sessions))).expect("engine must build");

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
    sessions.shutdown().await;
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

#[tokio::test(flavor = "multi_thread")]
async fn should_scrape_with_a_browser_pool_and_default_session_settings() {
    let mock = site().await;
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    let config = pool_only_config(&pool);
    assert!(config.browser.session_affinity);
    assert!(config.browser_session_pool.is_none());
    let engine = create_engine(Some(config)).expect("engine must build");
    let result = scrape(&engine, &mock.uri()).await;
    pool.shutdown().await;
    let page = result.expect("default affinity must not require a session pool");
    assert_eq!(page.status_code, 200);
    assert!(page.html.contains("first page"));
}

#[tokio::test(flavor = "multi_thread")]
async fn should_crawl_with_a_browser_pool_and_default_session_settings() {
    let mock = site().await;
    Mock::given(method("GET"))
        .and(path("/seed"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(r#"<a href="/">first</a><a href="/next">next</a>"#, "text/html"),
        )
        .mount(&mock)
        .await;
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    let engine = CrawlEngine::builder()
        .config(pool_only_config(&pool))
        .build()
        .expect("engine must build");
    let result = engine.crawl(&format!("{}/seed", mock.uri())).await;
    pool.shutdown().await;
    let result = result.expect("crawl");
    assert_eq!(result.error, None);
    assert_eq!(result.pages.len(), 3);
    assert!(result.pages.iter().all(|page| page.status_code == 200));
}

fn pool_only_config(pool: &Arc<BrowserPool>) -> CrawlConfig {
    CrawlConfig {
        browser: BrowserConfig {
            mode: BrowserMode::Always,
            ..Default::default()
        },
        browser_pool: Some(Arc::clone(pool)),
        respect_robots_txt: false,
        max_depth: Some(1),
        ..CrawlConfig::builder().allow_private_networks(true).build()
    }
}
