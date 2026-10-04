//! Chrome's sockets under `ssrf.deny_private`: a WebSocket, a WebTransport session, a WebRTC
//! STUN probe and a host name Chrome would resolve again must not reach a denied address, on
//! the page paths that can originate each primitive. Each WebSocket row has an allowlisted twin,
//! and each page also fetches a denied address, which must be refused, so a row that sends nothing
//! because the page never ran cannot pass. WebRTC positive controls use a real STUN response,
//! and the default isolated one-shot path separately proves the launched scratch profile carries
//! Chrome's UDP-blocking policy.

#![cfg(feature = "browser")]

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use crawlberg::{
    BrowserBackend, BrowserConfig, BrowserMode, BrowserPool, BrowserPoolConfig, BrowserProfile, BrowserWait,
    CrawlConfig, CrawlError, HostMatcher, PageAction, SsrfPolicy, create_engine, interact, scrape, validate_url,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
#[cfg(unix)]
use common::is_snap_executable;
use common::{announce_chrome_skip, announce_skip, is_missing_chrome_message};

static PROFILE_COUNTER: AtomicU64 = AtomicU64::new(0);
const EGRESS_COMPLETION_SETUP: &str = r#"
    window.__egressCompleted = 0;
    window.__egressDone = () => {
        window.__egressCompleted += 1;
        document.documentElement.dataset.egressDone = String(window.__egressCompleted);
    };
"#;
const EGRESS_COMPLETION_SELECTOR: &str = "[data-egress-done='2']";
// ~keep WebTransport completes through a 1.5 s page timer. This wait covers that timer;
// ~keep `run_actions` then adds its 25 ms refusal grace before the final selector assertion.
const EGRESS_ACTION_SETTLE_MS: i64 = 2_000;

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
async fn host_ip() -> Option<IpAddr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    let ip = socket.local_addr().ok()?.ip();
    if ip.is_loopback() || ip.is_unspecified() {
        return None;
    }
    let url = format!("http://{ip}/").parse().ok()?;
    validate_url(&url, &SsrfPolicy::default()).await.is_err().then_some(ip)
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

/// ~keep Count every probe while answering valid STUN binding requests so ICE can complete.
async fn stun_server(ip: IpAddr) -> (u16, Arc<AtomicUsize>) {
    let socket = tokio::net::UdpSocket::bind(SocketAddr::new(ip, 0))
        .await
        .expect("the test socket must bind");
    let port = socket.local_addr().expect("a bound socket has an address").port();
    let count = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&count);
    tokio::spawn(async move {
        let mut buffer = [0u8; 4096];
        while let Ok((len, peer)) = socket.recv_from(&mut buffer).await {
            counted.fetch_add(1, Ordering::SeqCst);
            if let Some(response) = stun_binding_success(&buffer[..len], peer) {
                let _ = socket.send_to(&response, peer).await;
            }
        }
    });
    (port, count)
}

fn stun_binding_success(request: &[u8], peer: SocketAddr) -> Option<Vec<u8>> {
    const BINDING_REQUEST: [u8; 2] = [0, 1];
    const MAGIC_COOKIE: [u8; 4] = [0x21, 0x12, 0xa4, 0x42];
    if request.len() < 20 || request[..2] != BINDING_REQUEST || request[4..8] != MAGIC_COOKIE {
        return None;
    }
    let message_len = usize::from(u16::from_be_bytes([request[2], request[3]]));
    if !message_len.is_multiple_of(4) || request.len() < 20 + message_len {
        return None;
    }

    let (family, encoded_address) = match peer.ip() {
        IpAddr::V4(ip) => {
            let encoded = ip
                .octets()
                .into_iter()
                .zip(MAGIC_COOKIE)
                .map(|(address, mask)| address ^ mask)
                .collect::<Vec<_>>();
            (1, encoded)
        }
        IpAddr::V6(ip) => {
            let mut mask = [0u8; 16];
            mask[..4].copy_from_slice(&MAGIC_COOKIE);
            mask[4..].copy_from_slice(&request[8..20]);
            let encoded = ip
                .octets()
                .into_iter()
                .zip(mask)
                .map(|(address, mask)| address ^ mask)
                .collect::<Vec<_>>();
            (2, encoded)
        }
    };
    let attribute_len = encoded_address.len() + 4;
    let attribute_len = u16::try_from(attribute_len).expect("a STUN address attribute must fit in u16");
    let mut response = Vec::with_capacity(28 + encoded_address.len());
    response.extend_from_slice(&[1, 1]);
    response.extend_from_slice(&(attribute_len + 4).to_be_bytes());
    response.extend_from_slice(&MAGIC_COOKIE);
    response.extend_from_slice(&request[8..20]);
    response.extend_from_slice(&[0, 0x20]);
    response.extend_from_slice(&attribute_len.to_be_bytes());
    response.extend_from_slice(&[0, family]);
    response.extend_from_slice(&(peer.port() ^ 0x2112).to_be_bytes());
    response.extend_from_slice(&encoded_address);
    Some(response)
}

#[test]
fn a_stun_binding_request_receives_an_xor_mapped_success_response() {
    let request = [
        0x00, 0x01, 0x00, 0x00, 0x21, 0x12, 0xa4, 0x42, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a,
        0x0b, 0x0c,
    ];
    let peer = SocketAddr::from(([192, 0, 2, 1], 3478));
    let response = stun_binding_success(&request, peer).expect("a binding request must receive a response");

    assert_eq!(
        response,
        [
            0x01, 0x01, 0x00, 0x0c, 0x21, 0x12, 0xa4, 0x42, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a,
            0x0b, 0x0c, 0x00, 0x20, 0x00, 0x08, 0x00, 0x01, 0x2c, 0x84, 0xe1, 0x12, 0xa6, 0x43,
        ]
    );
}

#[test]
fn a_non_stun_datagram_receives_no_response() {
    assert_eq!(
        stun_binding_success(b"not a STUN request", SocketAddr::from(([127, 0, 0, 1], 3478))),
        None
    );
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
        // ~keep Interact executes after navigation. Match that point here: Chrome can reject a
        // ~keep WebSocket or ICE setup during initial parsing before an isolated context's
        // ~keep network stack is ready, which exercises no egress policy and proves nothing.
        _ => format!("<script>window.addEventListener('load', () => {{ {script} }});</script>"),
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
    if !matches!(via, Via::Interact) {
        config.browser.wait = BrowserWait::Selector;
        config.browser.wait_selector = Some(EGRESS_COMPLETION_SELECTOR.to_owned());
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
    let policy_must_refuse = config.ssrf.deny_private;
    let engine = create_engine(Some(config)).expect("the engine must build");
    let outcome = match via {
        Via::Interact => {
            let actions = vec![
                PageAction::ExecuteJs {
                    script: format!("{script} return 1;"),
                },
                PageAction::Wait {
                    milliseconds: Some(EGRESS_ACTION_SETTLE_MS),
                    selector: None,
                },
                PageAction::Wait {
                    milliseconds: None,
                    selector: Some(EGRESS_COMPLETION_SELECTOR.to_owned()),
                },
            ];
            tokio::time::timeout(Duration::from_secs(60), interact(&engine, &seed, actions))
                .await
                .map(|result| {
                    result.map(|result| {
                        assert_eq!(
                            result.action_results.len(),
                            3,
                            "{test_name}: the completion sequence must return exactly three action results: {:?}",
                            result.action_results
                        );
                        let pre_completion = &result.action_results[..2];
                        let is_policy_failure = |action: &crawlberg::ActionResult| {
                            !action.success
                                && action
                                    .error
                                    .as_deref()
                                    .is_some_and(|error| error.starts_with("ssrf_policy_violation:"))
                        };
                        if policy_must_refuse {
                            assert!(
                                pre_completion.iter().any(&is_policy_failure),
                                "{test_name}: at least one pre-completion action must own a policy refusal: {pre_completion:?}"
                            );
                            assert!(
                                pre_completion
                                    .iter()
                                    .all(|action| action.success || is_policy_failure(action)),
                                "{test_name}: no pre-completion action may fail for another reason: {pre_completion:?}"
                            );
                        } else {
                            assert!(
                                pre_completion
                                    .iter()
                                    .all(|action| action.success && action.error.is_none()),
                                "{test_name}: both pre-completion actions must succeed with private access allowed: {pre_completion:?}"
                            );
                        }
                        let completion = &result.action_results[2];
                        assert!(
                            completion.success && completion.error.is_none(),
                            "{test_name}: the final completion selector wait must succeed: {completion:?}"
                        );
                        result.ssrf_refused_urls
                    })
                })
        }
        _ => tokio::time::timeout(Duration::from_secs(60), scrape(&engine, &seed))
            .await
            .map(|result| result.map(|result| result.ssrf_refused_urls)),
    };
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
    let Some(ip) = host_ip().await else {
        announce_skip(
            test_name,
            "this machine has no non-loopback address denied by the default SSRF policy",
        );
        return;
    };
    let allowlist = if allowed {
        vec![HostMatcher::cidr(format!("{ip}/32")).expect("a valid CIDR")]
    } else {
        Vec::new()
    };
    let (target, reached) = counting_tcp(SocketAddr::new(ip, 0)).await;
    let (refused, fetch) = control().await;
    let open = format!(
        r#"
        try {{
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
        }} catch (error) {{
            window.__egressDone();
        }}
        "#
    );
    let script = if worker {
        let worker_script = format!(
            r#"
            try {{
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
            }} catch (error) {{
                postMessage('done');
            }}
            "#
        );
        format!(
            r#"
            {EGRESS_COMPLETION_SETUP}
            try {{
                const worker = new Worker(URL.createObjectURL(
                    new Blob([{worker_script:?}], {{ type: 'text/javascript' }})
                ));
                let workerFinished = false;
                const workerDone = () => {{
                    if (!workerFinished) {{
                        workerFinished = true;
                        window.__egressDone();
                    }}
                }};
                worker.addEventListener('message', workerDone);
                worker.addEventListener('error', workerDone);
            }} catch (error) {{
                window.__egressDone();
            }}
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
        // ~keep An isolated browser context can reject a WebSocket before Chrome sends a
        // ~keep proxy-visible handshake. The denied listener plus the allowlisted twin prove
        // ~keep the policy outcome; only shared/action paths can also promise a listed socket.
        if matches!(via, Via::Profile | Via::Interact) {
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
    let (port, datagrams) = stun_server(IpAddr::from([127, 0, 0, 1])).await;
    let (refused, fetch) = control().await;
    let script = format!(
        r#"
        {EGRESS_COMPLETION_SETUP}
        try {{
            const transport = new WebTransport('https://127.0.0.1:{port}/wt');
            transport.ready.catch(() => {{}});
            transport.closed.catch(() => {{}});
            setTimeout(window.__egressDone, 1500);
        }} catch (error) {{
            window.__egressDone();
        }}
        {fetch}
        "#
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
    let Some(ip) = host_ip().await else {
        announce_skip(
            test_name,
            "this machine has no non-loopback address denied by the default SSRF policy",
        );
        return;
    };
    let (port, datagrams) = stun_server(ip).await;
    let (refused, fetch) = control().await;
    // ~keep ICE gathering completion follows the STUN attempts instead of racing them against a
    // ~keep fixed timer; loopback STUN destinations can be rejected inside Chrome without a probe.
    let script = format!(
        r#"
        {EGRESS_COMPLETION_SETUP}
        try {{
            const pc = new RTCPeerConnection({{
                iceServers: [{{ urls: 'stun:{ip}:{port}' }}]
            }});
            let finished = false;
            const done = () => {{
                if (!finished) {{
                    finished = true;
                    window.__egressDone();
                }}
            }};
            pc.addEventListener('icegatheringstatechange', () => {{
                if (pc.iceGatheringState === 'complete') done();
            }});
            pc.createDataChannel('x');
            pc.createOffer()
                .then(o => pc.setLocalDescription(o))
                .then(() => {{
                    if (pc.iceGatheringState === 'complete') done();
                }})
                .catch(done);
        }} catch (error) {{
            window.__egressDone();
        }}
        {fetch}
        "#
    );
    let mut config = config(Vec::new());
    config.browser.timeout = Duration::from_secs(45);
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

#[cfg(unix)]
fn shell_quote(path: &std::path::Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\"'\"'"))
}

/// ~keep Prove the ordinary one-shot branch writes WebRTC policy into its launched scratch
/// ~keep profile even though no caller supplied a `browser_profile`.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn default_one_shot_disables_non_proxied_udp_in_its_launched_profile() {
    use std::os::unix::fs::PermissionsExt;

    let test_name = "default_one_shot_disables_non_proxied_udp_in_its_launched_profile";
    let real_chrome = match chromiumoxide::detection::default_executable(Default::default()) {
        Ok(path) => path,
        Err(message) => {
            announce_chrome_skip(test_name, &message);
            return;
        }
    };
    let real_chrome_is_snap = is_snap_executable(&real_chrome);
    let dir = tempfile::tempdir().expect("a temp directory");
    let snapshot = dir.path().join("Preferences.snapshot");
    let wrapper = dir.path().join("chrome-wrapper.sh");
    let script = format!(
        "#!/bin/sh\nprofile=''\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    --user-data-dir=*) profile=${{arg#*=}} ;;\n  esac\ndone\ncp \"$profile/Default/Preferences\" {}\nexec {} \"$@\"\n",
        shell_quote(&snapshot),
        shell_quote(&real_chrome)
    );
    std::fs::write(&wrapper, script).expect("the wrapper must be writable");
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).expect("the wrapper must be executable");

    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&site)
        .await;
    let mut config = config(Vec::new());
    assert_eq!(
        config.browser_profile, None,
        "the default one-shot path must be under test"
    );
    config.browser.chrome_path = Some(wrapper);
    let engine = create_engine(Some(config)).expect("the engine must build");
    let result = scrape(&engine, &format!("http://localhost:{}/", site.address().port())).await;

    let preferences: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&snapshot).expect("the launched wrapper must snapshot its scratch profile preferences"),
    )
    .expect("the profile preferences must be JSON");
    assert_eq!(
        preferences.pointer("/webrtc/ip_handling_policy"),
        Some(&serde_json::Value::String("disable_non_proxied_udp".to_owned()))
    );
    match result {
        Ok(_) => {}
        Err(CrawlError::BrowserError { message, .. })
            if real_chrome_is_snap && message.contains("the browser did not use crawlberg's profile directory") =>
        {
            announce_chrome_skip(test_name, &message);
        }
        Err(error) => panic!("the default one-shot scrape must succeed after the policy snapshot: {error:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn webrtc_sends_nothing_to_a_denied_address_with_a_browser_profile() {
    webrtc_row("webrtc_profile", Via::Profile, true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn webrtc_sends_with_deny_private_off_and_a_browser_profile() {
    webrtc_row("webrtc_profile_off", Via::Profile, false).await;
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
    let script = format!(
        r#"
        {EGRESS_COMPLETION_SETUP}
        fetch('http://{name}:{port}/rebind', {{ mode: 'no-cors' }})
            .then(window.__egressDone, window.__egressDone);
        {fetch}
        "#
    );
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
