//! Regression coverage for xberg-io/crawlberg#66: a one-shot chromiumoxide fetch
//! had no deadline covering the WHOLE operation -- launch, page setup,
//! navigation, and shutdown -- only navigation itself (`BrowserConfig::timeout`)
//! was time-bounded. This proves `BrowserConfig::overall_timeout` bounds the
//! fetch even when a stalled navigation would otherwise run for the full
//! (deliberately much longer) `timeout`.
//!
//! Requires a real Chrome binary (found at `/Applications/Google Chrome.app` in
//! this environment; chromiumoxide auto-detects it) and is gated behind the
//! `browser` feature; skipped (not failed) when Chrome is unavailable, matching
//! `test_browser_pool_lifecycle.rs` and friends.

#![cfg(feature = "browser")]

use std::net::TcpListener;
use std::time::{Duration, Instant};

use crawlberg::{BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig, CrawlError, create_engine, scrape};

mod common;
use common::{announce_chrome_skip, is_missing_chrome_message};

/// Binds a TCP listener that accepts connections and holds each one open
/// without ever reading or writing a byte, so a browser navigation to it
/// stalls indefinitely rather than failing fast with a connection error. Runs
/// on plain OS threads (not tokio) so it keeps accepting independent of the
/// test's async runtime.
fn spawn_stalling_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("test server should bind");
    let addr = listener.local_addr().expect("test server should have local addr");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            // ~keep Held open on its own thread and never read from: on some platforms,
            // ~keep dropping a socket with unread request bytes still in its receive
            // ~keep buffer sends a TCP RST (net::ERR_CONNECTION_RESET) instead of hanging,
            // ~keep which defeated an earlier version of this test that read one byte and
            // ~keep returned. Holding the stream alive with no I/O keeps the connection
            // ~keep open with no RST and no response, which is what actually stalls
            // ~keep `page.goto`.
            std::thread::spawn(move || {
                let _stream = stream;
                std::thread::sleep(Duration::from_secs(3600));
            });
        }
    });
    format!("http://{addr}/")
}

fn stalled_navigation_config() -> CrawlConfig {
    CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            mode: BrowserMode::Always,
            // ~keep Deliberately much longer than overall_timeout below: this proves the
            // ~keep NEW overall deadline is what bounds the fetch, not that the pre-existing
            // ~keep per-navigation timeout happens to also be short.
            timeout: Duration::from_secs(30),
            overall_timeout: Duration::from_secs(2),
            ..BrowserConfig::default()
        },
        ..CrawlConfig::builder().allow_private_networks(true).build()
    }
}

/// A fetch against a server that never responds must fail near
/// `overall_timeout` (2s), not run for the full navigation `timeout` (30s).
#[tokio::test]
async fn one_shot_fetch_fails_near_the_overall_deadline_when_navigation_stalls() {
    let url = spawn_stalling_server();
    let engine = create_engine(Some(stalled_navigation_config())).expect("engine must build");

    let start = Instant::now();
    let result = scrape(&engine, &url).await;
    let elapsed = start.elapsed();

    match result {
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(
                "one_shot_fetch_fails_near_the_overall_deadline_when_navigation_stalls",
                &message,
            );
        }
        Ok(response) => panic!("a fetch against a server that never responds must not succeed: {response:?}"),
        Err(error) => {
            assert!(
                elapsed < Duration::from_secs(10),
                "expected the fetch to fail near overall_timeout (2s), took {elapsed:?} instead \
                 -- the per-navigation `timeout` (30s) must not be the effective bound: {error}"
            );
        }
    }
}
