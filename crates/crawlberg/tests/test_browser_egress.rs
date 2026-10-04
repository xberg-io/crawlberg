//! Chrome's sockets under `ssrf.deny_private`: a WebSocket, a WebTransport session, a WebRTC
//! STUN probe and a host name Chrome would resolve again must not reach a denied address, on
//! every path that opens a page. Each row has a twin that reaches an allowed address, and each
//! page also fetches a denied address, which must be refused, so a row that sends nothing
//! because the page never ran cannot pass.

#![cfg(feature = "browser")]

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use crawlberg::{
    BrowserBackend, BrowserConfig, BrowserMode, BrowserPool, BrowserPoolConfig, BrowserProfile, BrowserWait,
    CrawlConfig, CrawlError, HostMatcher, PageAction, create_engine, interact, scrape,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::{announce_chrome_skip, announce_skip, is_missing_chrome_message};

static PROFILE_COUNTER: AtomicU64 = AtomicU64::new(0);
const EGRESS_COMPLETION_SETUP: &str = r#"
    window.__egressCompleted = 0;
    window.__egressDone = () => {
        window.__egressCompleted += 1;
        document.documentElement.dataset.egressDone = String(window.__egressCompleted);
    };
"#;

/// How the page is opened.
#[derive(Clone, Copy, Debug)]
enum Via {
    /// A one-shot scrape in a launched Chrome.
    OneShot,
    /// A one-shot scrape with `browser_profile`: the page is in the browser's own context.
    Profile,
    /// A scrape through a `BrowserPool`.
    Pooled,
    /// An `interact` session whose script runs as an action.
    Interact,
    /// A one-shot scrape on a Chrome on this machine reached through `browser.endpoint`.
    Endpoint,
}

fn config(allowlist: Vec<HostMatcher>) -> CrawlConfig {
    let mut builder = CrawlConfig::builder().ssrf_allowlist_host(HostMatcher::exact("localhost"));
    for matcher in allowlist {
        builder = builder.ssrf_allowlist_host(matcher);
    }
    let mut config = CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            mode: BrowserMode::Always,
            timeout: Duration::from_secs(20),
            extra_wait: Some(Duration::from_millis(2500)),
            ..BrowserConfig::default()
        },
        respect_robots_txt: false,
        ..builder.build()
    };
    config.browser.session_affinity = false;
    config
}

/// This machine's address on its default route, which the policy denies unless allowlisted.
fn host_ip() -> Option<IpAddr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then_some(ip)
}

/// A TCP listener that counts its connections and answers each with a small page.
async fn counting_tcp(address: SocketAddr) -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .expect("the test listener must bind");
    let bound = listener.local_addr().expect("a bound listener has an address");
    let count = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&count);
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            counted.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut buffer = [0u8; 2048];
                let _ = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buffer)).await;
                let _ = stream
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\ncontent-type: text/plain\r\nconnection: close\r\n\r\nok")
                    .await;
            });
        }
    });
    (bound, count)
}

/// A UDP socket on loopback that counts datagrams.
async fn counting_udp() -> (u16, Arc<AtomicUsize>) {
    let socket = tokio::net::UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("the test socket must bind");
    let port = socket.local_addr().expect("a bound socket has an address").port();
    let count = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&count);
    tokio::spawn(async move {
        let mut buffer = [0u8; 4096];
        while socket.recv_from(&mut buffer).await.is_ok() {
            counted.fetch_add(1, Ordering::SeqCst);
        }
    });
    (port, count)
}

/// A denied listener every page fetches, and the script that fetches it.
async fn control() -> (Arc<AtomicUsize>, String) {
    let (address, count) = counting_tcp(SocketAddr::from(([127, 0, 0, 1], 0))).await;
    let script = format!(
        "fetch('http://{address}/control', {{ mode: 'no-cors' }}).then(window.__egressDone, window.__egressDone);"
    );
    (count, script)
}

/// Run `script` in a page opened `via`, with `config`, and return the result's refused URLs;
/// `None` when Chrome is missing.
async fn run(test_name: &str, via: Via, script: &str, mut config: CrawlConfig) -> Option<Vec<String>> {
    let site = MockServer::start().await;
    let body = match via {
        Via::Interact => String::new(),
        _ => format!("<script>{script}</script>"),
    };
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(format!("<html><body><p>start</p>{body}</body></html>"), "text/html"),
        )
        .mount(&site)
        .await;
    let seed = format!("http://localhost:{}/", site.address().port());
    let waits_for_completion = script.contains("__egressDone");
    if waits_for_completion {
        config.browser.wait = BrowserWait::Selector;
        config.browser.wait_selector = Some("[data-egress-done='2']".to_owned());
    }
    let pool = matches!(via, Via::Pooled).then(|| {
        BrowserPool::new(BrowserPoolConfig {
            chrome_args: config.browser.chrome_args.clone(),
            ..BrowserPoolConfig::default()
        })
    });
    if let Some(pool) = &pool {
        config.browser_pool = Some(Arc::clone(pool));
    }
    let profile = matches!(via, Via::Profile).then(|| {
        let name = format!(
            "crawlberg-egress-{}-{}",
            std::process::id(),
            PROFILE_COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        config.browser_profile = Some(name.clone());
        BrowserProfile::new(&name).expect("the profile name must be valid")
    });
    let mut endpoint = None;
    if matches!(via, Via::Endpoint) {
        let dir = tempfile::tempdir().expect("a temp profile directory");
        let launched = match chromiumoxide::BrowserConfig::builder()
            .no_sandbox()
            .new_headless_mode()
            .user_data_dir(dir.path())
            .build()
        {
            Ok(launch) => chromiumoxide::Browser::launch(launch)
                .await
                .map_err(|error| error.to_string()),
            Err(error) => Err(error),
        };
        let (chrome, handler) = common::expect_chrome_or_skip(test_name, launched)?;
        let handler = common::spawn_handler(handler);
        config.browser.endpoint = Some(chrome.websocket_address().clone());
        endpoint = Some((chrome, handler, dir));
    }
    let engine = create_engine(Some(config)).expect("the engine must build");
    let outcome = match via {
        Via::Interact => {
            let wait = if waits_for_completion {
                PageAction::Wait {
                    milliseconds: None,
                    selector: Some("[data-egress-done='2']".to_owned()),
                }
            } else {
                PageAction::Wait {
                    milliseconds: Some(1500),
                    selector: None,
                }
            };
            let actions = vec![
                PageAction::ExecuteJs {
                    script: format!("{script} return 1;"),
                },
                wait,
            ];
            tokio::time::timeout(Duration::from_secs(60), interact(&engine, &seed, actions))
                .await
                .map(|result| result.map(|result| result.ssrf_refused_urls))
        }
        _ => tokio::time::timeout(Duration::from_secs(60), scrape(&engine, &seed))
            .await
            .map(|result| result.map(|result| result.ssrf_refused_urls)),
    };
    if !waits_for_completion {
        tokio::time::sleep(Duration::from_millis(1500)).await;
    }
    if let Some(pool) = pool {
        pool.shutdown().await;
    }
    if let Some((mut chrome, handler, _dir)) = endpoint {
        let _ = chrome.close().await;
        handler.abort();
    }
    if let Some(profile) = profile {
        let _ = profile.delete();
    }
    match outcome {
        Err(_) => panic!("{test_name}: the page timed out"),
        Ok(Ok(refused)) => Some(refused),
        Ok(Err(CrawlError::BrowserError { message, .. })) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
            None
        }
        Ok(Err(error)) => panic!("{test_name}: the page must load: {error:?}"),
    }
}

/// A WebSocket from the page, or from a worker it starts, to a denied address or, as the
/// twin, to this machine's allowlisted address.
async fn websocket_row(test_name: &str, via: Via, worker: bool, allowed: bool) {
    let (ip, allowlist) = if allowed {
        let Some(ip) = host_ip() else {
            announce_skip(test_name, "this machine has no address off loopback");
            return;
        };
        (ip, vec![HostMatcher::cidr(format!("{ip}/32")).expect("a valid CIDR")])
    } else {
        (IpAddr::from([127, 0, 0, 1]), Vec::new())
    };
    let (target, reached) = counting_tcp(SocketAddr::new(ip, 0)).await;
    let (refused, fetch) = control().await;
    let open = format!(
        r#"
        {{
            const socket = new WebSocket('ws://{target}/ws');
            let finished = false;
            const done = () => {{
                if (!finished) {{
                    finished = true;
                    window.__egressDone();
                }}
            }};
            socket.addEventListener('open', done);
            socket.addEventListener('error', done);
        }}
        "#
    );
    let script = if worker {
        let worker_script = format!(
            r#"
            const socket = new WebSocket('ws://{target}/ws');
            let finished = false;
            const done = () => {{
                if (!finished) {{
                    finished = true;
                    postMessage('done');
                }}
            }};
            socket.addEventListener('open', done);
            socket.addEventListener('error', done);
            "#
        );
        format!(
            r#"
            {EGRESS_COMPLETION_SETUP}
            const worker = new Worker(URL.createObjectURL(
                new Blob([{worker_script:?}], {{ type: 'text/javascript' }})
            ));
            worker.addEventListener('message', window.__egressDone);
            {fetch}
            "#
        )
    } else {
        format!("{EGRESS_COMPLETION_SETUP}{open}{fetch}")
    };
    let Some(listed) = run(test_name, via, &script, config(allowlist)).await else {
        return;
    };
    let (reached, refused) = (reached.load(Ordering::SeqCst), refused.load(Ordering::SeqCst));
    assert_eq!(
        refused, 0,
        "{test_name}: the page's fetch to a denied address must be refused"
    );
    if allowed {
        assert!(
            reached >= 1,
            "{test_name}: a WebSocket to an allowlisted address must connect"
        );
    } else {
        assert_eq!(
            reached, 0,
            "{test_name}: a WebSocket to a denied address must not connect, got {reached}"
        );
        // ~keep A pool logs its refusals: one pool serves many crawls, so no result owns them.
        if !matches!(via, Via::Pooled) {
            assert!(
                listed.contains(&target.to_string()),
                "{test_name}: the refused WebSocket must be listed as {target}, got {listed:?}"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_websocket_to_a_denied_address_is_refused_in_a_one_shot_scrape() {
    websocket_row("ws_one_shot", Via::OneShot, false, false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_websocket_to_an_allowed_address_connects_in_a_one_shot_scrape() {
    websocket_row("ws_one_shot_allowed", Via::OneShot, false, true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_websocket_to_a_denied_address_is_refused_in_a_pooled_scrape() {
    websocket_row("ws_pooled", Via::Pooled, false, false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_websocket_to_an_allowed_address_connects_in_a_pooled_scrape() {
    websocket_row("ws_pooled_allowed", Via::Pooled, false, true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_worker_websocket_to_a_denied_address_is_refused() {
    websocket_row("ws_worker", Via::OneShot, true, false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_worker_websocket_to_an_allowed_address_connects() {
    websocket_row("ws_worker_allowed", Via::OneShot, true, true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_websocket_to_a_denied_address_is_refused_in_interact() {
    websocket_row("ws_interact", Via::Interact, false, false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_websocket_to_an_allowed_address_connects_in_interact() {
    websocket_row("ws_interact_allowed", Via::Interact, false, true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_websocket_to_a_denied_address_is_refused_on_a_local_endpoint() {
    websocket_row("ws_endpoint", Via::Endpoint, false, false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_websocket_to_an_allowed_address_connects_on_a_local_endpoint() {
    websocket_row("ws_endpoint_allowed", Via::Endpoint, false, true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_websocket_to_a_denied_address_is_refused_with_a_browser_profile() {
    websocket_row("ws_profile", Via::Profile, false, false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_websocket_to_an_allowed_address_connects_with_a_browser_profile() {
    websocket_row("ws_profile_allowed", Via::Profile, false, true).await;
}

/// A WebTransport session to a denied UDP port. The twin turns `deny_private` off, so the same
/// page does send, which shows the count can see a datagram.
async fn webtransport_row(test_name: &str, deny_private: bool) {
    let (port, datagrams) = counting_udp().await;
    let (refused, fetch) = control().await;
    let script = format!(
        "try {{ const t = new WebTransport('https://127.0.0.1:{port}/wt'); t.ready.catch(() => {{}}); t.closed.catch(() => {{}}); }} catch (e) {{}}{fetch}"
    );
    let mut config = config(Vec::new());
    config.ssrf.deny_private = deny_private;
    if run(test_name, Via::OneShot, &script, config).await.is_none() {
        return;
    }
    let (datagrams, refused) = (datagrams.load(Ordering::SeqCst), refused.load(Ordering::SeqCst));
    if deny_private {
        assert_eq!(
            refused, 0,
            "{test_name}: the page's fetch to a denied address must be refused"
        );
        assert_eq!(
            datagrams, 0,
            "{test_name}: WebTransport must send nothing to a denied address, got {datagrams}"
        );
    } else {
        assert!(
            datagrams >= 1,
            "{test_name}: with deny_private off the same page must send"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn webtransport_sends_nothing_to_a_denied_address() {
    webtransport_row("webtransport", true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn webtransport_sends_with_deny_private_off() {
    webtransport_row("webtransport_off", false).await;
}

/// A WebRTC STUN probe to a denied UDP port. The twin turns `deny_private` off, which leaves
/// Chrome's WebRTC policy at its default, so the same page does send, which shows the count can
/// see a datagram. A pooled Chrome has no twin: the pool writes the policy whatever the crawl's
/// `deny_private`.
async fn webrtc_row(test_name: &str, via: Via, deny_private: bool) {
    let (port, datagrams) = counting_udp().await;
    let (refused, fetch) = control().await;
    let script = format!(
        r#"
        {EGRESS_COMPLETION_SETUP}
        const pc = new RTCPeerConnection({{
            iceServers: [{{ urls: 'stun:127.0.0.1:{port}' }}]
        }});
        let finished = false;
        const done = () => {{
            if (!finished) {{
                finished = true;
                window.__egressDone();
            }}
        }};
        let candidateSeen = false;
        pc.addEventListener('icecandidate', event => {{
            if (event.candidate && !candidateSeen) {{
                candidateSeen = true;
                setTimeout(done, 500);
            }}
        }});
        pc.addEventListener('icegatheringstatechange', () => {{
            if (pc.iceGatheringState === 'complete' && !candidateSeen) done();
        }});
        pc.createDataChannel('x');
        pc.createOffer().then(o => pc.setLocalDescription(o)).catch(done);
        {fetch}
        "#
    );
    let mut config = config(Vec::new());
    config.ssrf.deny_private = deny_private;
    if run(test_name, via, &script, config).await.is_none() {
        return;
    }
    let (datagrams, refused) = (datagrams.load(Ordering::SeqCst), refused.load(Ordering::SeqCst));
    if deny_private {
        assert_eq!(
            refused, 0,
            "{test_name}: the page's fetch to a denied address must be refused"
        );
        assert_eq!(
            datagrams, 0,
            "{test_name}: WebRTC must send nothing to a denied address, got {datagrams}"
        );
    } else {
        assert!(
            datagrams >= 1,
            "{test_name}: with deny_private off the same page must send"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn webrtc_sends_nothing_to_a_denied_address() {
    webrtc_row("webrtc", Via::OneShot, true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn webrtc_sends_with_deny_private_off() {
    webrtc_row("webrtc_off", Via::OneShot, false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn webrtc_sends_nothing_to_a_denied_address_in_a_pooled_scrape() {
    webrtc_row("webrtc_pooled", Via::Pooled, true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn webrtc_sends_nothing_to_a_denied_address_in_interact() {
    webrtc_row("webrtc_interact", Via::Interact, true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn webrtc_sends_in_interact_with_deny_private_off() {
    webrtc_row("webrtc_interact_off", Via::Interact, false).await;
}

/// This machine's name, the IPv4 address it resolves to, and every address it resolves to.
/// `None` when any answer is loopback: the proxy refuses a name when any answer fails the
/// policy, and loopback stays denied here, so such a name never reaches its address. A macOS
/// name answers with its interfaces' other addresses, such as `::1` and IPv6 link-local.
async fn host_name_and_ip() -> Option<(String, IpAddr, Vec<IpAddr>)> {
    let name = match std::env::var("CRAWLBERG_TEST_HOST_NAME") {
        Ok(name) => name,
        Err(_) => String::from_utf8(std::process::Command::new("hostname").output().ok()?.stdout).ok()?,
    };
    let name = name.trim().to_owned();
    let answers: Vec<IpAddr> = tokio::net::lookup_host((name.as_str(), 80))
        .await
        .ok()?
        .map(|address| address.ip())
        .collect();
    if answers.iter().any(IpAddr::is_loopback) {
        return None;
    }
    let ip = answers.iter().copied().find(IpAddr::is_ipv4)?;
    Some((name, ip, answers))
}

/// A fetch by host name in a pooled page. The check resolves the name to this machine's
/// allowlisted address; `remap` makes Chrome's own lookup answer the denied loopback address,
/// as a second DNS answer would. Without `remap` it is the twin.
async fn rebinding_row(test_name: &str, remap: bool) {
    let Some((name, ip, answers)) = host_name_and_ip().await else {
        announce_skip(test_name, "this machine's name does not resolve only off loopback");
        return;
    };
    let (allowed, reached_allowed) = counting_tcp(SocketAddr::new(ip, 0)).await;
    let port = allowed.port();
    let (_denied, reached_denied) = counting_tcp(SocketAddr::from(([127, 0, 0, 1], port))).await;
    let (refused, fetch) = control().await;
    let script = format!("fetch('http://{name}:{port}/rebind', {{ mode: 'no-cors' }}).catch(() => {{}});{fetch}");
    let allowlist = answers
        .iter()
        .map(|answer| {
            let prefix = if answer.is_ipv4() { 32 } else { 128 };
            HostMatcher::cidr(format!("{answer}/{prefix}")).expect("a valid CIDR")
        })
        .collect();
    let mut config = config(allowlist);
    if remap {
        config.browser.chrome_args = vec![format!("--host-resolver-rules=MAP {name} 127.0.0.1")];
    }
    let Some(refused_urls) = run(test_name, Via::Pooled, &script, config).await else {
        return;
    };
    let (denied, allowed, refused) = (
        reached_denied.load(Ordering::SeqCst),
        reached_allowed.load(Ordering::SeqCst),
        refused.load(Ordering::SeqCst),
    );
    assert_eq!(
        refused, 0,
        "{test_name}: the page's fetch to a denied address must be refused"
    );
    assert_eq!(
        denied, 0,
        "{test_name}: the denied address must receive no connection, got {denied}"
    );
    assert!(
        allowed >= 1,
        "{test_name}: the address the check passed must be reached; {name} resolves to {answers:?}, refused {refused_urls:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_name_chrome_would_resolve_again_reaches_only_the_checked_address() {
    rebinding_row("rebinding", true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_name_reaches_its_address_without_a_second_answer() {
    rebinding_row("rebinding_twin", false).await;
}
