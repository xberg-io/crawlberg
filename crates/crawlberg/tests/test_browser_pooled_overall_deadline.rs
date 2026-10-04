//! Regression coverage for the pooled counterpart of xberg-io/crawlberg#66.
//!
//! `test_browser_overall_deadline.rs` proves `BrowserConfig::overall_timeout` bounds the
//! ONE-SHOT fetch path (`browser.rs:231`, no `browser_pool` configured). The doc comment on
//! `overall_timeout` also promises it covers "page acquisition from a shared pool"
//! (`pooled_fetch`, `browser.rs:143`), but nothing exercised that path before this file --
//! the field had zero coverage for the pooled case specifically.
//!
//! Requires a real Chrome binary (found at `/Applications/Google Chrome.app` in this
//! environment; chromiumoxide auto-detects it) and is gated behind the `browser` feature;
//! skipped (not failed) when Chrome is unavailable, matching the sibling deadline test.

#![cfg(feature = "browser")]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crawlberg::{
    BrowserBackend, BrowserConfig, BrowserMode, BrowserPool, BrowserPoolConfig, BrowserSessionPool, CrawlConfig,
    CrawlError, HostMatcher, create_engine, scrape,
};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata};

mod common;
use common::{announce_chrome_skip, is_missing_chrome_message};

/// Binds a TCP listener that accepts connections and holds each one open without ever
/// reading or writing a byte, so a browser navigation to it stalls indefinitely. See
/// `test_browser_overall_deadline.rs::spawn_stalling_server` for why plain OS threads and
/// no read/write are load-bearing here (avoiding a TCP RST that would fail fast instead).
fn spawn_stalling_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("test server should bind");
    let addr = listener.local_addr().expect("test server should have local addr");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            std::thread::spawn(move || {
                let _stream = stream;
                std::thread::sleep(Duration::from_secs(3600));
            });
        }
    });
    format!("http://{addr}/")
}

fn spawn_stalled_redirect_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("test server should bind");
    let addr = listener.local_addr().expect("test server should have local addr");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            std::thread::spawn(move || {
                let Ok(mut writer) = stream.try_clone() else { return };
                let mut reader = BufReader::new(stream);
                let mut request_line = String::new();
                if reader.read_line(&mut request_line).is_err() {
                    return;
                }
                loop {
                    let mut header = String::new();
                    match reader.read_line(&mut header) {
                        Ok(0) | Err(_) => return,
                        Ok(_) if header == "\r\n" || header == "\n" => break,
                        Ok(_) => {}
                    }
                }
                if request_line.contains(" /ok ") {
                    let body = "<p>follow-up-ok</p>";
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = writer.write_all(response.as_bytes());
                    let _ = writer.flush();
                    return;
                }
                let _ = writer.write_all(
                    b"HTTP/1.1 302 Found\r\nLocation: mailto:someone@example.com\r\n\
                      Content-Type: text/html\r\nContent-Length: 1000000\r\n\r\npartial",
                );
                let _ = writer.flush();
                std::thread::sleep(Duration::from_secs(3600));
            });
        }
    });
    format!("http://{addr}")
}

fn spawn_refused_subresource_redirect_server() -> String {
    let denied = TcpListener::bind("127.0.0.1:0").expect("denied server should bind");
    let denied_url = format!("http://{}/blocked", denied.local_addr().expect("denied address"));
    std::thread::spawn(move || for _ in denied.incoming() {});

    let listener = TcpListener::bind("127.0.0.1:0").expect("test server should bind");
    let addr = listener.local_addr().expect("test server should have local addr");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            let denied_url = denied_url.clone();
            std::thread::spawn(move || {
                let Ok(mut writer) = stream.try_clone() else { return };
                let mut reader = BufReader::new(stream);
                let mut request_line = String::new();
                if reader.read_line(&mut request_line).is_err() {
                    return;
                }
                loop {
                    let mut header = String::new();
                    match reader.read_line(&mut header) {
                        Ok(0) | Err(_) => return,
                        Ok(_) if header == "\r\n" || header == "\n" => break,
                        Ok(_) => {}
                    }
                }
                let path = request_line.split_whitespace().nth(1).unwrap_or("/");
                let response = match path {
                    "/start" => html_response(&format!(
                        "<script>fetch({denied_url:?}).catch(() => 0); \
                         setTimeout(() => location.assign('/redirect'), 200)</script>"
                    )),
                    "/redirect" => "HTTP/1.1 302 Found\r\nLocation: mailto:someone@example.com\r\n\
                                    Content-Type: text/html\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_owned(),
                    "/ok" => html_response("<p>fresh-page-ok</p>"),
                    _ => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
                };
                let _ = writer.write_all(response.as_bytes());
                let _ = writer.flush();
            });
        }
    });
    format!("http://localhost:{}", addr.port())
}

fn html_response(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn stalled_pooled_config(pool: std::sync::Arc<BrowserPool>) -> CrawlConfig {
    CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            mode: BrowserMode::Always,
            session_affinity: false,
            // ~keep Deliberately much longer than overall_timeout below: this proves the
            // ~keep overall deadline is what bounds the pooled fetch, not that the
            // ~keep pre-existing per-navigation timeout happens to also be short.
            timeout: Duration::from_secs(30),
            overall_timeout: Duration::from_secs(2),
            ..BrowserConfig::default()
        },
        browser_pool: Some(pool),
        ..CrawlConfig::builder().allow_private_networks(true).build()
    }
}

/// A pooled fetch against a server that never responds must fail near `overall_timeout`
/// (2s), not run for the full navigation `timeout` (30s) or hang on page acquisition.
#[tokio::test]
#[serial_test::serial(pooled_browser_deadline)]
async fn pooled_fetch_fails_near_the_overall_deadline_when_navigation_stalls() {
    let url = spawn_stalling_server();
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    let engine = create_engine(Some(stalled_pooled_config(Arc::clone(&pool)))).expect("engine must build");

    let start = Instant::now();
    let result = scrape(&engine, &url).await;
    let elapsed = start.elapsed();
    pool.shutdown().await;

    match result {
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(
                "pooled_fetch_fails_near_the_overall_deadline_when_navigation_stalls",
                &message,
            );
        }
        Ok(response) => panic!("a pooled fetch against a server that never responds must not succeed: {response:?}"),
        Err(error) => {
            assert!(
                elapsed < Duration::from_secs(10),
                "expected the pooled fetch to fail near overall_timeout (2s), took {elapsed:?} \
                 instead -- the per-navigation `timeout` (30s) must not be the effective bound: {error}"
            );
        }
    }
}

#[tokio::test]
#[serial_test::serial(pooled_browser_deadline)]
async fn a_terminal_redirect_with_a_stalled_body_is_not_reused_for_the_follow_up_fetch() {
    let test_name = "a_terminal_redirect_with_a_stalled_body_is_not_reused_for_the_follow_up_fetch";
    let base = spawn_stalled_redirect_server();
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    match pool.warm().await {
        Ok(()) => {}
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
            return;
        }
        Err(error) => panic!("warming the pool must either succeed or report a missing Chrome: {error:?}"),
    }
    let mut config = stalled_pooled_config(Arc::clone(&pool));
    config.browser.timeout = Duration::from_secs(5);
    config.browser.overall_timeout = Duration::from_secs(15);
    config.browser.shutdown_timeout = Duration::from_millis(500);
    config.browser.session_affinity = true;
    config.browser_session_pool = Some(Arc::new(BrowserSessionPool::new()));
    let engine = create_engine(Some(config)).expect("engine must build");

    let sink: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let capture = tracing::subscriber::set_default(MessageCapture {
        sink: Arc::clone(&sink),
    });
    let redirect_url = format!("{base}/redirect");
    let started = Instant::now();
    let redirect = scrape(&engine, &redirect_url).await;
    let elapsed = started.elapsed();
    drop(capture);
    match redirect {
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
            pool.shutdown().await;
            return;
        }
        Ok(page) => assert_eq!(
            (page.status_code, page.html.as_str(), page.final_url.as_str()),
            (302, "", redirect_url.as_str()),
            "{test_name}: the redirect headers must return without waiting for its stalled body"
        ),
        Err(error) => panic!("{test_name}: the terminal redirect must succeed immediately: {error:?}"),
    }
    assert!(
        elapsed < Duration::from_secs(3),
        "{test_name}: the redirect headers must return before the 5s navigation timeout, took {elapsed:?}"
    );
    let messages = sink.lock().expect("sink mutex must not be poisoned").clone();
    assert!(
        messages
            .iter()
            .any(|message| message.contains("releasing a pooled browser page")),
        "{test_name}: the terminal redirect page must be released instead of parked; captured messages: {messages:?}"
    );

    let follow_up = scrape(&engine, &format!("{base}/ok"))
        .await
        .unwrap_or_else(|error| panic!("the follow-up must use a fresh page: {error:?}"));
    pool.shutdown().await;
    assert!(follow_up.html.contains("follow-up-ok"), "{}", follow_up.html);
}

#[tokio::test]
#[serial_test::serial(pooled_browser_deadline)]
async fn a_terminal_redirect_after_a_refused_subresource_is_not_reused() {
    let test_name = "a_terminal_redirect_after_a_refused_subresource_is_not_reused";
    let base = spawn_refused_subresource_redirect_server();
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    let session_pool = Arc::new(BrowserSessionPool::new());
    let mut config = CrawlConfig::builder()
        .ssrf_allowlist_host(HostMatcher::exact("localhost"))
        .build();
    config.browser = BrowserConfig {
        backend: BrowserBackend::Chromiumoxide,
        mode: BrowserMode::Always,
        session_affinity: true,
        extra_wait: Some(Duration::from_secs(1)),
        ..BrowserConfig::default()
    };
    config.browser_pool = Some(Arc::clone(&pool));
    config.browser_session_pool = Some(session_pool);
    let engine = create_engine(Some(config)).expect("engine must build");

    let sink: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let capture = tracing::subscriber::set_default(MessageCapture {
        sink: Arc::clone(&sink),
    });
    let first = scrape(&engine, &format!("{base}/start")).await;
    drop(capture);
    let first = match first {
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
            pool.shutdown().await;
            return;
        }
        Ok(page) => page,
        Err(error) => panic!("{test_name}: terminal redirect must be returned: {error:?}"),
    };
    assert_eq!((first.status_code, first.html.as_str()), (302, ""), "{test_name}");
    assert_eq!(first.ssrf_refused_urls.len(), 1, "{test_name}: refused URLs");
    let messages = sink.lock().expect("sink mutex must not be poisoned").clone();
    assert!(
        messages
            .iter()
            .any(|message| message.contains("releasing a pooled browser page")),
        "{test_name}: terminal interception must release, not park, the page: {messages:?}"
    );

    let follow_up = scrape(&engine, &format!("{base}/ok"))
        .await
        .unwrap_or_else(|error| panic!("{test_name}: fresh follow-up page must succeed: {error:?}"));
    pool.shutdown().await;
    assert!(follow_up.html.contains("fresh-page-ok"), "{test_name}");
}

/// Same shape as [`stalled_pooled_config`], with a longer overall deadline.
///
/// ~keep 2s is not enough here: the deadline must fire during NAVIGATION, not during page
/// ~keep acquisition. A fetch that times out while acquiring never holds a page to release, so
/// ~keep it would fail the assertion below for a reason unrelated to the fix -- and it fails
/// ~keep IDENTICALLY, because acquisition is not observable from outside. 5s was tried first and
/// ~keep did exactly that: it passed when run alone and failed when run alongside the sibling
/// ~keep test above, which launches a second Chrome on the same machine. 15s keeps ample room
/// ~keep over acquisition while staying well under the 30s navigation `timeout` this test exists
/// ~keep to prove is not the effective bound. Both tests in this file are `serial` for the same
/// ~keep reason: two concurrent Chrome launches are what made the margin too tight.
fn navigation_deadline_config(pool: Arc<BrowserPool>) -> CrawlConfig {
    let mut config = stalled_pooled_config(pool);
    config.browser.overall_timeout = Duration::from_secs(15);
    config
}

/// Records every `tracing` event's `message` field.
///
/// ~keep Hand-rolled rather than pulling in `tracing-subscriber`, and installed with the
/// ~keep thread-local `set_default` rather than `set_global_default`, exactly as
/// ~keep `test_crawl_span_credential_redaction.rs` does: `#[tokio::test]` runs a current-thread
/// ~keep runtime, so every task of the fetch -- including the page release -- stays on the
/// ~keep thread the guard was set on.
struct MessageCapture {
    sink: Arc<Mutex<Vec<String>>>,
}

struct MessageVisitor<'a>(&'a mut Vec<String>);

impl Visit for MessageVisitor<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0.push(format!("{value:?}"));
        }
    }
}

impl tracing::Subscriber for MessageCapture {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _attrs: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}
    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut sink = self.sink.lock().expect("sink mutex must not be poisoned");
        event.record(&mut MessageVisitor(&mut sink));
    }

    fn enter(&self, _span: &Id) {}
    fn exit(&self, _span: &Id) {}
}

/// A pooled fetch that hits `overall_timeout` mid-navigation must still release its page.
///
/// ~keep The release event is the only signal observable from outside. The semaphore permit is
/// ~keep released either way (it is a local of the dropped future) and `BrowserPool` exposes no
/// ~keep open-target count, so "released" and "leaked" are otherwise indistinguishable from a
/// ~keep test. Before the fix the overall deadline wrapped the whole pooled fetch, so expiry
/// ~keep dropped the future before any release could run and this event never appeared --
/// ~keep `chromiumoxide::Page` has no closing `Drop`, so the CDP target stayed open in the
/// ~keep shared browser. xberg-io/crawlberg#179.
#[tokio::test]
#[serial_test::serial(pooled_browser_deadline)]
async fn a_pooled_fetch_that_hits_the_overall_deadline_still_releases_its_page() {
    const TEST_NAME: &str = "a_pooled_fetch_that_hits_the_overall_deadline_still_releases_its_page";
    const RELEASE_EVENT: &str = "releasing a pooled browser page";

    let pool = BrowserPool::new(BrowserPoolConfig::default());
    match pool.warm().await {
        Ok(()) => {}
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(TEST_NAME, &message);
            return;
        }
        Err(error) => panic!("warming the pool must either succeed or report a missing Chrome: {error:?}"),
    }

    let url = spawn_stalling_server();
    let engine = create_engine(Some(navigation_deadline_config(Arc::clone(&pool)))).expect("engine must build");

    let sink: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let capture = tracing::subscriber::set_default(MessageCapture {
        sink: Arc::clone(&sink),
    });
    let result = scrape(&engine, &url).await;
    drop(capture);
    pool.shutdown().await;

    match result {
        Ok(response) => panic!("a pooled fetch against a server that never responds must not succeed: {response:?}"),
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(TEST_NAME, &message);
            return;
        }
        Err(error) => assert!(
            matches!(error, CrawlError::BrowserTimeout { .. }),
            "the fetch must fail on the overall deadline and not for some other reason, or the \
             release below would be checked on a path this test does not mean to exercise: {error}"
        ),
    }

    let messages = sink.lock().expect("sink mutex must not be poisoned").clone();
    assert!(
        messages.iter().any(|message| message.contains(RELEASE_EVENT)),
        "a pooled fetch that hit its overall deadline must still release its page, but no \
         '{RELEASE_EVENT}' event was emitted; captured messages: {messages:?}"
    );
}
