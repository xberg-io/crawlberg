//! Regression coverage for xberg-io/crawlberg#567: `browser.timeout` bounded only the
//! navigation block (`goto` + `wait_for_ready`); the post-navigation reads of the committed
//! document (`page.content()`, `page.url()`) inherited chromiumoxide's fixed 30 s CDP timeout.
//! Chrome holds renderer commands while a main-frame navigation is in flight, so a
//! self-replacing page stalled every read for the full 30 s. Both fetch paths must now fail
//! within a fresh `browser.timeout` budget, with a `BrowserTimeout` naming the read.
//!
//! Requires a real Chrome binary (found at `/Applications/Google Chrome.app` in this
//! environment; chromiumoxide auto-detects it) and is gated behind the `browser` feature;
//! skipped (not failed) when Chrome is unavailable, matching the sibling deadline tests.

#![cfg(feature = "browser")]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crawlberg::{
    BrowserBackend, BrowserConfig, BrowserMode, BrowserPool, BrowserPoolConfig, CrawlConfig, CrawlError, ScrapeResult,
    create_engine, scrape,
};

mod common;
use common::{announce_chrome_skip, is_missing_chrome_message};

/// Serves the #526 page shape: the initial document commits normally, then replaces itself
/// every 40 ms with a target this server accepts and NEVER answers -- so a main-frame
/// navigation is always in flight and Chrome holds every renderer command. Deterministic:
/// there is no window in which a document read would answer, so neither a sleep nor a race
/// decides the outcome. Connections are answered (if the document) and then held open for
/// hours, never dropped with unread bytes in flight: dropping in that state can send a TCP
/// RST, which would fail a navigation fast instead of leaving one in flight forever -- see
/// `test_browser_overall_deadline.rs::spawn_stalling_server` for why holding is load-bearing.
fn spawn_document_never_settles_server() -> String {
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
                if request_line.split_whitespace().nth(1) == Some("/") {
                    let body = "<!doctype html><html><head><title>never-settles</title></head><body><p>committed</p>\
                                <script>setInterval(function () { location.replace('/hang'); }, 40);</script>\
                                </body></html>";
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    let mut writer = write_half;
                    let _ = writer.write_all(response.as_bytes());
                    let _ = writer.flush();
                }
                // Hold every connection open for the test's lifetime, whether answered or not.
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

/// Shared by both tests: a `BrowserTimeout` naming the read and the budget, in well under the
/// 30 s the pre-fix reads stalled to. Skips (does not fail) on runners without Chrome.
fn assert_bounded_read_error(test_name: &str, result: Result<ScrapeResult, CrawlError>, elapsed: Duration) {
    match result {
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
        }
        Ok(response) => panic!(
            "a page whose document reads are held behind an always in-flight navigation must not succeed: {response:?}"
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
                 pre-fix the read stalls to chromiumoxide's internal 30s: {message}"
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
async fn fetch_fails_within_browser_timeout_when_the_document_never_settles() {
    let url = spawn_document_never_settles_server();
    let engine = create_engine(Some(read_bound_config())).expect("engine must build");

    let start = Instant::now();
    let result = scrape(&engine, &url).await;
    let elapsed = start.elapsed();

    assert_bounded_read_error(
        "fetch_fails_within_browser_timeout_when_the_document_never_settles",
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

    let url = spawn_document_never_settles_server();
    let engine = create_engine(Some(config)).expect("engine must build");

    let start = Instant::now();
    let result = scrape(&engine, &url).await;
    let elapsed = start.elapsed();

    assert_bounded_read_error("pooled_fetch_applies_the_same_read_bound", result, elapsed);
}
