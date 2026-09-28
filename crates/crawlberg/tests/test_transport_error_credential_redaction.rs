//! Integration test pinning that a transport-level error never leaks URL credentials (#373).
//!
//! ~keep `reqwest` strips `user:password@` from the URL it attaches to a transport error
//! before rendering it, so this never needed redaction logic of its own here — but nothing
//! pinned that fact, so a `reqwest` upgrade that changed it would go unnoticed. This drives
//! a real connection failure against a URL carrying credentials and inspects the resulting
//! `CrawlError`'s rendered text.

use crawlberg::{CrawlConfig, CrawlEngine};

fn build_engine(config: CrawlConfig) -> CrawlEngine {
    CrawlEngine::builder().config(config).build().unwrap()
}

/// Bind an ephemeral port and immediately drop the listener, guaranteeing the port is
/// closed (connection refused) rather than merely unassigned.
fn closed_local_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("must bind an ephemeral port");
    listener.local_addr().expect("must read local addr").port()
}

#[tokio::test]
async fn should_drop_url_credentials_from_a_transport_error_while_keeping_the_host() {
    let port = closed_local_port();
    let url = format!("http://alice:hunter2@127.0.0.1:{port}/");

    let mut config = CrawlConfig::default();
    config.ssrf.deny_private = false;

    let engine = build_engine(config);
    let err = engine
        .scrape(&url)
        .await
        .expect_err("a connection to a closed port must fail");

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
        rendered.contains("127.0.0.1"),
        "the host must still be named so the error stays actionable, got {rendered}"
    );
}
