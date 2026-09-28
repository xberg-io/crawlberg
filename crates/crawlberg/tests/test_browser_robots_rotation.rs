//! crawlberg#423, browser tier: `BrowserMode::Always`/`Stealth` never send a UA-rotation pick
//! -- the browser always sends the configured (or custom-header) agent -- so the robots
//! decision for a request the browser will fetch must judge that same agent, not a rotated
//! one the browser will never put on the wire.
//!
//! Requires a real Chrome binary (chromiumoxide auto-detects it) and is gated behind the
//! `browser` feature; skipped (not failed) when Chrome is unavailable, matching the other
//! browser tests.

#![cfg(feature = "browser")]

use std::time::Duration;

use crawlberg::{
    BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig, CrawlError, CrawlResult, crawl, create_engine,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::{announce_chrome_skip, is_missing_chrome_message};

fn browser_config() -> CrawlConfig {
    CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            mode: BrowserMode::Always,
            timeout: Duration::from_secs(20),
            ..BrowserConfig::default()
        },
        ..CrawlConfig::builder()
            .allow_private_networks(true)
            .respect_robots_txt(true)
            .max_pages(10)
            .request_timeout(Duration::from_secs(20))
            .user_agent("RealBrowserAgent")
            .user_agents(vec!["RotatedBot".to_owned()])
            .build()
    }
}

async fn mount_robots(mock: &MockServer, body: &str) {
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body.to_owned())
                .append_header("content-type", "text/plain"),
        )
        .mount(mock)
        .await;
}

/// Crawl `seed`, or `None` when no usable Chrome exists on this host.
async fn crawl_in_browser(test_name: &str, config: CrawlConfig, seed: &str) -> Option<CrawlResult> {
    let engine = create_engine(Some(config)).expect("engine must build");
    match crawl(&engine, seed).await {
        Ok(result) => Some(result),
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
            None
        }
        Err(e) => panic!("crawl failed: {e}"),
    }
}

/// crawlberg#423: with `BrowserMode::Always` and UA rotation configured, the browser tier
/// still always sends the configured agent ("RealBrowserAgent"), never the rotated pick
/// ("RotatedBot"). A robots.txt group naming the agent the browser actually sends must block
/// the page, even though rotation is configured.
#[tokio::test]
async fn should_block_a_browser_fetch_when_robots_txt_names_the_agent_the_browser_actually_sends() {
    let mock = MockServer::start().await;
    mount_robots(&mock, "User-agent: RealBrowserAgent\nDisallow: /\n").await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(b"<html><body>root</body></html>".to_vec(), "text/html"))
        .expect(0)
        .mount(&mock)
        .await;

    let Some(result) = crawl_in_browser(
        "should_block_a_browser_fetch_when_robots_txt_names_the_agent_the_browser_actually_sends",
        browser_config(),
        &mock.uri(),
    )
    .await
    else {
        return;
    };

    assert!(
        result.pages.is_empty(),
        "a robots.txt group naming the agent the browser tier actually sends must block it, \
         even though UA rotation is configured and would judge a different agent"
    );
}

/// Control for the test above: a robots.txt group naming only the rotated agent the browser
/// never sends must not block the page.
#[tokio::test]
async fn should_allow_a_browser_fetch_when_robots_txt_names_only_the_rotated_agent_never_sent() {
    let mock = MockServer::start().await;
    mount_robots(&mock, "User-agent: RotatedBot\nDisallow: /\n").await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(b"<html><body>root</body></html>".to_vec(), "text/html"))
        .mount(&mock)
        .await;

    let Some(result) = crawl_in_browser(
        "should_allow_a_browser_fetch_when_robots_txt_names_only_the_rotated_agent_never_sent",
        browser_config(),
        &mock.uri(),
    )
    .await
    else {
        return;
    };

    assert_eq!(
        result.pages.len(),
        1,
        "a robots.txt group naming only the agent the browser tier never sends must not block the page"
    );
}
