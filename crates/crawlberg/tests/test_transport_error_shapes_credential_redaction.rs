//! Integration test pinning that DNS, timeout and TLS transport errors never leak URL
//! credentials, the same way a refused connection already does not (#444).
//!
//! ~keep `classify_reqwest_error` builds every `NetworkErrorKind`'s message through the
//! same `format!("[network:{tag}] {e}")` inside one macro-generated constructor set, so the
//! connection-refused coverage in `test_transport_error_credential_redaction.rs` already
//! implies these shapes are safe today. But nothing pinned that, so a change that gave one
//! kind its own message (as `network_error_kind`'s own doc comment warns can happen -- a
//! fetched page's own text can pick its error class) could reprint the raw URL in exactly
//! one arm and every existing test would still pass. Each case below drives a real
//! `CrawlEngine::scrape()` against credentials embedded in the address.

use crawlberg::{CrawlConfig, CrawlEngine, HostMatcher};
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio::net::TcpListener;

fn build_engine(config: CrawlConfig) -> CrawlEngine {
    CrawlEngine::builder().config(config).build().unwrap()
}

fn assert_credentials_redacted(err: &crawlberg::CrawlError, host_needle: &str) {
    let rendered = format!("{err}\n{err:?}");
    assert!(
        !rendered.contains("hunter2"),
        "the transport error must never carry the URL's password, got {rendered}"
    );
    assert!(
        !rendered.contains("alice"),
        "the transport error must never carry the URL's username, got {rendered}"
    );
    assert!(
        rendered.contains(host_needle),
        "the host must still be named so the error stays actionable, got {rendered}"
    );
}

/// An unresolvable hostname, allowlisted so crawlberg's own SSRF DNS check (which resolves
/// independently and would otherwise fail closed with a different error) steps aside and
/// the failure comes from `reqwest`'s own resolution instead -- the same `.invalid` host
/// `error.rs`'s pre-existing `dns_failure_produces_dns_tag` unit test uses.
#[tokio::test]
async fn should_drop_url_credentials_from_a_dns_failure() {
    let host = "this-hostname-does-not-exist-crawlberg-test.invalid";
    let mut config = CrawlConfig::default();
    config.ssrf.allowlist.push(HostMatcher::exact(host));
    let engine = build_engine(config);

    let err = engine
        .scrape(&format!("http://alice:hunter2@{host}/"))
        .await
        .expect_err("an unresolvable host must fail");

    assert_credentials_redacted(&err, host);
}

/// A listener that accepts the TCP connection but never writes a response, so the
/// request fails on `config.request_timeout`, not on connection refusal.
#[tokio::test]
async fn should_drop_url_credentials_from_a_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind failed");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        if let Ok((_socket, _)) = listener.accept().await {
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });

    let mut config = CrawlConfig::default();
    config.ssrf.deny_private = false;
    config.request_timeout = Duration::from_millis(200);
    let engine = build_engine(config);

    let err = engine
        .scrape(&format!("http://alice:hunter2@{addr}/"))
        .await
        .expect_err("a server that never answers must time out");

    assert_credentials_redacted(&err, &addr.ip().to_string());
}

/// Kills the child `openssl s_server` on drop, so a failed assertion still cleans it up.
struct OpensslServer(Child);

impl Drop for OpensslServer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A TLS server presenting a self-signed certificate for `127.0.0.1`, produced with the
/// box's own `openssl` binary (no new crate for one test's certificate). The client never
/// trusts it, so the handshake fails on certificate verification -- a real "bad
/// certificate" shape, not a bare connection abort.
fn spawn_self_signed_tls_server(port: u16, cert_dir: &std::path::Path) -> OpensslServer {
    let cert_path = cert_dir.join("cert.pem");
    let key_path = cert_dir.join("key.pem");

    let status = Command::new("openssl")
        .args([
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-keyout",
            key_path.to_str().expect("utf8 path"),
            "-out",
            cert_path.to_str().expect("utf8 path"),
            "-days",
            "2",
            "-nodes",
            "-subj",
            "/CN=127.0.0.1",
            "-addext",
            "subjectAltName=IP:127.0.0.1",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("openssl must be on PATH to generate a throwaway self-signed cert");
    assert!(status.success(), "openssl req must succeed, got {status:?}");

    let child = Command::new("openssl")
        .args([
            "s_server",
            "-accept",
            &port.to_string(),
            "-cert",
            cert_path.to_str().expect("utf8 path"),
            "-key",
            key_path.to_str().expect("utf8 path"),
            "-quiet",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("openssl s_server must start");

    OpensslServer(child)
}

async fn wait_until_accepting(addr: std::net::SocketAddr) {
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("openssl s_server never started accepting connections on {addr}");
}

#[tokio::test]
async fn should_drop_url_credentials_from_a_bad_certificate() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("must bind an ephemeral port");
    let addr = listener.local_addr().expect("must read local addr");
    drop(listener);

    let cert_dir = tempfile::tempdir().expect("must create a temp dir for the throwaway cert");
    let _server = spawn_self_signed_tls_server(addr.port(), cert_dir.path());
    wait_until_accepting(addr).await;

    let mut config = CrawlConfig::default();
    config.ssrf.deny_private = false;
    let engine = build_engine(config);

    let err = engine
        .scrape(&format!("https://alice:hunter2@{addr}/"))
        .await
        .expect_err("an untrusted self-signed certificate must fail verification");

    assert_credentials_redacted(&err, &addr.ip().to_string());
}
