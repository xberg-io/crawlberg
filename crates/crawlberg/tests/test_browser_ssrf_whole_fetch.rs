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

/// A page that sends 20 requests to `target` every millisecond.
fn fetch_flood(target: &str) -> String {
    format!(
        "<p>start</p><script>setInterval(() => {{ for (let k = 0; k < 20; k++) fetch({target:?} + '?' + Math.random(), {{ mode: 'no-cors' }}).catch(() => {{}}); }}, 1);</script>"
    )
}

/// A page that opens a popup to `target` every 10 ms.
fn popup_flood(target: &str) -> String {
    format!("<p>start</p><script>setInterval(() => window.open({target:?} + '?' + Math.random()), 10);</script>")
}

async fn scrape_then_assert_refused(test_name: &str, body: &str, denied: &MockServer, pooled: bool) {
    let (_site, seed) = seed_site(body).await;
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    let config = if pooled { pooled_config(&pool) } else { config() };
    let result = run(test_name, &seed, config).await;
    if result.is_some() {
        assert_refused(test_name, denied).await;
    }
    pool.shutdown().await;
}

#[tokio::test]
async fn scrape_refuses_a_request_flood_while_the_fetch_ends() {
    let test_name = "scrape_refuses_a_request_flood_while_the_fetch_ends";
    let denied = denied_server().await;
    for pooled in [false, true] {
        scrape_then_assert_refused(test_name, &fetch_flood(&denied_url(&denied)), &denied, pooled).await;
    }
}

#[tokio::test]
async fn scrape_refuses_popups_the_page_keeps_opening() {
    let test_name = "scrape_refuses_popups_the_page_keeps_opening";
    let denied = denied_server().await;
    for pooled in [false, true] {
        scrape_then_assert_refused(test_name, &popup_flood(&denied_url(&denied)), &denied, pooled).await;
    }
}

/// A pooled scrape cancelled mid-fetch closes its page and popups under the check, and the
/// pool keeps working.
#[tokio::test]
async fn cancelled_pooled_scrape_leaks_nothing() {
    let test_name = "cancelled_pooled_scrape_leaks_nothing";
    let denied = denied_server().await;
    let d = denied_url(&denied);
    let (_site, seed) = seed_site(&format!(
        "<p>start</p><script>setInterval(() => {{ for (let k = 0; k < 20; k++) fetch({d:?} + '?' + Math.random(), {{ mode: 'no-cors' }}).catch(() => {{}}); window.open({d:?}); }}, 1);</script>"
    ))
    .await;
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    let mut config = pooled_config(&pool);
    config.browser.extra_wait = Some(Duration::from_secs(5));
    let engine = create_engine(Some(config)).expect("engine must build");
    if tokio::time::timeout(Duration::from_millis(2500), scrape(&engine, &seed))
        .await
        .is_ok()
    {
        // ~keep The scrape ended before the timeout: Chrome is missing or the page failed.
        pool.shutdown().await;
        return;
    }
    assert_refused(test_name, &denied).await;
    let (_next_site, next_seed) = seed_site("<p>next</p>").await;
    let next = run(test_name, &next_seed, pooled_config(&pool)).await;
    assert!(
        next.is_some_and(|page| page.html.contains("next")),
        "{test_name}: the pool must still work"
    );
    pool.shutdown().await;
}

/// A permissive policy (private networks allowed) for a second engine on the same pool.
fn permissive_pooled_config(pool: &Arc<BrowserPool>) -> CrawlConfig {
    let mut config = CrawlConfig {
        browser: config().browser,
        browser_pool: Some(Arc::clone(pool)),
        respect_robots_txt: false,
        ..CrawlConfig::builder().allow_private_networks(true).build()
    };
    config.browser.session_affinity = false;
    config
}

/// Two engines with different policies share one pooled browser. Each page's frames and
/// popups are judged by that page's own policy, whatever the other page allows.
#[tokio::test]
async fn pooled_pages_are_each_judged_by_their_own_policy() {
    let test_name = "pooled_pages_are_each_judged_by_their_own_policy";
    let private = denied_server().await;
    let d = denied_url(&private);
    let (_permissive_site, permissive_seed) = seed_site(&format!("<p>A</p><iframe src={d:?}></iframe>")).await;
    let (_strict_site, strict_seed) = seed_site(&format!(
        "<p>B</p><script>setTimeout(() => window.open({d:?} + '?strict'), 300);</script>"
    ))
    .await;
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    let mut permissive = permissive_pooled_config(&pool);
    permissive.browser.extra_wait = Some(Duration::from_millis(1500));
    let mut strict = pooled_config(&pool);
    strict.browser.extra_wait = Some(Duration::from_millis(1500));
    let permissive = create_engine(Some(permissive)).expect("engine must build");
    let strict = create_engine(Some(strict)).expect("engine must build");
    let (a, b) = tokio::join!(scrape(&permissive, &permissive_seed), scrape(&strict, &strict_seed));
    match (&a, &b) {
        (Ok(_), Ok(_)) => {}
        (Err(CrawlError::BrowserError { message, .. }), _) if is_missing_chrome_message(message) => {
            announce_chrome_skip(test_name, message);
            pool.shutdown().await;
            return;
        }
        _ => panic!("{test_name}: both scrapes must succeed: {a:?} {b:?}"),
    }
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let requested: Vec<String> = private
        .received_requests()
        .await
        .expect("request recording is on")
        .iter()
        .map(|request| request.url.to_string())
        .collect();
    assert!(
        requested.iter().any(|url| !url.contains("strict")),
        "{test_name}: the permissive page's iframe must load: {requested:?}"
    );
    assert!(
        !requested.iter().any(|url| url.contains("strict")),
        "{test_name}: the strict page's popup must be refused: {requested:?}"
    );
    pool.shutdown().await;
}

/// Pages that start navigating while the check is being turned on or off are still checked:
/// rounds of concurrent pooled scrapes, each page loading a denied image at once.
#[tokio::test]
async fn pooled_scrapes_that_start_together_are_all_checked() {
    let test_name = "pooled_scrapes_that_start_together_are_all_checked";
    let denied = denied_server().await;
    let (_site, seed) = seed_site(&format!(r#"<img src="{}"><p>start</p>"#, denied_url(&denied))).await;
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    let engine = create_engine(Some(pooled_config(&pool))).expect("engine must build");
    for _ in 0..5 {
        let results = futures::future::join_all((0..4).map(|_| scrape(&engine, &seed))).await;
        if let Some(Err(CrawlError::BrowserError { message, .. })) = results.first()
            && is_missing_chrome_message(message)
        {
            announce_chrome_skip(test_name, message);
            pool.shutdown().await;
            return;
        }
        assert!(results.iter().all(Result::is_ok), "{test_name}: {results:?}");
    }
    assert_refused(test_name, &denied).await;
    pool.shutdown().await;
}

/// Control: the requests of a worker, an iframe and a popup of the page are judged by the
/// page's policy, so those to an allowed address still leave.
#[tokio::test]
async fn scrape_still_sends_allowed_requests_from_workers_frames_and_popups() {
    let test_name = "scrape_still_sends_allowed_requests_from_workers_frames_and_popups";
    let (site, seed) = seed_site(concat!(
        "<p>start</p><iframe src=\"/allowed?frame\"></iframe><script>",
        "new Worker(URL.createObjectURL(new Blob([\"fetch(self.location.origin + '/allowed?worker')\"], { type: 'text/javascript' })));",
        "window.open('/allowed?popup');",
        "</script>"
    ))
    .await;
    let mut config = config();
    config.browser.extra_wait = Some(Duration::from_millis(1000));
    if run(test_name, &seed, config).await.is_none() {
        return;
    }
    let requested: Vec<String> = site
        .received_requests()
        .await
        .expect("request recording is on")
        .iter()
        .filter_map(|request| request.url.query().map(str::to_owned))
        .collect();
    for source in ["frame", "worker", "popup"] {
        assert!(
            requested.iter().any(|query| query == source),
            "{test_name}: the {source} request must reach the allowed address: {requested:?}"
        );
    }
}
