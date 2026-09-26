//! The SSRF policy covers every request a browser-mode scrape makes, for the whole fetch: a
//! popup the page opens on load, a request it sends during the extra wait, and requests it
//! sends while it is read and screenshotted are refused when their address is denied. This
//! holds on the one-shot browser and on a shared `BrowserPool`, also with several pages at once.
//!
//! The seed is served on `localhost`, which the policy allowlists by name. The denied target
//! is the literal address `127.0.0.1` on a second server, which `deny_private` refuses. Both
//! servers listen on the loopback interface, so the denied server counts every request Chrome
//! sends it.
//!
//! Requires a real Chrome binary; skipped (not failed) when Chrome is unavailable, matching the
//! other browser tests.

#![cfg(feature = "browser")]

use std::sync::Arc;
use std::time::Duration;

use crawlberg::{
    BrowserBackend, BrowserConfig, BrowserMode, BrowserPool, BrowserPoolConfig, CrawlConfig, CrawlError, HostMatcher,
    ScrapeResult, create_engine, scrape,
};
use wiremock::matchers::{any, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::{announce_chrome_skip, is_missing_chrome_message};

const SECRET_MARKER: &str = "denied-marker";

fn config() -> CrawlConfig {
    CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            mode: BrowserMode::Always,
            timeout: Duration::from_secs(20),
            ..BrowserConfig::default()
        },
        respect_robots_txt: false,
        ..CrawlConfig::builder()
            .ssrf_allowlist_host(HostMatcher::exact("localhost"))
            .build()
    }
}

/// Fetch through `pool`, one page per fetch.
fn pooled_config(pool: &Arc<BrowserPool>) -> CrawlConfig {
    let mut config = CrawlConfig {
        browser_pool: Some(Arc::clone(pool)),
        ..config()
    };
    config.browser.session_affinity = false;
    config
}

fn html(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(format!("<html><body>{body}</body></html>"), "text/html")
}

/// The server the policy denies. It answers every request with the secret marker.
async fn denied_server() -> MockServer {
    let denied = MockServer::start().await;
    Mock::given(any())
        .respond_with(html(SECRET_MARKER))
        .mount(&denied)
        .await;
    denied
}

/// The denied URL, addressed by the literal loopback IP the policy refuses.
fn denied_url(denied: &MockServer) -> String {
    format!("http://127.0.0.1:{}/secret", denied.address().port())
}

/// The seed site, reached as `localhost`: `/` carries `body`, `/allowed` is a page the policy
/// permits.
async fn seed_site(body: &str) -> (MockServer, String) {
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(html(body))
        .mount(&site)
        .await;
    Mock::given(method("GET"))
        .and(path("/allowed"))
        .respond_with(html("allowed"))
        .mount(&site)
        .await;
    let seed = format!("http://localhost:{}/", site.address().port());
    (site, seed)
}

/// Scrape `seed` with `config`, or `None` without Chrome.
async fn run(test_name: &str, seed: &str, config: CrawlConfig) -> Option<ScrapeResult> {
    let engine = create_engine(Some(config)).expect("engine must build");
    match scrape(&engine, seed).await {
        Ok(result) => Some(result),
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
            None
        }
        Err(error) => panic!("{test_name}: scrape must succeed: {error:?}"),
    }
}

/// Assert the denied server received nothing, after a pause long enough for a late request.
async fn assert_refused(test_name: &str, denied: &MockServer) {
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let received = denied.received_requests().await.expect("request recording is on");
    assert!(
        received.is_empty(),
        "{test_name}: the denied address must receive no request, got {:?}",
        received
            .iter()
            .map(|request| format!("{} {}", request.method, request.url))
            .collect::<Vec<_>>()
    );
}

fn popup_on_load(denied: &MockServer) -> String {
    format!("<p>start</p><script>window.open({:?});</script>", denied_url(denied))
}

/// A page that sends a request to `target` `delay_ms` after it loads.
fn fetch_after(target: &str, delay_ms: u64) -> String {
    format!(
        "<p>start</p><script>setTimeout(() => fetch({target:?}, {{ mode: 'no-cors' }}).catch(() => {{}}), {delay_ms});</script>"
    )
}

#[tokio::test]
async fn scrape_refuses_a_popup_opened_on_load() {
    let test_name = "scrape_refuses_a_popup_opened_on_load";
    let denied = denied_server().await;
    let (_site, seed) = seed_site(&popup_on_load(&denied)).await;
    if run(test_name, &seed, config()).await.is_none() {
        return;
    }
    assert_refused(test_name, &denied).await;
}

#[tokio::test]
async fn pooled_scrape_refuses_a_popup_opened_on_load() {
    let test_name = "pooled_scrape_refuses_a_popup_opened_on_load";
    let denied = denied_server().await;
    let (_site, seed) = seed_site(&popup_on_load(&denied)).await;
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    let config = pooled_config(&pool);
    let result = run(test_name, &seed, config).await;
    if result.is_some() {
        assert_refused(test_name, &denied).await;
    }
    pool.shutdown().await;
}

/// A popup outlives the page that opened it. On a pooled browser, which stays up after the
/// fetch, the popups are closed with the page, so a request a popup sends later never leaves.
#[tokio::test]
async fn pooled_scrape_closes_the_popups_the_page_opened() {
    let test_name = "pooled_scrape_closes_the_popups_the_page_opened";
    let denied = denied_server().await;
    let (site, seed) = seed_site(r#"<p>start</p><script>window.open("/popup");</script>"#).await;
    Mock::given(method("GET"))
        .and(path("/popup"))
        .respond_with(html(&fetch_after(&denied_url(&denied), 1500)))
        .mount(&site)
        .await;
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    let result = run(test_name, &seed, pooled_config(&pool)).await;
    if result.is_some() {
        tokio::time::sleep(Duration::from_millis(1000)).await;
        assert_refused(test_name, &denied).await;
    }
    pool.shutdown().await;
}

#[tokio::test]
async fn scrape_refuses_a_fetch_during_the_extra_wait() {
    let test_name = "scrape_refuses_a_fetch_during_the_extra_wait";
    let denied = denied_server().await;
    let (_site, seed) = seed_site(&fetch_after(&denied_url(&denied), 700)).await;
    let mut config = config();
    config.browser.extra_wait = Some(Duration::from_millis(1500));
    if run(test_name, &seed, config).await.is_none() {
        return;
    }
    assert_refused(test_name, &denied).await;
}

/// Requests sent once the page has settled, while the scrape reads the page and takes its
/// screenshot, with no extra wait.
#[tokio::test]
async fn scrape_refuses_requests_while_the_page_is_read_and_screenshotted() {
    let test_name = "scrape_refuses_requests_while_the_page_is_read_and_screenshotted";
    let denied = denied_server().await;
    let body = format!(
        "<p>start</p><script>setTimeout(() => setInterval(() => fetch({:?}, {{ mode: 'no-cors' }}).catch(() => {{}}), 5), 400);</script>",
        denied_url(&denied)
    );
    let (_site, seed) = seed_site(&body).await;
    let mut config = config();
    config.capture_screenshot = true;
    let Some(result) = run(test_name, &seed, config).await else {
        return;
    };
    assert!(result.screenshot.is_some(), "{test_name}: the screenshot must be taken");
    assert_refused(test_name, &denied).await;
}

/// Several scrapes share one pooled browser at once. Each page's late request is refused, and
/// every scrape still returns its page.
#[tokio::test]
async fn concurrent_pooled_scrapes_all_keep_the_check() {
    let test_name = "concurrent_pooled_scrapes_all_keep_the_check";
    let denied = denied_server().await;
    let (_site, seed) = seed_site(&fetch_after(&denied_url(&denied), 700)).await;
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    let mut config = pooled_config(&pool);
    config.browser.extra_wait = Some(Duration::from_millis(1500));
    let engine = create_engine(Some(config)).expect("engine must build");
    let results = futures::future::join_all((0..3).map(|_| scrape(&engine, &seed))).await;
    let mut skipped = false;
    for result in results {
        match result {
            Ok(page) => assert!(page.html.contains("start"), "{test_name}: {}", page.html),
            Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
                announce_chrome_skip(test_name, &message);
                skipped = true;
            }
            Err(error) => panic!("{test_name}: every scrape must succeed: {error:?}"),
        }
    }
    if !skipped {
        assert_refused(test_name, &denied).await;
    }
    pool.shutdown().await;
}

/// Control: a late request to an address the policy permits still reaches it.
#[tokio::test]
async fn scrape_still_sends_a_late_request_to_an_allowed_address() {
    let test_name = "scrape_still_sends_a_late_request_to_an_allowed_address";
    let (site, seed) = seed_site(&fetch_after("/allowed", 700)).await;
    let mut config = config();
    config.browser.extra_wait = Some(Duration::from_millis(1500));
    if run(test_name, &seed, config).await.is_none() {
        return;
    }
    let requested: Vec<String> = site
        .received_requests()
        .await
        .expect("request recording is on")
        .iter()
        .map(|request| request.url.path().to_owned())
        .collect();
    assert!(
        requested.iter().any(|p| p == "/allowed"),
        "{test_name}: the late request must reach the allowed address: {requested:?}"
    );
}
