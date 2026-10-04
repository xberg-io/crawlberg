//! Chromiumoxide selector waits poll until a matching element appears instead of performing a
//! single query against the DOM state at the start of the wait.

#![cfg(feature = "browser")]

use std::time::{Duration, Instant};

use crawlberg::{
    BrowserBackend, BrowserConfig, BrowserMode, BrowserWait, CrawlConfig, CrawlError, PageAction, create_engine,
    interact, scrape,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::{announce_chrome_skip, is_missing_chrome_message};

fn config(timeout: Duration) -> CrawlConfig {
    CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            mode: BrowserMode::Always,
            timeout,
            ..BrowserConfig::default()
        },
        respect_robots_txt: false,
        ..CrawlConfig::builder().allow_private_networks(true).build()
    }
}

async fn page(body: &str) -> MockServer {
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/html"))
        .mount(&site)
        .await;
    site
}

#[tokio::test(flavor = "multi_thread")]
async fn browser_selector_waits_for_a_delayed_match() {
    let test_name = "browser_selector_waits_for_a_delayed_match";
    let site = page(
        "<html><body><script>setTimeout(() => { document.body.dataset.ready = 'yes'; }, 100);</script></body></html>",
    )
    .await;
    let mut config = config(Duration::from_secs(5));
    config.browser.wait = BrowserWait::Selector;
    config.browser.wait_selector = Some("[data-ready='yes']".to_owned());
    let engine = create_engine(Some(config)).expect("the engine must build");

    match scrape(&engine, &site.uri()).await {
        Ok(result) => assert!(result.html.contains("data-ready=\"yes\""), "got: {}", result.html),
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
        }
        Err(error) => panic!("{test_name}: the delayed selector must appear: {error:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn browser_selector_does_not_pass_when_the_match_never_appears() {
    let test_name = "browser_selector_does_not_pass_when_the_match_never_appears";
    let site = page("<html><body><p>never ready</p></body></html>").await;
    let timeout = Duration::from_millis(300);
    let mut config = config(timeout);
    config.browser.wait = BrowserWait::Selector;
    config.browser.wait_selector = Some("[data-ready='yes']".to_owned());
    let engine = create_engine(Some(config)).expect("the engine must build");
    let started = Instant::now();

    match scrape(&engine, &site.uri()).await {
        Err(CrawlError::BrowserTimeout { .. }) => assert!(
            started.elapsed() >= timeout,
            "the absent selector must consume its wait budget"
        ),
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
        }
        outcome => panic!("{test_name}: the absent selector must time out, got {outcome:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn browser_selector_propagates_an_invalid_selector_error() {
    let test_name = "browser_selector_propagates_an_invalid_selector_error";
    let site = page("<html><body><p>ready</p></body></html>").await;
    let mut config = config(Duration::from_secs(5));
    config.browser.wait = BrowserWait::Selector;
    config.browser.wait_selector = Some("[".to_owned());
    let engine = create_engine(Some(config)).expect("the engine must build");

    match scrape(&engine, &site.uri()).await {
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
        }
        Err(CrawlError::BrowserError { message, .. }) => {
            assert!(
                message.contains("wait failed"),
                "the selector error must retain its context: {message}"
            );
        }
        outcome => panic!("{test_name}: an invalid selector must fail immediately, got {outcome:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn interaction_selector_waits_for_a_delayed_match() {
    let test_name = "interaction_selector_waits_for_a_delayed_match";
    let site = page("<html><body><p>start</p></body></html>").await;
    let engine = create_engine(Some(config(Duration::from_secs(5)))).expect("the engine must build");
    let actions = vec![
        PageAction::ExecuteJs {
            script: "setTimeout(() => { document.body.dataset.ready = 'yes'; }, 100); return true;".to_owned(),
        },
        PageAction::Wait {
            milliseconds: None,
            selector: Some("[data-ready='yes']".to_owned()),
        },
    ];

    let result = match interact(&engine, &site.uri(), actions).await {
        Ok(result) => result,
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
            return;
        }
        Err(error) => panic!("{test_name}: the interaction must load: {error:?}"),
    };
    assert_eq!(result.action_results.len(), 2);
    assert!(
        result.action_results.iter().all(|action| action.success),
        "both actions must succeed: {:?}",
        result.action_results
    );
    assert!(result.final_html.contains("data-ready=\"yes\""));
}
