//! Chromiumoxide selector waits poll until a matching element appears instead of performing a
//! single query against the DOM state at the start of the wait.

#![cfg(feature = "browser")]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crawlberg::{
    BrowserBackend, BrowserConfig, BrowserMode, BrowserWait, CrawlConfig, CrawlError, PageAction, create_engine,
    interact, scrape,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

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

struct SelectorGate {
    open: Arc<AtomicBool>,
    observed: Arc<AtomicUsize>,
}

impl Respond for SelectorGate {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        self.observed.fetch_add(1, Ordering::SeqCst);
        let status = if self.open.load(Ordering::SeqCst) { 204 } else { 425 };
        ResponseTemplate::new(status).append_header("cache-control", "no-store")
    }
}

async fn gated_page() -> (MockServer, Arc<AtomicBool>, Arc<AtomicUsize>) {
    let site = MockServer::start().await;
    let gate_open = Arc::new(AtomicBool::new(false));
    let gate_observed = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"<html><body><script>
            window.addEventListener('load', () => {
                const poll = () => fetch('/selector-gate', { cache: 'no-store' }).then(response => {
                    if (response.ok) {
                        document.body.dataset.ready = 'yes';
                    } else {
                        setTimeout(poll, 10);
                    }
                }, () => setTimeout(poll, 10));
                poll();
            });
            </script></body></html>"#,
            "text/html",
        ))
        .mount(&site)
        .await;
    Mock::given(method("GET"))
        .and(path("/selector-gate"))
        .respond_with(SelectorGate {
            open: Arc::clone(&gate_open),
            observed: Arc::clone(&gate_observed),
        })
        .mount(&site)
        .await;
    (site, gate_open, gate_observed)
}

async fn wait_until_gate_is_observed(observed: &AtomicUsize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while observed.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the loaded page must poll the closed selector gate");
}

#[tokio::test(flavor = "multi_thread")]
async fn browser_selector_waits_for_a_delayed_match() {
    let test_name = "browser_selector_waits_for_a_delayed_match";
    let (site, gate_open, gate_observed) = gated_page().await;
    let mut config = config(Duration::from_secs(5));
    config.browser.wait = BrowserWait::Selector;
    config.browser.wait_selector = Some("[data-ready='yes']".to_owned());
    let engine = create_engine(Some(config)).expect("the engine must build");
    let url = site.uri();

    let scrape_page = scrape(&engine, &url);
    tokio::pin!(scrape_page);
    let gate_handshake = wait_until_gate_is_observed(&gate_observed);
    tokio::pin!(gate_handshake);
    let result = tokio::select! {
        result = &mut scrape_page => result,
        () = &mut gate_handshake => {
            gate_open.store(true, Ordering::SeqCst);
            scrape_page.await
        }
    };

    match result {
        Ok(result) => assert!(result.html.contains("data-ready=\"yes\""), "got: {}", result.html),
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
        }
        Err(error) => panic!("{test_name}: the delayed selector must appear: {error:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn interaction_navigation_waits_for_its_configured_selector() {
    let test_name = "interaction_navigation_waits_for_its_configured_selector";
    let (site, gate_open, gate_observed) = gated_page().await;
    let mut config = config(Duration::from_secs(5));
    config.browser.wait = BrowserWait::Selector;
    config.browser.wait_selector = Some("[data-ready='yes']".to_owned());
    let engine = create_engine(Some(config)).expect("the engine must build");

    let interact_page = interact(&engine, &site.uri(), vec![PageAction::Scrape]);
    tokio::pin!(interact_page);
    let gate_handshake = wait_until_gate_is_observed(&gate_observed);
    tokio::pin!(gate_handshake);
    let result = tokio::select! {
        result = &mut interact_page => result,
        () = &mut gate_handshake => {
            gate_open.store(true, Ordering::SeqCst);
            interact_page.await
        }
    };

    let result = match result {
        Ok(result) => result,
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
            return;
        }
        Err(error) => panic!("{test_name}: navigation must find its configured selector: {error:?}"),
    };
    assert_eq!(result.action_results.len(), 1);
    assert!(
        result.action_results[0].success,
        "the scrape action must succeed: {:?}",
        result.action_results
    );
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
