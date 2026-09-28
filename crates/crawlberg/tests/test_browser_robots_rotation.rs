//! crawlberg#423, browser tier: `BrowserMode::Always`/`Stealth` never send a UA-rotation pick
//! -- the browser always sends the configured (or custom-header) agent -- so the robots
//! decision for a request the browser will fetch must judge that same agent, not a rotated
//! one the browser will never put on the wire.
//!
//! Requires a real Chrome binary (chromiumoxide auto-detects it) and is gated behind the
//! `browser` feature; skipped (not failed) when Chrome is unavailable, matching the other
//! browser tests.

#![cfg(feature = "browser")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use crawlberg::{
    BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig, CrawlError, CrawlResult, crawl, create_engine,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

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

/// `BrowserMode::Auto`, otherwise identical to [`browser_config`]: the Http tier runs first
/// (sending a UA-rotation pick) and escalates to the Browser tier -- the default
/// `EscalationStrategy::BrowserOnly` -- only once the Http tier is blocked.
fn auto_escalation_config() -> CrawlConfig {
    let mut config = browser_config();
    config.browser.mode = BrowserMode::Auto;
    config
}

/// Responds 403 to the first `GET /` (the Http tier's attempt, forcing escalation) and 200 to
/// every later one (what the Browser tier would receive, were it ever dispatched).
fn escalating_root_responder(calls: Arc<AtomicUsize>) -> impl Fn(&Request) -> ResponseTemplate {
    move |_req: &Request| {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            ResponseTemplate::new(403).set_body_string("blocked")
        } else {
            ResponseTemplate::new(200).set_body_raw(b"<html><body>root</body></html>".to_vec(), "text/html")
        }
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

/// crawlberg#423, `BrowserMode::Auto` escalation: the agent robots.txt is judged against is
/// chosen (by UA rotation) before the tier is known, since the Http tier runs first. Once the
/// Http tier is blocked and the default `EscalationStrategy::BrowserOnly` escalates to the
/// Browser tier, the browser sends its own real agent ("RealBrowserAgent"), never the rotated
/// pick judged at admission. A robots.txt group naming the browser's real agent must stop the
/// escalation before the browser ever fetches, not just discard what it returns.
#[tokio::test]
async fn should_block_an_auto_mode_browser_escalation_when_robots_names_the_agent_it_actually_sends() {
    let mock = MockServer::start().await;
    mount_robots(&mock, "User-agent: RealBrowserAgent\nDisallow: /\n").await;
    let calls = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(escalating_root_responder(calls.clone()))
        .mount(&mock)
        .await;

    let Some(result) = crawl_in_browser(
        "should_block_an_auto_mode_browser_escalation_when_robots_names_the_agent_it_actually_sends",
        auto_escalation_config(),
        &mock.uri(),
    )
    .await
    else {
        return;
    };

    assert!(
        result.pages.is_empty(),
        "a robots.txt group naming the agent the escalated browser tier actually sends must block \
         the crawl, even though the Http tier's own agent (judged at admission) was a different, \
         rotated pick"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the browser tier must never dispatch its fetch once robots.txt disallows the agent it would \
         send -- only the Http tier's single (403) attempt may reach the mock"
    );
}

/// Control for the test above: a robots.txt group naming neither agent this crawl ever sends
/// (not the Http tier's rotated pick, admitted before escalation is known, and not the
/// browser's own real agent) must not block the escalation. Distinguishes the new re-judgment
/// from a check that blocks any escalation once robots.txt has any `Disallow` rule at all.
#[tokio::test]
async fn should_allow_an_auto_mode_browser_escalation_when_robots_names_neither_agent_this_crawl_sends() {
    let mock = MockServer::start().await;
    mount_robots(&mock, "User-agent: SomeOtherBot\nDisallow: /\n").await;
    let calls = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(escalating_root_responder(calls.clone()))
        .mount(&mock)
        .await;

    let Some(result) = crawl_in_browser(
        "should_allow_an_auto_mode_browser_escalation_when_robots_names_neither_agent_this_crawl_sends",
        auto_escalation_config(),
        &mock.uri(),
    )
    .await
    else {
        return;
    };

    assert_eq!(
        result.pages.len(),
        1,
        "a robots.txt group naming an agent this crawl never sends, at admission or at escalation, \
         must not block the escalated fetch"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "both the Http tier's blocked attempt and the escalated browser fetch must reach the mock"
    );
}
