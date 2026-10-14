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
    BrowserBackend, BrowserConfig, BrowserMode, BrowserPool, BrowserPoolConfig, BrowserSessionPool, CrawlConfig,
    CrawlError, HostMatcher, ScrapeResult, crawl, create_engine, scrape,
};
use wiremock::matchers::{any, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
#[path = "common/foreign_network.rs"]
mod foreign_network;
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
    let (site, seed) = seed_site(&format!(
        "<p>start</p><script>setInterval(() => {{ for (let k = 0; k < 20; k++) fetch({d:?} + '?' + Math.random(), {{ mode: 'no-cors' }}).catch(() => {{}}); window.open({d:?}); }}, 1);</script>"
    ))
    .await;
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    let mut config = pooled_config(&pool);
    config.browser.extra_wait = Some(Duration::from_secs(5));
    let seed_timeout = config.browser.timeout;
    let engine = create_engine(Some(config)).expect("engine must build");
    let cancelled = {
        let fetch = scrape(&engine, &seed);
        tokio::pin!(fetch);
        let seed_loaded = async {
            loop {
                if site
                    .received_requests()
                    .await
                    .expect("record seed requests")
                    .iter()
                    .any(|request| request.url.path() == "/")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        tokio::select! {
            result = &mut fetch => {
                if let Err(CrawlError::BrowserError { message, .. }) = &result
                    && is_missing_chrome_message(message)
                {
                    announce_chrome_skip(test_name, message);
                    pool.shutdown().await;
                    return;
                }
                panic!("{test_name}: the flood must remain running until the seed loads: {result:?}");
            }
            loaded = tokio::time::timeout(seed_timeout, seed_loaded) => {
                loaded.expect("the cancellation must exercise a loaded seed within the browser timeout");
            }
        }
        tokio::time::timeout(Duration::from_millis(2500), &mut fetch).await
    };
    assert!(
        site.received_requests()
            .await
            .expect("record seed requests")
            .iter()
            .any(|request| request.url.path() == "/"),
        "{test_name}: the cancellation must exercise a loaded seed"
    );
    assert!(
        cancelled.is_err(),
        "{test_name}: the flood must still be running when cancelled: {cancelled:?}"
    );
    assert_refused(test_name, &denied).await;
    let (_next_site, next_seed) = seed_site("<p>next</p>").await;
    let next = run(test_name, &next_seed, pooled_config(&pool)).await;
    assert!(
        next.is_some_and(|page| page.html.contains("next")),
        "{test_name}: the pool must still work"
    );
    pool.shutdown().await;
}

/// A page parked for session reuse keeps running its scripts. When the pool shuts down while
/// such a page still floods a denied address, nothing it sent reaches that address.
#[tokio::test]
async fn pool_shutdown_with_a_parked_flooding_page_leaks_nothing() {
    let test_name = "pool_shutdown_with_a_parked_flooding_page_leaks_nothing";
    let denied = denied_server().await;
    let (_site, seed) = seed_site(&fetch_flood(&denied_url(&denied))).await;
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    let mut config = pooled_config(&pool);
    config.browser.session_affinity = true;
    config.browser_session_pool = Some(Arc::new(BrowserSessionPool::new()));
    if run(test_name, &seed, config).await.is_none() {
        pool.shutdown().await;
        return;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    pool.shutdown().await;
    assert_refused(test_name, &denied).await;
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

#[derive(Debug, serde::Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum ForeignFailurePhase {
    Fetch,
    Body,
}

#[derive(Debug, serde::Deserialize, PartialEq)]
enum ForeignFailureName {
    AbortError,
    TypeError,
    Other,
}

#[derive(Debug, serde::Deserialize, PartialEq)]
struct ForeignFailure {
    phase: ForeignFailurePhase,
    name: ForeignFailureName,
}

#[derive(Debug, serde::Deserialize)]
struct ForeignProbeState {
    loads: u64,
    headers: u64,
    continued: u64,
    refused: u64,
    failures: Vec<ForeignFailure>,
}

fn foreign_probe_script(target: &str) -> String {
    format!(
        r#"const tally = key => sessionStorage.setItem(key, Number(sessionStorage.getItem(key) || 0) + 1);
        tally('loads');
        let phase = 'fetch';
        const controller = new AbortController();
        window.abortForeignProbe = () => controller.abort();
        window.foreignProbe = fetch({target:?}, {{ mode: 'cors', signal: controller.signal }})
            .then(response => {{ tally('headers'); phase = 'body'; return response.text(); }})
            .then(body => {{ window.foreignProbeBody = body; tally('continued'); }})
            .catch(error => {{
                tally('refused');
                const failures = JSON.parse(sessionStorage.getItem('failures') || '[]');
                if (failures.length < 16) {{
                    const name = ['AbortError', 'TypeError'].includes(error.name) ? error.name : 'Other';
                    failures.push({{phase, name}});
                    sessionStorage.setItem('failures', JSON.stringify(failures));
                }}
            }})
            .finally(() => {{
                if (Number(sessionStorage.getItem('loads')) < (window.foreignProbeMaxLoads || Infinity)) {{
                    setTimeout(() => location.reload(), 100);
                }}
            }});"#
    )
}

const FOREIGN_PROBE_STATE: &str = "({loads: Number(sessionStorage.getItem('loads') || 0), \
    headers: Number(sessionStorage.getItem('headers') || 0), \
    continued: Number(sessionStorage.getItem('continued') || 0), \
    refused: Number(sessionStorage.getItem('refused') || 0), \
    failures: JSON.parse(sessionStorage.getItem('failures') || '[]')})";

async fn foreign_probe_state(page: &chromiumoxide::Page) -> Result<ForeignProbeState, chromiumoxide::error::CdpError> {
    page.evaluate(FOREIGN_PROBE_STATE)
        .await?
        .into_value()
        .map_err(Into::into)
}

struct HeldForeignBody {
    url: String,
    headers_sent: tokio::sync::oneshot::Receiver<()>,
    release: tokio::sync::watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for HeldForeignBody {
    fn drop(&mut self) {
        self.task.abort();
    }
}

const FOREIGN_PROBE_BODY: &str = "foreign-body-complete";

async fn respond_with_held_foreign_body(
    stream: &mut tokio::net::TcpStream,
    released: &mut tokio::sync::watch::Receiver<bool>,
    headers_sent: &mut Option<tokio::sync::oneshot::Sender<()>>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    const MAX_REQUEST_BYTES: usize = 8192;
    const READ_BUFFER_BYTES: usize = 1024;
    let mut request = Vec::new();
    let mut buffer = [0_u8; READ_BUFFER_BYTES];
    while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
        let read = stream.read(&mut buffer).await.expect("probe request");
        assert_ne!(read, 0);
        request.extend_from_slice(&buffer[..read]);
        assert!(request.len() <= MAX_REQUEST_BYTES);
    }
    let headers = format!(
        "HTTP/1.1 200 OK\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        FOREIGN_PROBE_BODY.len()
    );
    stream.write_all(headers.as_bytes()).await.expect("probe headers");
    if let Some(sent) = headers_sent.take() {
        let _ = sent.send(());
    }
    if released.wait_for(|release| *release).await.is_ok() {
        let _ = stream.write_all(FOREIGN_PROBE_BODY.as_bytes()).await;
        let _ = stream.shutdown().await;
    }
}

async fn held_foreign_body() -> HeldForeignBody {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("probe listener");
    let url = format!("http://{}/probe", listener.local_addr().unwrap());
    let (headers_sent, headers_received) = tokio::sync::oneshot::channel();
    let (release, mut released) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(async move {
        let mut headers_sent = Some(headers_sent);
        while let Ok((mut stream, _)) = listener.accept().await {
            respond_with_held_foreign_body(&mut stream, &mut released, &mut headers_sent).await;
        }
    });
    HeldForeignBody {
        url,
        headers_sent: headers_received,
        release,
        task,
    }
}

async fn launch_foreign_probe_browser(
    test_name: &str,
) -> (chromiumoxide::Browser, tokio::task::JoinHandle<()>, tempfile::TempDir) {
    let profile = tempfile::tempdir().expect("probe Chrome profile");
    let config = chromiumoxide::browser::BrowserConfig::builder()
        .no_sandbox()
        .new_headless_mode()
        .user_data_dir(profile.path())
        .build()
        .expect("probe Chrome configuration");
    let (browser, handler) = chromiumoxide::Browser::launch(config)
        .await
        .unwrap_or_else(|error| panic!("{test_name}: actual Chrome is required for this control: {error}"));
    (browser, common::spawn_handler(handler), profile)
}

async fn wait_for_foreign_probe_headers(page: &chromiumoxide::Page) -> ForeignProbeState {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let state = foreign_probe_state(page).await.expect("probe state");
            if state.headers > 0 {
                break state;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("probe must observe response headers")
}

async fn release_foreign_body_and_wait_for_reload(page: &chromiumoxide::Page, body: &HeldForeignBody) {
    use chromiumoxide::cdp::browser_protocol::page::EventDomContentEventFired;
    use futures::StreamExt as _;
    let mut reloaded = page
        .event_listener::<EventDomContentEventFired>()
        .await
        .expect("reload listener");
    body.release.send(true).expect("release body");
    tokio::time::timeout(Duration::from_secs(5), reloaded.next())
        .await
        .expect("completed response must permit reload")
        .expect("reload event");
}

async fn completed_foreign_probe(page: &chromiumoxide::Page) -> ForeignProbeState {
    page.evaluate(format!("window.foreignProbe.then(() => {FOREIGN_PROBE_STATE})"))
        .await
        .expect("completed probe")
        .into_value()
        .expect("completed probe state")
}

#[tokio::test]
async fn should_wait_for_the_foreign_response_body_before_reloading() {
    let test_name = "should_wait_for_the_foreign_response_body_before_reloading";
    let (mut browser, handler, _profile) = launch_foreign_probe_browser(test_name).await;
    let mut body = held_foreign_body().await;
    let (site, seed) = seed_site(&format!(
        "<script>window.foreignProbeMaxLoads = 2; {}</script>",
        foreign_probe_script(&body.url)
    ))
    .await;
    let page = browser.new_page("about:blank").await.expect("probe tab");
    page.goto(seed).await.expect("probe navigation");
    tokio::time::timeout(Duration::from_secs(5), &mut body.headers_sent)
        .await
        .unwrap()
        .unwrap();
    let before = wait_for_foreign_probe_headers(&page).await;
    release_foreign_body_and_wait_for_reload(&page, &body).await;
    let after = completed_foreign_probe(&page).await;
    let completed_body = page
        .evaluate("window.foreignProbeBody")
        .await
        .and_then(|result| result.into_value::<String>().map_err(Into::into))
        .ok();
    let received = site.received_requests().await.expect("probe recording");
    foreign_network::shutdown_owned_browser(&mut browser, handler)
        .await
        .expect("probe cleanup");
    assert_eq!(
        (before.loads, before.headers, before.continued, before.refused),
        (1, 1, 0, 0)
    );
    assert_eq!(before.failures, []);
    assert_eq!(
        (after.loads, after.headers, after.continued, after.refused),
        (2, 2, 2, 0)
    );
    assert_eq!(after.failures, []);
    assert_eq!(completed_body.as_deref(), Some(FOREIGN_PROBE_BODY));
    assert_eq!(received.iter().filter(|request| request.url.path() == "/").count(), 2);
}

#[tokio::test]
async fn should_count_an_explicitly_aborted_foreign_fetch_as_a_rejection() {
    let test_name = "should_count_an_explicitly_aborted_foreign_fetch_as_a_rejection";
    let (mut browser, handler, _profile) = launch_foreign_probe_browser(test_name).await;
    let (site, seed) = seed_site("<p>probe</p>").await;
    Mock::given(path("/held"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(30)))
        .mount(&site)
        .await;
    let page = browser.new_page(&seed).await.expect("probe tab");
    let script = format!(
        "window.foreignProbeMaxLoads = 1; {}",
        foreign_probe_script(&format!("{seed}held"))
    );
    page.evaluate(format!("(() => {{{script}}})()"))
        .await
        .expect("start probe");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if site
                .received_requests()
                .await
                .unwrap()
                .iter()
                .any(|request| request.url.path() == "/held")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("probe must reach its server before cancellation");
    page.evaluate("window.abortForeignProbe()").await.expect("cancel probe");
    let state = completed_foreign_probe(&page).await;
    foreign_network::shutdown_owned_browser(&mut browser, handler)
        .await
        .expect("probe cleanup");
    assert_eq!((state.headers, state.continued, state.refused), (0, 0, 1));
    assert_eq!(
        state.failures,
        [ForeignFailure {
            phase: ForeignFailurePhase::Fetch,
            name: ForeignFailureName::AbortError
        }]
    );
}

/// On an external browser used without IP denials, another client's tab keeps working while
/// crawlberg scrapes, and Crawlberg does not claim a socket-denial boundary it cannot own.
#[tokio::test]
async fn an_external_browser_keeps_other_clients_tabs_working() {
    let test_name = "an_external_browser_keeps_other_clients_tabs_working";
    let private = MockServer::start().await;
    Mock::given(any())
        .respond_with(html(SECRET_MARKER).append_header("access-control-allow-origin", "*"))
        .mount(&private)
        .await;
    let d = denied_url(&private);
    let Some(browser_config) = common::expect_chrome_or_skip(
        test_name,
        chromiumoxide::browser::BrowserConfig::builder()
            .no_sandbox()
            .new_headless_mode()
            .user_data_dir(std::env::temp_dir().join(format!("crawlberg-{test_name}-{}", std::process::id())))
            .build(),
    ) else {
        return;
    };
    let Some((mut other_client, handler)) =
        common::expect_chrome_or_skip(test_name, chromiumoxide::Browser::launch(browser_config).await)
    else {
        return;
    };
    let handler_task = common::spawn_handler(handler);
    // ~keep Wait for a readable body before reloading; a no-cors response exposes no body to drain.
    let (_other_site, other_seed) = seed_site(&format!(
        "<script>{}</script>",
        foreign_probe_script(&format!("{d}?other"))
    ))
    .await;
    let other_tab = other_client
        .new_page("about:blank")
        .await
        .expect("the other client's tab must open");
    // ~keep Observe before navigation so failures preceding interception remain distinguishable.
    let observer = foreign_network::Observer::start(&other_tab)
        .await
        .expect("foreign network observer");
    other_tab
        .goto(other_seed.as_str())
        .await
        .expect("the other client's tab must navigate");
    let (_site, seed) = seed_site(&format!("<p>start</p><script>window.open({d:?} + '?popup');</script>")).await;
    let mut config = config();
    config.ssrf.deny_private = false;
    config.browser.endpoint = Some(other_client.websocket_address().clone());
    config.browser.extra_wait = Some(Duration::from_millis(1500));

    let count = |requests: &[wiremock::Request], query: &str| {
        requests
            .iter()
            .filter(|request| request.url.query() == Some(query))
            .count()
    };
    let before = count(&private.received_requests().await.expect("recording"), "other");
    let before_probe = foreign_probe_state(&other_tab).await.ok();
    // ~keep Events drain asynchronously; these snapshots bound observation, not exact CDP failure time.
    let before_failures = observer.snapshot();
    let result = run(test_name, &seed, config).await;
    let received = private.received_requests().await.expect("recording");
    if result.is_none() {
        drop(observer);
        foreign_network::shutdown_owned_browser(&mut other_client, handler_task)
            .await
            .expect("owned external browser cleanup");
        return;
    }
    let during = count(&received, "other") - before;
    let mut probe = None;
    for _ in 0..50 {
        if let Ok(value) = foreign_probe_state(&other_tab).await {
            probe = Some(value);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let refused = probe.as_ref().map(|state| state.refused);
    let failures = observer.snapshot();
    drop(observer);
    foreign_network::shutdown_owned_browser(&mut other_client, handler_task)
        .await
        .expect("owned external browser cleanup");
    // ~keep The other tab reloads 100 ms after each fetch settles, so its requests come from
    // ~keep documents loaded while crawlberg's check is on; a document loaded before that is
    // ~keep never paused. No count of them is asserted beyond one: load decides how many fit
    // ~keep in the scrape. The property is that none is refused.
    assert!(
        during >= 1,
        "{test_name}: the other client's requests must keep reaching its address during the scrape, got {during}"
    );
    assert_eq!(
        refused,
        Some(0),
        "{test_name}: the check must refuse none of the other client's requests; \
         pre_scrape_probe={before_probe:?}, probe={probe:?}, pre_scrape_network={before_failures:?}, network={failures:?}"
    );
    assert!(
        count(&received, "popup") >= 1,
        "{test_name}: the permissive endpoint policy must let crawlberg's popup run"
    );
}

/// On an external browser reached through `browser.endpoint`, the scrape's page starts with the
/// browser's own cookies, and the cookies the page sets stay out of the browser.
#[tokio::test]
async fn an_external_browsers_cookies_reach_the_scrape_and_its_own_stay_out() {
    use chromiumoxide::cdp::browser_protocol::storage::GetCookiesParams;

    let test_name = "an_external_browsers_cookies_reach_the_scrape_and_its_own_stay_out";
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(html("start").append_header("set-cookie", "fresh=1; Path=/"))
        .mount(&site)
        .await;
    let seed = format!("http://localhost:{}/", site.address().port());
    let Some(mut other_client) = common::launch_external_chrome_with_cookie(test_name, &seed).await else {
        return;
    };
    let baseline = other_client
        .execute(GetCookiesParams::default())
        .await
        .expect("setup: the external browser must list its cookies")
        .result
        .cookies;
    let owner = baseline.iter().find(|cookie| cookie.name == "owner");
    assert_eq!(
        owner.map(|cookie| cookie.value.as_str()),
        Some("1"),
        "setup: owner cookie baseline"
    );
    let mut config = config();
    config.ssrf.deny_private = false;
    config.browser.endpoint = Some(other_client.websocket_address().clone());
    let result = run(test_name, &seed, config).await;
    let received = site.received_requests().await.expect("recording");
    let sent = received
        .iter()
        .find(|request| request.url.path() == "/")
        .and_then(|request| request.headers.get("cookie"))
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let mut names: Vec<String> = other_client
        .execute(GetCookiesParams::default())
        .await
        .expect("the browser must list its cookies")
        .result
        .cookies
        .into_iter()
        .map(|cookie| cookie.name)
        .collect();
    names.sort();
    let _ = other_client.close().await;
    let _ = other_client.wait().await;
    if result.is_none() {
        return;
    }
    assert!(
        sent.contains("owner=1"),
        "{test_name}: setup verified owner=1; the page-request stage must send it; cookie_header_present={}",
        !sent.is_empty()
    );
    assert_eq!(
        names,
        vec!["owner".to_owned()],
        "{test_name}: the page's own cookie must stay out of the external browser"
    );
}

/// A page with an image at a denied address and a `fetch()` to one during the extra wait.
fn refused_image_and_late_fetch(denied: &MockServer) -> (String, String, String) {
    let image = format!("{}?image", denied_url(denied));
    let late = format!("{}?late", denied_url(denied));
    let body = format!("<p>start</p><img src={image:?}>{}", fetch_after(&late, 700));
    (body, image, late)
}

/// Assert `listed` names exactly `expected`, in any order.
fn assert_listed(test_name: &str, mut listed: Vec<String>, mut expected: Vec<String>) {
    listed.sort();
    expected.sort();
    assert_eq!(
        listed, expected,
        "{test_name}: the result must list every refused address"
    );
}

/// A refused image and a refused `fetch()` during the extra wait keep the page, and the result
/// lists both addresses.
#[tokio::test]
async fn scrape_keeps_the_page_and_lists_the_refused_requests() {
    let test_name = "scrape_keeps_the_page_and_lists_the_refused_requests";
    let denied = denied_server().await;
    let (body, image, late) = refused_image_and_late_fetch(&denied);
    let (_site, seed) = seed_site(&body).await;
    let mut config = config();
    config.browser.extra_wait = Some(Duration::from_millis(1500));
    let Some(result) = run(test_name, &seed, config).await else {
        return;
    };
    assert!(
        result.html.contains("start"),
        "{test_name}: the page must be kept: {}",
        result.html
    );
    assert_listed(test_name, result.ssrf_refused_urls, vec![image, late]);
    assert_refused(test_name, &denied).await;
}

/// A page that sends nothing to a denied address lists nothing.
#[tokio::test]
async fn scrape_lists_nothing_when_nothing_is_refused() {
    let test_name = "scrape_lists_nothing_when_nothing_is_refused";
    let (_site, seed) = seed_site(r#"<p>start</p><img src="/allowed">"#).await;
    let Some(result) = run(test_name, &seed, config()).await else {
        return;
    };
    assert!(
        result.ssrf_refused_urls.is_empty(),
        "{test_name}: nothing was refused, got {:?}",
        result.ssrf_refused_urls
    );
}

/// A crawl in browser mode lists the refused addresses on the page result.
#[tokio::test]
async fn crawl_lists_the_refused_requests_on_the_page() {
    let test_name = "crawl_lists_the_refused_requests_on_the_page";
    let denied = denied_server().await;
    let (body, image, late) = refused_image_and_late_fetch(&denied);
    let (_site, seed) = seed_site(&body).await;
    let mut config = config();
    config.browser.extra_wait = Some(Duration::from_millis(1500));
    config.max_depth = Some(0);
    let engine = create_engine(Some(config)).expect("engine must build");
    let result = match crawl(&engine, &seed).await {
        Ok(result) => result,
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
            return;
        }
        Err(error) => panic!("{test_name}: crawl must succeed: {error:?}"),
    };
    let page = result
        .pages
        .first()
        .unwrap_or_else(|| panic!("{test_name}: no page: {result:?}"));
    assert!(
        page.html.contains("start"),
        "{test_name}: the page must be kept: {}",
        page.html
    );
    assert_listed(test_name, page.ssrf_refused_urls.clone(), vec![image, late]);
    assert_refused(test_name, &denied).await;
}

/// On an external browser reached through `browser_endpoint`, a pooled scrape's page starts
/// with the browser's own cookies, as a one-shot scrape's does.
#[tokio::test]
async fn a_pooled_scrape_on_an_external_browser_starts_with_its_cookies() {
    let test_name = "a_pooled_scrape_on_an_external_browser_starts_with_its_cookies";
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(html("start"))
        .mount(&site)
        .await;
    let seed = format!("http://localhost:{}/", site.address().port());
    let Some(mut other_client) = common::launch_external_chrome_with_cookie(test_name, &seed).await else {
        return;
    };
    let pool = BrowserPool::new(BrowserPoolConfig {
        browser_endpoint: Some(other_client.websocket_address().clone()),
        ..BrowserPoolConfig::default()
    });
    let mut config = pooled_config(&pool);
    config.ssrf.deny_private = false;
    let result = run(test_name, &seed, config).await;
    pool.shutdown().await;
    let received = site.received_requests().await.expect("recording");
    let sent = received
        .iter()
        .find(|request| request.url.path() == "/")
        .and_then(|request| request.headers.get("cookie"))
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let _ = other_client.close().await;
    let _ = other_client.wait().await;
    if result.is_none() {
        return;
    }
    assert!(
        sent.contains("owner=1"),
        "{test_name}: the pooled page must start with the external browser's cookie, sent {sent:?}"
    );
}

/// A dedicated worker's request to a denied address is refused and listed, while its request
/// to an allowed address still leaves.
#[tokio::test]
async fn scrape_refuses_a_worker_request_to_a_denied_address() {
    let test_name = "scrape_refuses_a_worker_request_to_a_denied_address";
    let denied = denied_server().await;
    let worker = format!(
        "fetch(self.location.origin + '/allowed?worker').catch(() => {{}}); fetch({:?}, {{ mode: 'no-cors' }}).catch(() => {{}});",
        denied_url(&denied)
    );
    let body = format!(
        "<p>start</p><script>new Worker(URL.createObjectURL(new Blob([{worker:?}], {{ type: 'text/javascript' }})));</script>"
    );
    let (site, seed) = seed_site(&body).await;
    let mut config = config();
    config.browser.extra_wait = Some(Duration::from_millis(1000));
    let Some(result) = run(test_name, &seed, config).await else {
        return;
    };
    let requested: Vec<String> = site
        .received_requests()
        .await
        .expect("request recording is on")
        .iter()
        .filter_map(|request| request.url.query().map(str::to_owned))
        .collect();
    assert!(
        requested.iter().any(|query| query == "worker"),
        "{test_name}: the worker's allowed request must leave: {requested:?}"
    );
    assert_listed(test_name, result.ssrf_refused_urls, vec![denied_url(&denied)]);
    assert_refused(test_name, &denied).await;
}

/// A popup to a denied address with credentials, opened twice, is listed once, without them.
///
/// ~keep A popup is a top-level navigation, so Chrome sends its userinfo to the check; a
/// ~keep subresource URL with userinfo Chrome refuses on its own, before the check sees it.
#[tokio::test]
async fn scrape_lists_a_refused_popup_once_without_its_credentials() {
    let test_name = "scrape_lists_a_refused_popup_once_without_its_credentials";
    let denied = denied_server().await;
    let port = denied.address().port();
    let target = format!("http://user:secret@127.0.0.1:{port}/cred");
    let body = format!("<p>start</p><script>window.open({target:?}); window.open({target:?});</script>");
    let (_site, seed) = seed_site(&body).await;
    let mut config = config();
    config.browser.extra_wait = Some(Duration::from_millis(1000));
    let Some(result) = run(test_name, &seed, config).await else {
        return;
    };
    assert_eq!(
        result.ssrf_refused_urls,
        vec![format!("http://127.0.0.1:{port}/cred")],
        "{test_name}: the popup's address must be listed once, without its credentials"
    );
    assert_refused(test_name, &denied).await;
}
