//! `BrowserConfig.chrome_path` and `chrome_args` are ignored, with a warning, when no Chrome
//! is launched from them: an external `browser.endpoint`, the native backend, and a scrape
//! through a shared browser pool. Each test captures the warning the fetch logs. None of them
//! needs Chrome.
//!
//! The `interact_on_*` tests below cover the same two warnings on the `interact()` entry
//! point (`interact/chromiumoxide.rs` and `interact/native.rs`), which is a separate call
//! site from `scrape()`'s and had no coverage of its own.

#![cfg(any(feature = "browser", feature = "browser-native"))]

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crawlberg::{BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig, PageAction, create_engine, interact, scrape};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const IGNORED_WARNING: &str = "browser.chrome_path and browser.chrome_args are ignored when";

/// Every log line written while the returned guard is alive, on this thread.
#[derive(Clone, Default)]
struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

impl Write for CapturedLogs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("log buffer lock").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl CapturedLogs {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("log buffer lock")).into_owned()
    }
}

/// Install a thread-local subscriber that records WARN and above. The tests run on tokio's
/// current-thread runtime, so every task the fetch spawns logs through it.
fn capture_warnings() -> (CapturedLogs, tracing::subscriber::DefaultGuard) {
    let logs = CapturedLogs::default();
    let writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    (logs, tracing::subscriber::set_default(subscriber))
}

/// Launch options that would refuse the config if they were checked: the path does not exist.
fn ignored_launch_options(browser: BrowserConfig) -> BrowserConfig {
    BrowserConfig {
        mode: BrowserMode::Always,
        timeout: Duration::from_secs(5),
        overall_timeout: Duration::from_secs(10),
        chrome_path: Some("/nonexistent/crawlberg-ignored-chrome".into()),
        chrome_args: vec!["--user-agent=crawlberg-ignored".to_owned()],
        ..browser
    }
}

async fn start_page_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("test server should bind");
    let addr = listener.local_addr().expect("test server should have local addr");
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buffer = [0_u8; 4096];
                let _ = stream.read(&mut buffer).await;
                let body = "<html><body><p>ignored-launch-options-page</p></body></html>";
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    format!("http://{addr}/")
}

#[cfg(feature = "browser")]
#[tokio::test]
async fn an_external_endpoint_ignores_the_launch_options_with_a_warning() {
    // ~keep Nothing listens on port 1, so the connect fails after the warning is logged.
    let config = CrawlConfig {
        browser: ignored_launch_options(BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            endpoint: Some("ws://127.0.0.1:1/devtools/browser/crawlberg-test".to_owned()),
            ..BrowserConfig::default()
        }),
        ..CrawlConfig::builder().allow_private_networks(true).build()
    };
    let (logs, _guard) = capture_warnings();
    let engine = create_engine(Some(config)).expect("an endpoint config must not be refused for ignored options");
    let url = start_page_server().await;
    let _ = scrape(&engine, &url).await;
    let logs = logs.text();
    assert!(
        logs.contains(IGNORED_WARNING) && logs.contains("browser.endpoint"),
        "the endpoint path must warn that the launch options are ignored; logs: {logs}"
    );
}

#[cfg(feature = "browser-native")]
#[tokio::test]
async fn the_native_backend_ignores_the_launch_options_with_a_warning() {
    let config = CrawlConfig {
        browser: ignored_launch_options(BrowserConfig {
            backend: BrowserBackend::Native,
            ..BrowserConfig::default()
        }),
        ..CrawlConfig::builder().allow_private_networks(true).build()
    };
    let (logs, _guard) = capture_warnings();
    let engine = create_engine(Some(config)).expect("a native config must not be refused for ignored options");
    let url = start_page_server().await;
    let _ = scrape(&engine, &url).await;
    let logs = logs.text();
    assert!(
        logs.contains(IGNORED_WARNING) && logs.contains("native browser backend"),
        "the native path must warn that the launch options are ignored; logs: {logs}"
    );
}

#[cfg(feature = "browser")]
#[tokio::test]
async fn a_shared_browser_pool_ignores_the_launch_options_with_a_warning() {
    // ~keep The pool's own chrome_path does not exist, so page acquisition fails without
    // ~keep Chrome, after the per-fetch warning is logged.
    let pool = crawlberg::BrowserPool::new(crawlberg::BrowserPoolConfig {
        chrome_path: Some("/nonexistent/crawlberg-pool-chrome".into()),
        ..crawlberg::BrowserPoolConfig::default()
    });
    // ~keep The config's own chrome_path stays unset: `interact()` ignores the pool and
    // ~keep launches from these fields, so validation still checks them with a pool.
    let config = CrawlConfig {
        browser: BrowserConfig {
            chrome_path: None,
            ..ignored_launch_options(BrowserConfig {
                backend: BrowserBackend::Chromiumoxide,
                session_affinity: false,
                ..BrowserConfig::default()
            })
        },
        browser_pool: Some(pool),
        ..CrawlConfig::builder().allow_private_networks(true).build()
    };
    let (logs, _guard) = capture_warnings();
    let engine = create_engine(Some(config)).expect("a pooled config with valid launch options must build");
    let url = start_page_server().await;
    let _ = scrape(&engine, &url).await;
    let logs = logs.text();
    assert!(
        logs.contains(IGNORED_WARNING) && logs.contains("browser_pool"),
        "the pool path must warn that the launch options are ignored; logs: {logs}"
    );
}

#[cfg(feature = "browser")]
#[tokio::test]
async fn interact_on_an_external_endpoint_ignores_the_launch_options_with_a_warning() {
    // ~keep Nothing listens on port 1, so the connect fails after the warning is logged.
    let config = CrawlConfig {
        browser: ignored_launch_options(BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            endpoint: Some("ws://127.0.0.1:1/devtools/browser/crawlberg-test".to_owned()),
            ..BrowserConfig::default()
        }),
        ..CrawlConfig::builder().allow_private_networks(true).build()
    };
    let (logs, _guard) = capture_warnings();
    let engine = create_engine(Some(config)).expect("an endpoint config must not be refused for ignored options");
    let url = start_page_server().await;
    let _ = interact(&engine, &url, vec![PageAction::Scrape]).await;
    let logs = logs.text();
    assert!(
        logs.contains(IGNORED_WARNING) && logs.contains("browser.endpoint"),
        "interact() on the endpoint path must warn that the launch options are ignored; logs: {logs}"
    );
}

#[cfg(feature = "browser-native")]
#[tokio::test]
async fn interact_on_the_native_backend_ignores_the_launch_options_with_a_warning() {
    let config = CrawlConfig {
        browser: ignored_launch_options(BrowserConfig {
            backend: BrowserBackend::Native,
            ..BrowserConfig::default()
        }),
        ..CrawlConfig::builder().allow_private_networks(true).build()
    };
    let (logs, _guard) = capture_warnings();
    let engine = create_engine(Some(config)).expect("a native config must not be refused for ignored options");
    let url = start_page_server().await;
    let _ = interact(&engine, &url, vec![PageAction::Scrape]).await;
    let logs = logs.text();
    assert!(
        logs.contains(IGNORED_WARNING) && logs.contains("native browser backend"),
        "interact() on the native path must warn that the launch options are ignored; logs: {logs}"
    );
}
