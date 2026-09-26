//! Regression coverage for xberg-io/crawlberg#73: with `browser.endpoint` set, crawlberg
//! connects to a Chrome it did not start, and teardown used to send that Chrome a CDP
//! `Browser.close`, shutting down the caller's browser.
//!
//! Each test starts its own Chrome with a remote debugging port, points crawlberg at it by
//! endpoint, and asserts that the Chrome still answers on its port after crawlberg's teardown,
//! with only the tabs it had before. Skipped (not failed) when no Chrome is installed, matching
//! the other browser integration tests.

#![cfg(feature = "browser")]

use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crawlberg::{
    BrowserBackend, BrowserConfig, BrowserMode, BrowserPool, BrowserPoolConfig, CrawlConfig, create_engine,
};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::announce_chrome_skip;

/// A Chrome process owned by the test, reachable through its DevTools port.
struct ExternalChrome {
    child: Child,
    port: u16,
    ws_url: String,
    /// Kept only so the profile directory outlives the Chrome process that uses it.
    _user_data_dir: tempfile::TempDir,
}

impl Drop for ExternalChrome {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl ExternalChrome {
    /// Start Chrome with `--remote-debugging-port=0` and read the chosen port from the
    /// `DevToolsActivePort` file Chrome writes into its profile directory.
    fn start(test_name: &str) -> Option<Self> {
        let executable = match chromiumoxide::detection::default_executable(Default::default()) {
            Ok(path) => path,
            Err(reason) => {
                announce_chrome_skip(test_name, &reason);
                return None;
            }
        };
        let user_data_dir = tempfile::tempdir().expect("a temp profile directory must be created");
        let child = Command::new(executable)
            .args([
                "--headless=new",
                "--remote-debugging-port=0",
                "--no-sandbox",
                "--no-first-run",
                "--no-default-browser-check",
                "--use-mock-keychain",
                &format!("--user-data-dir={}", user_data_dir.path().display()),
                "about:blank",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the detected Chrome executable must start");

        let mut chrome = Self {
            child,
            port: 0,
            ws_url: String::new(),
            _user_data_dir: user_data_dir,
        };
        let stderr = chrome.child.stderr.take().expect("Chrome's stderr is piped");
        let (port, ws_url) = wait_for_devtools_url(stderr);
        chrome.port = port;
        chrome.ws_url = ws_url;
        Some(chrome)
    }

    /// Whether the Chrome process is still running and answers `/json/version` on its port.
    fn answers(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None)) && devtools_get(self.port, "/json/version").is_some()
    }

    /// The number of page targets Chrome reports on `/json/list`.
    fn page_count(&self) -> usize {
        let body = devtools_get(self.port, "/json/list").expect("Chrome must answer /json/list");
        let targets: Vec<serde_json::Value> = serde_json::from_str(&body).expect("/json/list must be a JSON array");
        targets
            .iter()
            .filter(|target| target.get("type").and_then(serde_json::Value::as_str) == Some("page"))
            .count()
    }

    /// Assert that Chrome still answers on its port for `window`, and still has `pages` tabs.
    ///
    /// ~keep One-shot teardown runs in a background task, so a single probe right after the
    /// ~keep fetch returns could run before the teardown does. Probing across a window catches
    /// ~keep a teardown that shuts Chrome down a moment later. The window sleeps on the Tokio
    /// ~keep timer, not the thread: a `#[tokio::test]` runtime has one thread, and a blocking
    /// ~keep sleep would stop the teardown task from running at all during the window.
    async fn assert_still_serving(&mut self, window: Duration, pages: usize) {
        let deadline = Instant::now() + window;
        while Instant::now() < deadline {
            assert!(
                self.answers(),
                "the external Chrome on port {} stopped answering after crawlberg's teardown",
                self.port
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        // ~keep The tab close also runs in the background task, and on a slow runner it can land
        // ~keep after `window`. Chrome must keep answering the whole time; the tab count only has
        // ~keep to reach `pages` before a generous deadline.
        let tab_deadline = Instant::now() + Duration::from_secs(15);
        while self.page_count() != pages && Instant::now() < tab_deadline {
            assert!(
                self.answers(),
                "the external Chrome on port {} stopped answering after crawlberg's teardown",
                self.port
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(
            self.page_count(),
            pages,
            "crawlberg must close the tabs it opened and no others"
        );
    }
}

/// Read the DevTools websocket URL from the `DevTools listening on ws://…` line Chrome prints on
/// stderr, and return it with its port.
///
/// ~keep The address comes from stderr, as chromiumoxide reads it, rather than from the
/// ~keep `DevToolsActivePort` file: a sandboxed Chrome (a snap build on a CI runner) writes that
/// ~keep file into its own private `/tmp`, where the test cannot see it. A thread keeps draining
/// ~keep stderr afterwards so Chrome never blocks on a full pipe.
fn wait_for_devtools_url(stderr: std::process::ChildStderr) -> (u16, String) {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut sender = Some(sender);
        for line in std::io::BufRead::lines(std::io::BufReader::new(stderr)).map_while(Result::ok) {
            if let Some(url) = line.split("DevTools listening on ").nth(1)
                && let Some(sender) = sender.take()
            {
                let _ = sender.send(url.trim().to_owned());
            }
        }
    });
    let url = receiver
        .recv_timeout(Duration::from_secs(30))
        .expect("Chrome must print its DevTools address on stderr within 30s");
    let port = url
        .strip_prefix("ws://")
        .and_then(|rest| rest.split(['/', ':']).nth(1))
        .and_then(|port| port.parse().ok())
        .unwrap_or_else(|| panic!("the DevTools address must carry a port: {url}"));
    (port, url)
}

/// A minimal HTTP GET against the DevTools port, returning the body of a 200 response.
///
/// ~keep Chrome's DevTools server keeps the connection open after it responds, so the body
/// ~keep is read up to its `Content-Length` rather than to end of stream.
fn devtools_get(port: u16, path: &str) -> Option<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    write!(stream, "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").ok()?;
    let mut response = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        response.extend_from_slice(&chunk[..read]);
        let text = String::from_utf8_lossy(&response);
        let Some((head, body)) = text.split_once("\r\n\r\n") else {
            continue;
        };
        let content_length: usize = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse().ok())?
            })
            .unwrap_or(0);
        if body.len() >= content_length {
            return head.lines().next()?.contains(" 200 ").then(|| body.to_owned());
        }
    }
}

/// Accept connections and hold each one open without a response, so a navigation stalls.
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

async fn page_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw("<html><body><p>endpoint-teardown-marker</p></body></html>", "text/html"),
        )
        .mount(&server)
        .await;
    server
}

fn endpoint_config(ws_url: &str) -> CrawlConfig {
    CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            mode: BrowserMode::Always,
            endpoint: Some(ws_url.to_owned()),
            ..BrowserConfig::default()
        },
        ..CrawlConfig::builder().allow_private_networks(true).build()
    }
}

/// A one-shot fetch through `browser.endpoint` leaves the external Chrome running.
#[tokio::test]
#[serial_test::serial(external_chrome)]
async fn one_shot_fetch_leaves_the_external_chrome_running() {
    let Some(mut chrome) = ExternalChrome::start("one_shot_fetch_leaves_the_external_chrome_running") else {
        return;
    };
    let pages_before = chrome.page_count();
    let server = page_server().await;
    let engine = create_engine(Some(endpoint_config(&chrome.ws_url))).expect("engine must build");

    let response = crawlberg::scrape(&engine, &server.uri())
        .await
        .expect("a fetch through the external Chrome must succeed");
    assert!(response.html.contains("endpoint-teardown-marker"));

    chrome.assert_still_serving(Duration::from_secs(3), pages_before).await;
}

/// A one-shot fetch that hits `overall_timeout` through `browser.endpoint` closes the tab it
/// opened in the external Chrome, even though the fetch itself was cut short.
#[tokio::test]
#[serial_test::serial(external_chrome)]
async fn one_shot_fetch_past_its_deadline_closes_its_tab_in_the_external_chrome() {
    let Some(mut chrome) =
        ExternalChrome::start("one_shot_fetch_past_its_deadline_closes_its_tab_in_the_external_chrome")
    else {
        return;
    };
    let pages_before = chrome.page_count();
    let url = spawn_stalling_server();
    let mut config = endpoint_config(&chrome.ws_url);
    config.browser.timeout = Duration::from_secs(30);
    config.browser.overall_timeout = Duration::from_secs(2);
    let engine = create_engine(Some(config)).expect("engine must build");

    let result = crawlberg::scrape(&engine, &url).await;
    assert!(
        result.is_err(),
        "a fetch against a server that never responds must fail at the overall deadline"
    );

    chrome.assert_still_serving(Duration::from_secs(3), pages_before).await;
}

/// A one-shot fetch dropped by its caller while it runs closes the tab it opened in the external
/// Chrome, and leaves that Chrome running.
///
/// ~keep Fails before xberg-io/crawlberg#131's fix, where teardown was straight-line code after
/// ~keep the fetch and an aborted task reached none of it. Dropping a connected
/// ~keep `chromiumoxide::Browser` is a no-op — it owns no child process — and its handler task
/// ~keep keeps the CDP websocket open, so the tab stayed open in the caller's Chrome. This is a
/// ~keep different path from `one_shot_fetch_past_its_deadline_closes_its_tab_in_the_external_chrome`:
/// ~keep that fetch returns normally and runs its own teardown, this one never returns at all.
#[tokio::test]
#[serial_test::serial(external_chrome)]
async fn dropping_a_one_shot_fetch_closes_its_tab_in_the_external_chrome() {
    const TEST_NAME: &str = "dropping_a_one_shot_fetch_closes_its_tab_in_the_external_chrome";
    let Some(mut chrome) = ExternalChrome::start(TEST_NAME) else {
        return;
    };
    let pages_before = chrome.page_count();
    let url = spawn_stalling_server();
    let mut config = endpoint_config(&chrome.ws_url);
    // ~keep Both deadlines outlast the test: the drop under test has to be the caller's, not
    // ~keep `overall_timeout` expiring and running the teardown on its behalf.
    config.browser.timeout = Duration::from_secs(120);
    config.browser.overall_timeout = Duration::from_secs(180);

    let fetch = tokio::spawn(async move {
        let engine = create_engine(Some(config)).expect("engine must build");
        let _ = crawlberg::scrape(&engine, &url).await;
    });

    // ~keep The tab has to exist before the drop, or the teardown under test would have nothing
    // ~keep to close and the test would pass having observed nothing.
    let tab_deadline = Instant::now() + Duration::from_secs(30);
    while chrome.page_count() == pages_before && Instant::now() < tab_deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        chrome.page_count(),
        pages_before + 1,
        "precondition: the fetch never opened a tab in the external Chrome"
    );

    fetch.abort();

    chrome.assert_still_serving(Duration::from_secs(3), pages_before).await;
}

/// Shutting down a pool connected through `browser_endpoint` leaves the external Chrome running.
#[tokio::test]
#[serial_test::serial(external_chrome)]
async fn pool_shutdown_leaves_the_external_chrome_running() {
    let Some(mut chrome) = ExternalChrome::start("pool_shutdown_leaves_the_external_chrome_running") else {
        return;
    };
    let pages_before = chrome.page_count();
    let pool = BrowserPool::new(BrowserPoolConfig {
        browser_endpoint: Some(chrome.ws_url.clone()),
        ..BrowserPoolConfig::default()
    });

    let page = pool
        .acquire_page()
        .await
        .expect("the pool must connect to the external Chrome");
    page.close().await;
    pool.shutdown().await;

    chrome.assert_still_serving(Duration::from_secs(1), pages_before).await;
}

/// A pooled page dropped without `close().await` still has its tab closed in the external
/// Chrome, because pool shutdown waits for the close `Drop` spawned.
///
/// ~keep `PooledPage::drop` cannot await, so it spawns `page.close()`. Pool teardown aborts the
/// ~keep task that owns the CDP websocket, and before `release_browser` learned to wait, that
/// ~keep abort cancelled the spawned close and left the tab open in the caller's Chrome.
/// ~keep `pool_shutdown_leaves_the_external_chrome_running` awaits `page.close()` explicitly and
/// ~keep therefore never exercised this path.
#[tokio::test]
#[serial_test::serial(external_chrome)]
async fn pool_shutdown_closes_a_pooled_tab_dropped_without_awaiting_its_close() {
    let Some(mut chrome) =
        ExternalChrome::start("pool_shutdown_closes_a_pooled_tab_dropped_without_awaiting_its_close")
    else {
        return;
    };
    let pages_before = chrome.page_count();
    let pool = BrowserPool::new(BrowserPoolConfig {
        browser_endpoint: Some(chrome.ws_url.clone()),
        ..BrowserPoolConfig::default()
    });

    let page = pool
        .acquire_page()
        .await
        .expect("the pool must connect to the external Chrome");
    assert_eq!(
        chrome.page_count(),
        pages_before + 1,
        "acquiring a pooled page must open exactly one tab in the external Chrome"
    );
    drop(page);
    pool.shutdown().await;

    chrome.assert_still_serving(Duration::from_secs(1), pages_before).await;
}

/// An interaction run through `browser.endpoint` leaves the external Chrome running.
#[cfg(feature = "interact")]
#[tokio::test]
#[serial_test::serial(external_chrome)]
async fn interact_leaves_the_external_chrome_running() {
    let Some(mut chrome) = ExternalChrome::start("interact_leaves_the_external_chrome_running") else {
        return;
    };
    let pages_before = chrome.page_count();
    let server = page_server().await;
    let engine = create_engine(Some(endpoint_config(&chrome.ws_url))).expect("engine must build");

    let result = crawlberg::interact(&engine, &server.uri(), vec![crawlberg::PageAction::Scrape])
        .await
        .expect("an interaction through the external Chrome must succeed");
    assert!(result.final_html.contains("endpoint-teardown-marker"));

    chrome.assert_still_serving(Duration::from_secs(1), pages_before).await;
}
