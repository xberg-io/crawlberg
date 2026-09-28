//! A Chrome render goes through the configured proxy and never direct, and Chrome refuses a
//! proxy with credentials before it starts.
//!
//! The targets are `.test` hosts no resolver answers, so a direct connection cannot return a
//! page: the only way a render gets one is through a proxy this test runs, and the marker in
//! the page names which proxy. Requires a real Chrome binary and is gated behind
//! the `browser` feature; skipped (not failed) when Chrome is unavailable, like the other
//! browser tests.

#![cfg(feature = "browser")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crawlberg::{
    BrowserBackend, BrowserConfig, BrowserMode, BrowserPool, BrowserPoolConfig, BrowserSessionPool, CrawlConfig,
    CrawlError, HostMatcher, PageAction, ProxyConfig, ScrapeResult, SessionKey, create_engine, interact, scrape,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

mod common;
use common::{announce_chrome_skip, is_missing_chrome_message};

const TARGET: &str = "http://render-target.test/";
const MARKER: &str = "served-by-the-test-proxy";
/// Each crawl on the shared pool renders its own host through its own proxy.
const TARGET_A: &str = "http://crawl-a.test/";
const TARGET_B: &str = "http://crawl-b.test/";

/// What the proxy saw: each request line, in order.
type Seen = Arc<Mutex<Vec<String>>>;

/// An HTTP proxy that answers every request itself with [`MARKER`].
async fn spawn_proxy() -> (String, Seen) {
    spawn_named_proxy(MARKER).await
}

/// [`spawn_proxy`], answering with `marker` so a page shows which proxy served it.
async fn spawn_named_proxy(marker: &'static str) -> (String, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind the proxy");
    let address = listener.local_addr().expect("proxy address").to_string();
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let record = Arc::clone(&record);
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut buf = [0_u8; 4096];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => head.extend_from_slice(&buf[..n]),
                    }
                }
                let head = String::from_utf8_lossy(&head).into_owned();
                let request_line = head.lines().next().unwrap_or_default().to_owned();
                record.lock().expect("record").push(request_line);
                let body = format!("<html><body><p>{marker}</p></body></html>");
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    (address, seen)
}

fn render_config(browser_proxy: ProxyConfig) -> CrawlConfig {
    let mut config = CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            mode: BrowserMode::Always,
            timeout: Duration::from_secs(20),
            proxy: Some(browser_proxy),
            ..BrowserConfig::default()
        },
        respect_robots_txt: false,
        ..CrawlConfig::default()
    };
    for host in ["render-target.test", "crawl-a.test", "crawl-b.test"] {
        config.ssrf.allowlist.push(HostMatcher::exact(host));
    }
    config
}

fn proxy_at(url: String, username: Option<&str>, password: Option<&str>) -> ProxyConfig {
    ProxyConfig {
        url,
        username: username.map(Into::into),
        password: password.map(Into::into),
    }
}

/// Render [`TARGET`], or `None` when no usable Chrome exists on this host.
async fn render(test_name: &str, config: CrawlConfig, seen: &Seen) -> Option<ScrapeResult> {
    render_url(test_name, config, TARGET, seen).await
}

/// Render `url`, or `None` when no usable Chrome exists on this host.
async fn render_url(test_name: &str, config: CrawlConfig, url: &str, seen: &Seen) -> Option<ScrapeResult> {
    let engine = create_engine(Some(config)).expect("engine must build");
    match tokio::time::timeout(Duration::from_secs(60), scrape(&engine, url))
        .await
        .expect("the render must finish within 60s")
    {
        Ok(result) => Some(result),
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
            None
        }
        Err(error) => panic!(
            "{test_name}: the render must succeed through the proxy: {error:?}; the proxy saw {:?}",
            seen.lock().expect("record")
        ),
    }
}

fn target_requests(seen: &Seen) -> Vec<String> {
    requests_for(seen, TARGET)
}

fn requests_for(seen: &Seen, target: &str) -> Vec<String> {
    seen.lock()
        .expect("record")
        .iter()
        .filter(|line| line.starts_with(&format!("GET {target} ")))
        .cloned()
        .collect()
}

#[tokio::test]
async fn a_chrome_render_goes_through_the_browser_proxy() {
    let (address, seen) = spawn_proxy().await;
    let Some(result) = render(
        "a_chrome_render_goes_through_the_browser_proxy",
        render_config(proxy_at(address, None, None)),
        &seen,
    )
    .await
    else {
        return;
    };
    assert!(
        result.html.contains(MARKER),
        "the page must come from the proxy, got {}",
        result.html
    );
    assert!(
        !target_requests(&seen).is_empty(),
        "the proxy must see the render's request, saw {:?}",
        seen.lock().expect("record")
    );
}

#[tokio::test]
async fn two_crawls_on_one_pool_each_go_through_their_own_proxy() {
    let (address_a, seen_a) = spawn_named_proxy("served-by-proxy-a").await;
    let (address_b, seen_b) = spawn_named_proxy("served-by-proxy-b").await;
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    let pooled = |address: String| {
        let mut config = render_config(proxy_at(address, None, None));
        config.browser.session_affinity = false;
        config.browser_pool = Some(Arc::clone(&pool));
        config
    };
    let name = "two_crawls_on_one_pool_each_go_through_their_own_proxy";
    let (a, b) = tokio::join!(
        render_url(name, pooled(address_a), TARGET_A, &seen_a),
        render_url(name, pooled(address_b), TARGET_B, &seen_b),
    );
    pool.shutdown().await;
    let (Some(a), Some(b)) = (a, b) else {
        return;
    };
    assert!(
        a.html.contains("served-by-proxy-a"),
        "crawl A must go through proxy A, got {}",
        a.html
    );
    assert!(
        b.html.contains("served-by-proxy-b"),
        "crawl B must go through proxy B, got {}",
        b.html
    );
    assert!(!requests_for(&seen_a, TARGET_A).is_empty(), "proxy A must see crawl A");
    assert!(!requests_for(&seen_b, TARGET_B).is_empty(), "proxy B must see crawl B");
    assert!(
        requests_for(&seen_a, TARGET_B).is_empty(),
        "proxy A must not see crawl B"
    );
    assert!(
        requests_for(&seen_b, TARGET_A).is_empty(),
        "proxy B must not see crawl A"
    );
}

#[tokio::test]
async fn an_interact_session_goes_through_the_proxy() {
    let (address, seen) = spawn_proxy().await;
    let engine = create_engine(Some(render_config(proxy_at(address, None, None)))).expect("engine must build");
    let result = match tokio::time::timeout(
        Duration::from_secs(60),
        interact(&engine, TARGET, vec![PageAction::Scrape]),
    )
    .await
    .expect("the interact session must finish within 60s")
    {
        Ok(result) => result,
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip("an_interact_session_goes_through_the_proxy", &message);
            return;
        }
        Err(error) => panic!(
            "the interact session must succeed through the proxy: {error:?}; the proxy saw {:?}",
            seen.lock().expect("record")
        ),
    };
    assert!(result.final_html.contains(MARKER), "got {}", result.final_html);
    assert!(
        !target_requests(&seen).is_empty(),
        "the proxy must see the session's request"
    );
}

#[tokio::test]
async fn a_chrome_proxy_with_credentials_is_refused_before_chrome_starts() {
    let (address, seen) = spawn_proxy().await;
    for (label, crawl_proxy, browser_proxy) in [
        (
            "credentials in the browser proxy address",
            None,
            Some(proxy_at(format!("operator:s3cr3t@{address}"), None, None)),
        ),
        (
            "credential fields on the browser proxy",
            None,
            Some(proxy_at(address.clone(), Some("operator"), Some("s3cr3t"))),
        ),
        (
            "credentials on the crawl-wide proxy Chrome falls back to",
            Some(proxy_at(format!("http://operator:s3cr3t@{address}"), None, None)),
            None,
        ),
    ] {
        let mut config = render_config(proxy_at(address.clone(), None, None));
        config.proxy = crawl_proxy;
        config.browser.proxy = browser_proxy;
        let error = match create_engine(Some(config)) {
            Err(error) => error,
            Ok(engine) => match interact(&engine, TARGET, vec![PageAction::Scrape]).await {
                Err(error) => error,
                Ok(_) => panic!("{label}: Chrome cannot use a proxy with credentials"),
            },
        };
        let shown = error.to_string();
        assert!(shown.contains("username or password"), "{label}: got {shown}");
        assert!(
            !shown.contains("s3cr3t"),
            "{label}: the password must not be shown, got {shown}"
        );
    }
    assert!(
        seen.lock().expect("record").is_empty(),
        "a refused proxy must see no request"
    );
}

#[tokio::test]
async fn a_parked_page_is_not_reused_for_a_crawl_with_another_proxy() {
    let (address_a, seen_a) = spawn_named_proxy("served-by-proxy-a").await;
    let (address_b, seen_b) = spawn_named_proxy("served-by-proxy-b").await;
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    let sessions = Arc::new(BrowserSessionPool::new());
    let affine = |address: String| {
        let mut config = render_config(proxy_at(address, None, None));
        config.proxy = config.browser.proxy.take();
        config.browser.session_affinity = true;
        config.browser_pool = Some(Arc::clone(&pool));
        config.browser_session_pool = Some(Arc::clone(&sessions));
        config
    };
    let name = "a_parked_page_is_not_reused_for_a_crawl_with_another_proxy";
    let key_a = SessionKey::from_url(TARGET, Some(&format!("http://{address_a}"))).expect("a session key");
    let Some(first) = render_url(name, affine(address_a), TARGET, &seen_a).await else {
        return;
    };
    assert!(first.html.contains("served-by-proxy-a"), "got {}", first.html);
    let Some(second) = render_url(name, affine(address_b), TARGET, &seen_b).await else {
        return;
    };
    let parked_a = sessions.acquire(&key_a).await;
    assert!(
        parked_a.is_some(),
        "the first crawl's page must stay parked under proxy A's address"
    );
    drop(parked_a);
    sessions.shutdown().await;
    pool.shutdown().await;
    assert!(
        second.html.contains("served-by-proxy-b"),
        "the second crawl must go through its own proxy, got {}",
        second.html
    );
    assert_eq!(
        target_requests(&seen_a).len(),
        1,
        "proxy A must see the first crawl only"
    );
}

#[tokio::test]
async fn a_render_and_an_interact_session_on_a_connected_chrome_go_through_the_proxy() {
    use futures::StreamExt as _;
    let name = "a_render_and_an_interact_session_on_a_connected_chrome_go_through_the_proxy";
    let profile = tempfile::tempdir().expect("a temp profile directory");
    let launched = chromiumoxide::BrowserConfig::builder()
        .no_sandbox()
        .new_headless_mode()
        .user_data_dir(profile.path())
        .build()
        .map_err(|e| e.to_string());
    let launched = match launched {
        Ok(config) => chromiumoxide::Browser::launch(config).await.map_err(|e| e.to_string()),
        Err(e) => Err(e),
    };
    let (mut chrome, mut handler) = match launched {
        Ok(launched) => launched,
        Err(reason) => {
            announce_chrome_skip(name, &reason);
            return;
        }
    };
    let handler = tokio::spawn(async move { while handler.next().await.is_some() {} });
    let (address, seen) = spawn_proxy().await;
    let mut config = render_config(proxy_at(address, None, None));
    config.browser.endpoint = Some(chrome.websocket_address().clone());
    let engine = create_engine(Some(config)).expect("engine must build");

    let rendered = scrape(&engine, TARGET).await;
    let interacted = interact(&engine, TARGET, vec![PageAction::Scrape]).await;
    let _ = chrome.close().await;
    handler.abort();

    let rendered = rendered.unwrap_or_else(|e| panic!("the render must go through the proxy: {e:?}"));
    assert!(rendered.html.contains(MARKER), "render: got {}", rendered.html);
    let interacted = interacted.unwrap_or_else(|e| panic!("the interact session must go through the proxy: {e:?}"));
    assert!(
        interacted.final_html.contains(MARKER),
        "interact: got {}",
        interacted.final_html
    );
    assert!(target_requests(&seen).len() >= 2, "the proxy must see both sessions");
}

/// A loopback address no server listens on: a direct request gets a refused connection, so the
/// page can only come from the proxy.
///
/// ~keep A port bound and released, not a low fixed one: Chrome refuses ports such as 1 as
/// ~keep unsafe before it consults any proxy.
async fn loopback_target() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind a loopback port");
    let port = listener.local_addr().expect("loopback address").port();
    drop(listener);
    format!("http://127.0.0.1:{port}/loopback")
}

/// Chrome sends loopback requests direct unless told not to, for a launch flag and for a
/// browser context alike.
#[tokio::test]
async fn a_loopback_page_goes_through_the_proxy_on_a_launch_and_in_a_pool() {
    let name = "a_loopback_page_goes_through_the_proxy_on_a_launch_and_in_a_pool";
    let (address, seen) = spawn_proxy().await;
    let target = loopback_target().await;
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    for (label, pooled) in [("one-shot launch", false), ("shared pool", true)] {
        let mut config = render_config(proxy_at(address.clone(), None, None));
        config
            .ssrf
            .allowlist
            .push(HostMatcher::cidr("127.0.0.1/32").expect("a valid CIDR"));
        config.browser.session_affinity = false;
        if pooled {
            config.browser_pool = Some(Arc::clone(&pool));
        }
        let before = requests_for(&seen, &target).len();
        let Some(result) = render_url(&format!("{name} ({label})"), config, &target, &seen).await else {
            pool.shutdown().await;
            return;
        };
        assert!(result.html.contains(MARKER), "{label}: got {}", result.html);
        assert!(
            requests_for(&seen, &target).len() > before,
            "{label}: the proxy must see the loopback request"
        );
    }
    pool.shutdown().await;
}

#[tokio::test]
async fn a_chrome_render_with_only_the_crawl_wide_proxy_goes_through_it() {
    let (address, seen) = spawn_proxy().await;
    let mut config = render_config(proxy_at(address, None, None));
    config.proxy = config.browser.proxy.take();
    config.browser.session_affinity = false;
    let Some(result) = render(
        "a_chrome_render_with_only_the_crawl_wide_proxy_goes_through_it",
        config,
        &seen,
    )
    .await
    else {
        return;
    };
    assert!(result.browser_used, "the page must come from a Chrome render");
    assert!(
        result.html.contains(MARKER),
        "the one-shot Chrome launch must go through the crawl-wide proxy, got {}",
        result.html
    );
    assert!(
        !target_requests(&seen).is_empty(),
        "the proxy must see the render's request, saw {:?}",
        seen.lock().expect("record")
    );
}
