//! Regression coverage for xberg-io/crawlberg#567: `browser.timeout` bounded only the
//! navigation block (`goto` + `wait_for_ready`). The reads of the committed document after it
//! run a script in the page, so the renderer answers them only when its main thread is free.
//! On a page that keeps that thread busy, each read waits for as long as the page runs its
//! script, up to chromiumoxide's fixed 30 s CDP timeout. The scrape paths and the interact path
//! must now fail within `browser.timeout`, with a `BrowserTimeout` naming the budget.
//!
//! Requires a real Chrome binary, which chromiumoxide auto-detects, and is gated behind the
//! `browser` feature; skipped (not failed) when Chrome is unavailable, matching the sibling
//! deadline tests.

#![cfg(feature = "browser")]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crawlberg::{
    BrowserBackend, BrowserConfig, BrowserMode, BrowserPool, BrowserPoolConfig, CrawlConfig, CrawlError, PageAction,
    create_engine, interact, scrape,
};

mod common;
use common::{announce_chrome_skip, is_missing_chrome_message};

/// Serves the renderer-saturating page shape: the document commits and loads immediately,
/// then a `setTimeout` at 300 ms starts repeating 5 s busy loops on the main thread, so every
/// renderer-answered CDP command issued after ~300 ms queues behind main-thread work.
///
/// ~keep Timing, so the READ bound fires and not the pre-existing NAV bound: `load` fires at
/// ~keep ~0 ms and `page_fetch`'s navigation+ready block (goto + ~1 s settle, plain sleeps, no
/// ~keep CDP round-trip) completes well inside `browser.timeout`; the `page.content()` read is
/// ~keep therefore issued ~1.1-1.5 s, inside a busy block, so the fresh 3 s read bound fires —
/// ~keep BrowserTimeout naming "reading the committed document" and "3s", total elapsed ~4-8 s.
///
/// ~keep A navigation the server accepts and never answers does not stall the reads: it waits in
/// ~keep Chrome's browser process while the renderer keeps answering. The stall needs a busy
/// ~keep renderer main thread.
///
/// Connections are answered (if the document) and then held open for hours, never dropped
/// with unread bytes in flight: dropping in that state can send a TCP RST -- see
/// `test_browser_overall_deadline.rs::spawn_stalling_server` for why holding is load-bearing.
fn spawn_saturating_renderer_server() -> String {
    spawn_server(|path| (path == "/").then(|| html_response(SATURATING_PAGE)))
}

const SATURATING_PAGE: &str = "<!doctype html><html><head><title>saturated</title></head><body><p>committed</p>\
    <script>setTimeout(function(){ setInterval(function(){ \
    const until = Date.now() + 5000; while (Date.now() < until) {} }, 1); }, 300);</script>\
    </body></html>";

/// Serves `/start` as a redirect to `/landing`, a page that starts one 8 s busy loop on its main
/// thread right after its HTML is read. The HTML read answers, and the read of the final URL
/// that follows it waits behind the busy loop.
///
/// ~keep The page wraps the `outerHTML` getter that chromiumoxide's `content()` script calls, so
/// ~keep the busy loop starts at the HTML read itself rather than at a fixed time.
fn spawn_redirect_to_busy_after_read_server() -> String {
    spawn_server(|path| match path {
        "/start" => Some(
            "HTTP/1.1 302 Found\r\nLocation: /landing\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
        ),
        "/landing" => Some(html_response(BUSY_AFTER_READ_PAGE)),
        _ => None,
    })
}

const BUSY_AFTER_READ_PAGE: &str = "<!doctype html><html><head><title>busy-after-read</title></head><body><p>landed</p>\
    <script>(function(){ const original = Object.getOwnPropertyDescriptor(Element.prototype, 'outerHTML'); \
    let armed = true; Object.defineProperty(Element.prototype, 'outerHTML', { configurable: true, set: original.set, \
    get: function(){ const html = original.get.call(this); if (armed) { armed = false; setTimeout(function(){ \
    const until = Date.now() + 8000; while (Date.now() < until) {} }, 0); } return html; } }); })();</script>\
    </body></html>";

fn html_response(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

/// Answers each request with the response `respond` gives for its path, or with nothing.
fn spawn_server(respond: fn(&str) -> Option<String>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("test server should bind");
    let addr = listener.local_addr().expect("test server should have local addr");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            std::thread::spawn(move || {
                let Ok(write_half) = stream.try_clone() else { return };
                let mut reader = BufReader::new(stream);
                let mut request_line = String::new();
                if reader.read_line(&mut request_line).is_err() {
                    return;
                }
                loop {
                    let mut header = String::new();
                    match reader.read_line(&mut header) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            if header == "\r\n" || header == "\n" {
                                break;
                            }
                        }
                    }
                }
                if let Some(response) = request_line.split_whitespace().nth(1).and_then(respond) {
                    let mut writer = write_half;
                    let _ = writer.write_all(response.as_bytes());
                    let _ = writer.flush();
                }
                // ~keep Hold every connection open for the test's lifetime, whether answered or not.
                std::thread::sleep(Duration::from_secs(3600));
            });
        }
    });
    format!("http://{addr}/")
}

fn read_bound_config() -> CrawlConfig {
    CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            mode: BrowserMode::Always,
            // ~keep The bound under test: the stalled document read must fire at 3s, not at
            // ~keep chromiumoxide's internal 30s. overall_timeout is deliberately far larger so
            // ~keep the ONLY deadline that can end this fetch is the new per-read bound.
            timeout: Duration::from_secs(3),
            overall_timeout: Duration::from_secs(30),
            ..BrowserConfig::default()
        },
        ..CrawlConfig::builder().allow_private_networks(true).build()
    }
}

/// One short wait, so an interaction has an action to run before its final read.
fn one_short_wait() -> Vec<PageAction> {
    vec![PageAction::Wait {
        milliseconds: Some(100),
        selector: None,
    }]
}

/// Shared by the HTML-read tests: a `BrowserTimeout` naming the read and the budget, well before
/// the busy page frees its renderer. Skips (does not fail) on runners without Chrome.
fn assert_bounded_read_error<T: std::fmt::Debug>(test_name: &str, result: Result<T, CrawlError>, elapsed: Duration) {
    match result {
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
        }
        Ok(response) => panic!(
            "a page that keeps its renderer main thread busy must not let the document read succeed: {response:?}"
        ),
        Err(error @ CrawlError::BrowserTimeout { .. }) => {
            let message = error.to_string();
            assert!(
                message.contains("reading the committed document"),
                "the timeout must name the stalled read, got: {message}"
            );
            assert!(
                message.contains("3s"),
                "the timeout must name the configured browser.timeout, got: {message}"
            );
            assert!(
                elapsed < Duration::from_secs(10),
                "expected the fetch to fail near the 3s read bound, took {elapsed:?} instead -- \
                 without the bound the read waits until the page frees its renderer: {message}"
            );
        }
        Err(error) => panic!(
            "expected a BrowserTimeout naming the committed-document read, got {error:?} \
             (the pre-fix signature was a BrowserError from chromiumoxide's 30s CDP timeout)"
        ),
    }
}

/// One-shot path: `scrape` must fail with a `BrowserTimeout` naming "reading the committed
/// document" and "3s", well inside the 30 s the pre-fix read inherited.
#[tokio::test]
#[serial_test::serial(browser_document_read_timeout)]
async fn fetch_fails_within_browser_timeout_when_the_renderer_is_saturated() {
    let url = spawn_saturating_renderer_server();
    let engine = create_engine(Some(read_bound_config())).expect("engine must build");

    let start = Instant::now();
    let result = scrape(&engine, &url).await;
    let elapsed = start.elapsed();

    assert_bounded_read_error(
        "fetch_fails_within_browser_timeout_when_the_renderer_is_saturated",
        result,
        elapsed,
    );
}

/// Pooled path: `pooled_fetch` runs the same `page_fetch`, so the read bound applies through
/// the pool too. Same fixture, same assertions.
#[tokio::test]
#[serial_test::serial(browser_document_read_timeout)]
async fn pooled_fetch_applies_the_same_read_bound() {
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    match pool.warm().await {
        Ok(()) => {}
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip("pooled_fetch_applies_the_same_read_bound", &message);
            return;
        }
        Err(error) => panic!("warming the pool must either succeed or report a missing Chrome: {error:?}"),
    }

    let mut config = read_bound_config();
    config.browser.session_affinity = false;
    config.browser_pool = Some(Arc::clone(&pool));

    let url = spawn_saturating_renderer_server();
    let engine = create_engine(Some(config)).expect("engine must build");

    let start = Instant::now();
    let result = scrape(&engine, &url).await;
    let elapsed = start.elapsed();

    assert_bounded_read_error("pooled_fetch_applies_the_same_read_bound", result, elapsed);
}

/// Interact path: the read of the final HTML after the actions has the same bound.
#[tokio::test]
#[serial_test::serial(browser_document_read_timeout)]
async fn interact_fails_within_browser_timeout_when_the_renderer_is_saturated() {
    let url = spawn_saturating_renderer_server();
    let engine = create_engine(Some(read_bound_config())).expect("engine must build");

    let start = Instant::now();
    let result = interact(&engine, &url, one_short_wait()).await;
    let elapsed = start.elapsed();

    assert_bounded_read_error(
        "interact_fails_within_browser_timeout_when_the_renderer_is_saturated",
        result,
        elapsed,
    );
}

/// Interact path, URL read: only the read of the final URL stalls. The requested URL redirects,
/// so a fallback to it would report a URL the page never had.
#[tokio::test]
#[serial_test::serial(browser_document_read_timeout)]
async fn interact_does_not_report_the_requested_url_when_the_url_read_times_out() {
    let url = format!("{}start", spawn_redirect_to_busy_after_read_server());
    let engine = create_engine(Some(read_bound_config())).expect("engine must build");

    let start = Instant::now();
    let result = interact(&engine, &url, one_short_wait()).await;
    let elapsed = start.elapsed();

    match result {
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(
                "interact_does_not_report_the_requested_url_when_the_url_read_times_out",
                &message,
            );
        }
        Ok(result) => panic!(
            "a read past browser.timeout must not become a result: final_url={} for requested {url}",
            result.final_url
        ),
        Err(error @ CrawlError::BrowserTimeout { .. }) => {
            let message = error.to_string();
            assert!(
                message.contains("3s"),
                "the timeout must name browser.timeout, got: {message}"
            );
            assert!(elapsed < Duration::from_secs(10), "took {elapsed:?}: {message}");
        }
        Err(error) => panic!("expected a BrowserTimeout for the stalled URL read, got {error:?}"),
    }
}
