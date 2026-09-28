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
use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::net::SocketAddr;
use std::sync::{Arc, Once};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

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

/// A throwaway self-signed certificate for `127.0.0.1`, checked in as static DER fixtures
/// rather than generated at test time: no external process, no cross-OpenSSL-version
/// drift between CI's Linux and macOS legs (#463's own review flagged the old
/// `openssl req`/`s_server` version as untested on macOS). `rustls` and `tokio-rustls`
/// are already resolved in `Cargo.lock` via other dependents, so this adds no new
/// supply-chain surface.
static CERT_DER: &[u8] = include_bytes!("fixtures/self_signed/cert.der");
static KEY_DER: &[u8] = include_bytes!("fixtures/self_signed/key.der");

fn install_crypto_provider() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Spawns an in-process TLS server presenting the self-signed certificate above and
/// returns the address it is listening on. The client never trusts the certificate, so
/// the handshake fails on verification -- a real "bad certificate" shape, not a bare
/// connection abort. Each accepted connection is handled on its own task, and a failed
/// handshake is expected and ignored: the test only cares what the *client* sees.
async fn spawn_self_signed_tls_server() -> SocketAddr {
    install_crypto_provider();

    let cert = CertificateDer::from(CERT_DER.to_vec());
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(KEY_DER.to_vec()));
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .expect("the self-signed server config must build");
    let acceptor = TlsAcceptor::from(Arc::new(config));

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("must bind an ephemeral port");
    let addr = listener.local_addr().expect("must read local addr");

    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let _ = acceptor.accept(stream).await;
            });
        }
    });

    addr
}

#[tokio::test]
async fn should_drop_url_credentials_from_a_bad_certificate() {
    let addr = spawn_self_signed_tls_server().await;

    let mut config = CrawlConfig::default();
    config.ssrf.deny_private = false;
    let engine = build_engine(config);

    let err = engine
        .scrape(&format!("https://alice:hunter2@{addr}/"))
        .await
        .expect_err("an untrusted self-signed certificate must fail verification");

    assert_credentials_redacted(&err, &addr.ip().to_string());
}
