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

use std::net::TcpListener;
use std::time::{Duration, Instant};

use crawlberg::{
    BrowserBackend, BrowserConfig, BrowserMode, BrowserPool, BrowserPoolConfig, CrawlConfig, CrawlError, create_engine,
    scrape,
};

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
async fn pooled_fetch_fails_near_the_overall_deadline_when_navigation_stalls() {
    let url = spawn_stalling_server();
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    let engine = create_engine(Some(stalled_pooled_config(pool))).expect("engine must build");

    let start = Instant::now();
    let result = scrape(&engine, &url).await;
    let elapsed = start.elapsed();

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
