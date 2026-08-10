//! Chrome-backed regression tests for the `BrowserPool` page-lifecycle bug.
//!
//! `crates/crawlberg/src/browser.rs` used to acquire a `PooledPage` guard from the
//! pool and immediately drop it as the tail expression of an `if`/`else` arm:
//!
//! ```ignore
//! let page = if config.browser.session_affinity {
//!     ...
//! } else {
//!     let pooled = pool.acquire_page().await?;
//!     pooled.page().clone()   // `pooled` (the PooledPage guard) drops right here
//! };
//! let result = page_fetch(url, config, &page, prior_cookies).await; // ...before this runs
//! ```
//!
//! `PooledPage::drop` releases the `BrowserPool` semaphore permit AND spawns a CDP
//! `Target.closeTarget` against the *same* page that `page_fetch` is about to
//! navigate. Two consequences, both exercised below:
//!
//! 1. The permit is released before the page is actually used, so `max_pages` only
//!    throttles *acquisition*, not concurrent in-flight navigations.
//! 2. `Target.closeTarget` races the navigation that starts moments later.
//!
//! These tests require a real Chrome binary (found at `/Applications/Google
//! Chrome.app` in this environment; chromiumoxide auto-detects it) and are gated
//! behind the `browser` feature, matching the `#![cfg(feature = "...")]` gating
//! pattern used by the other browser integration tests in this directory (e.g.
//! `test_browser_native.rs`).

#![cfg(feature = "browser")]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crawlberg::{
    BrowserBackend, BrowserConfig, BrowserMode, BrowserPool, BrowserPoolConfig, CrawlConfig, batch_scrape,
    create_engine, scrape,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

static ALLOW_PRIVATE: OnceLock<()> = OnceLock::new();

/// Opts into the SSRF policy's private-network allowance so the loopback test
/// server below is reachable. Without this the engine's SSRF check rejects
/// `127.0.0.1` before the pool-lifecycle behaviour under test ever runs.
fn allow_private_network() {
    ALLOW_PRIVATE.get_or_init(|| {
        // ~keep SAFETY: OnceLock writes this env var once before any network call is made.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "1");
        }
    });
}

fn pool_config(pool: Arc<BrowserPool>, max_concurrent: Option<usize>) -> CrawlConfig {
    allow_private_network();
    CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            mode: BrowserMode::Always,
            // ~keep session_affinity off isolates the bug: every fetch goes through
            // ~keep `pool.acquire_page()` -> `into_parts()`, the exact path that used to race.
            session_affinity: false,
            timeout: Duration::from_secs(20),
            ..BrowserConfig::default()
        },
        browser_pool: Some(pool),
        max_concurrent,
        ..CrawlConfig::default()
    }
}

/// Repeatedly fetch through a single-page pool. Before the fix this failed
/// intermittently (frequently every run) with a CDP error such as "Session
/// with given id not found" or a navigation error, because the page acquired
/// from the pool was closed by `PooledPage::drop` while `page_fetch` was
/// still mid-navigation on it. After the fix the guard (and its permit) is
/// held across `page_fetch`, so every iteration must succeed with the exact
/// expected body.
#[tokio::test]
async fn pool_page_survives_sequential_fetches_without_being_closed_mid_navigation() {
    let server = TestServer::start().await;
    let pool = BrowserPool::new(BrowserPoolConfig {
        max_pages: 1,
        ..BrowserPoolConfig::default()
    });
    let engine = create_engine(Some(pool_config(pool, None))).expect("engine must build");

    for iteration in 0..6 {
        let url = format!("{}/iter-{iteration}", server.base_url);
        let result = scrape(&engine, &url).await;
        assert!(
            result.is_ok(),
            "iteration {iteration}: scrape must succeed, got {:?}",
            result.err()
        );
        let html = result.unwrap().html;
        assert!(
            html.contains("pool-lifecycle-marker"),
            "iteration {iteration}: response body must contain the expected marker, got: {html}"
        );
    }
}

/// `max_pages` must bound the number of *concurrently navigating* tabs, not
/// merely the rate at which pages are acquired. Before the fix the semaphore
/// permit was released the instant `acquire_page()` returned — well before
/// the page actually navigated — so a burst of concurrent fetches against a
/// pool configured for a single page could still open multiple simultaneous
/// Chrome navigations. The test server below records the peak number of
/// concurrently open connections it observed; with `max_pages: 1` that peak
/// must never exceed 1.
#[tokio::test]
async fn pool_bounds_concurrent_navigations_to_max_pages() {
    let server = TestServer::start().await;
    let pool = BrowserPool::new(BrowserPoolConfig {
        max_pages: 1,
        ..BrowserPoolConfig::default()
    });
    let engine = create_engine(Some(pool_config(pool, Some(4)))).expect("engine must build");

    let urls = (0..4)
        .map(|index| format!("{}/concurrent-{index}", server.base_url))
        .collect::<Vec<_>>();
    let results = batch_scrape(&engine, urls).await.expect("batch_scrape must run");

    assert_eq!(results.total_count, 4, "all four URLs must have been attempted");
    for entry in &results.results {
        assert!(
            entry.result.is_some() && entry.error.is_none(),
            "url {}: expected success, got error {:?}",
            entry.url,
            entry.error
        );
    }

    let peak = server.max_in_flight.load(Ordering::SeqCst);
    assert_eq!(
        peak, 1,
        "max_pages=1 must bound concurrent navigations to 1, but the server observed {peak} concurrent connections"
    );
}

/// Minimal raw HTTP server that tracks the peak number of concurrently open
/// connections it has served, and holds each connection open for a fixed
/// delay before responding — long enough that overlapping navigations would
/// be observed as overlapping connections. Mirrors the `TestServer` pattern
/// already used in `test_browser_native.rs`.
struct TestServer {
    base_url: String,
    max_in_flight: Arc<AtomicUsize>,
}

impl TestServer {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("test server should bind");
        let addr = listener.local_addr().expect("test server should have local addr");
        let current = Arc::new(AtomicUsize::new(0));
        let max_in_flight = Arc::new(AtomicUsize::new(0));
        let current_for_task = current.clone();
        let max_for_task = max_in_flight.clone();

        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let current = current_for_task.clone();
                let max_in_flight = max_for_task.clone();
                tokio::spawn(async move {
                    let mut buffer = [0_u8; 1024];
                    let n = stream.read(&mut buffer).await.unwrap_or(0);
                    let request_line = String::from_utf8_lossy(&buffer[..n]);

                    // ~keep Chrome opens speculative/preconnect TCP connections around a
                    // ~keep navigation (and auto-requests /favicon.ico) independent of how many
                    // ~keep tabs are navigating. Neither is evidence of the pool exceeding
                    // ~keep max_pages, so only a real GET for our test path counts toward the
                    // ~keep peak: an empty read means the peer closed without sending a request.
                    if n == 0 || request_line.starts_with("GET /favicon.ico") {
                        let response = "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
                        let _ = stream.write_all(response.as_bytes()).await;
                        let _ = stream.shutdown().await;
                        return;
                    }

                    let active = current.fetch_add(1, Ordering::SeqCst) + 1;
                    max_in_flight.fetch_max(active, Ordering::SeqCst);

                    // ~keep Held open long enough to overlap with a second concurrent
                    // ~keep navigation if the pool's concurrency bound is not enforced.
                    tokio::time::sleep(Duration::from_millis(250)).await;

                    let body = "<html><body>pool-lifecycle-marker</body></html>";
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.shutdown().await;
                    current.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });

        Self {
            base_url: format!("http://{addr}"),
            max_in_flight,
        }
    }
}
