//! Chrome-backed regression tests for `CrawlConfig.capture_screenshot` on the
//! chromiumoxide fetch path.
//!
//! Before this change, `capture_screenshot` was accepted by every binding but did
//! nothing anywhere on the scrape/crawl fetch path: `HttpResponse` had no screenshot
//! field, and `browser.rs::page_fetch` never called a screenshot API. These tests
//! prove `scrape()` now returns real PNG bytes when the flag is set, that it stays
//! `None` when the flag is unset, and that the base64 field round-trips the same
//! bytes.
//!
//! Requires a real Chrome binary (found at `/Applications/Google Chrome.app` in this
//! environment; chromiumoxide auto-detects it) and is gated behind the `browser`
//! feature, matching `test_browser_pool_lifecycle.rs`.

#![cfg(feature = "browser")]

use std::sync::OnceLock;
use std::time::Duration;

use crawlberg::{BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig, CrawlError, create_engine, scrape};

mod common;
use common::{announce_chrome_skip, is_missing_chrome_message};

static ALLOW_PRIVATE: OnceLock<()> = OnceLock::new();

/// Opts into the SSRF policy's private-network allowance so the loopback test
/// server below is reachable.
fn allow_private_network() {
    ALLOW_PRIVATE.get_or_init(|| {
        // ~keep SAFETY: OnceLock writes this env var once before any network call is made.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "1");
        }
    });
}

fn chromiumoxide_config(capture_screenshot: bool) -> CrawlConfig {
    allow_private_network();
    CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            mode: BrowserMode::Always,
            timeout: Duration::from_secs(20),
            ..BrowserConfig::default()
        },
        capture_screenshot,
        ..CrawlConfig::default()
    }
}

/// A minimal HTML page served over loopback so the chromiumoxide backend has
/// something real to navigate to and screenshot.
const PAGE_BODY: &str = "<html><body style=\"background:#ff0000\"><h1>screenshot-marker</h1></body></html>";

async fn start_server() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test server should bind");
    let addr = listener.local_addr().expect("test server should have local addr");

    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buffer = [0_u8; 1024];
                let _ = stream.read(&mut buffer).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    PAGE_BODY.len(),
                    PAGE_BODY
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });

    format!("http://{addr}")
}

/// PNG files always start with this 8-byte magic-number signature.
const PNG_MAGIC: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

/// With `capture_screenshot: true` and `BrowserBackend::Chromiumoxide` +
/// `BrowserMode::Always`, `scrape()` must return real PNG bytes (verified by magic
/// number, not merely `Some(..)`), and `screenshot_base64` must decode back to the
/// exact same bytes.
#[tokio::test]
async fn scrape_captures_a_real_png_screenshot_when_requested() {
    let base_url = start_server().await;
    let engine = create_engine(Some(chromiumoxide_config(true))).expect("engine must build");

    let result = scrape(&engine, &base_url).await;
    let result = match result {
        Ok(result) => result,
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip("scrape_captures_a_real_png_screenshot_when_requested", &message);
            return;
        }
        Err(error) => panic!("scrape must succeed: {error:?}"),
    };

    assert!(
        result.html.contains("screenshot-marker"),
        "page must have actually rendered, got html: {}",
        result.html
    );

    let screenshot = result
        .screenshot
        .as_ref()
        .expect("screenshot must be Some when capture_screenshot is true");
    assert!(
        screenshot.len() > PNG_MAGIC.len(),
        "screenshot must contain real image data, got {} bytes",
        screenshot.len()
    );
    assert_eq!(
        &screenshot[..PNG_MAGIC.len()],
        &PNG_MAGIC,
        "screenshot bytes must start with the PNG magic number, got {:02x?}",
        &screenshot[..PNG_MAGIC.len().min(screenshot.len())]
    );

    let screenshot_base64 = result
        .screenshot_base64
        .as_ref()
        .expect("screenshot_base64 must be Some when capture_screenshot is true");
    use base64::Engine as _;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(screenshot_base64)
        .expect("screenshot_base64 must be valid base64");
    assert_eq!(
        &decoded, screenshot,
        "screenshot_base64 must decode back to the exact same bytes as screenshot"
    );
}

/// With `capture_screenshot: false` (the default), no screenshot is captured even
/// though the browser is used for every request.
#[tokio::test]
async fn scrape_does_not_capture_a_screenshot_when_not_requested() {
    let base_url = start_server().await;
    let engine = create_engine(Some(chromiumoxide_config(false))).expect("engine must build");

    let result = scrape(&engine, &base_url).await;
    let result = match result {
        Ok(result) => result,
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip("scrape_does_not_capture_a_screenshot_when_not_requested", &message);
            return;
        }
        Err(error) => panic!("scrape must succeed: {error:?}"),
    };

    assert!(
        result.html.contains("screenshot-marker"),
        "page must have actually rendered, got html: {}",
        result.html
    );
    assert!(
        result.screenshot.is_none(),
        "screenshot must be None when capture_screenshot is false, got {} bytes",
        result.screenshot.map(|b| b.len()).unwrap_or(0)
    );
    assert!(
        result.screenshot_base64.is_none(),
        "screenshot_base64 must be None when capture_screenshot is false"
    );
}
