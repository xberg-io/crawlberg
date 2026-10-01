//! A failed send is logged with the endpoint's origin and the cause, never with the API key.
//!
//! This test owns its binary: it installs the process-wide subscriber, so no other test's
//! events or interest caching can interfere with what it captures.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use crawlberg::BypassProvider;
use crawlberg_bypass::SimpleHttpProvider;
use crawlberg_bypass::config::{
    AuthScheme, CostExtraction, HttpMethod, ProviderConfig, RequestShape, ResponseKind, ResponseShape, UrlParamLocation,
};

const API_KEY: &str = "sk-live-9f8e7d6c5b4a";
const TARGET: &str = "https://example.com/page";

fn query_key_config(endpoint: &str) -> ProviderConfig {
    ProviderConfig {
        vendor_name: "querykey".into(),
        endpoint: endpoint.into(),
        method: HttpMethod::Get,
        auth: AuthScheme::QueryParam {
            name: "api_key".into(),
            value: API_KEY.into(),
        },
        request: RequestShape {
            body: None,
            query: vec![],
            url_param: UrlParamLocation::QueryParam { name: "url".into() },
        },
        response: ResponseShape {
            kind: ResponseKind::RawBody,
            cost_extraction: CostExtraction::None,
            fallback_cost_usd: None,
        },
        status_mapping: vec![],
    }
}

/// A server that reads one request and closes the connection without a response. The listener
/// stays bound until then, so no other process can take the port and answer instead.
fn closing_server() -> (u16, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
        }
    });
    (port, server)
}

/// A log writer that keeps every line in memory.
#[derive(Clone, Default)]
struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

impl Write for CapturedLogs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLogs {
    type Writer = CapturedLogs;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn transport_failures_log_the_endpoint_origin_and_the_cause() {
    let (port, server) = closing_server();
    let logs = CapturedLogs::default();
    tracing_subscriber::fmt()
        .with_writer(logs.clone())
        .with_ansi(false)
        .init();

    let provider = SimpleHttpProvider::new(query_key_config(&format!("http://127.0.0.1:{port}/v1/"))).unwrap();
    provider.fetch(TARGET).await.unwrap_err();
    let _ = server.join();

    let logged = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(
        logged.contains("bypass provider send failed"),
        "the send failure was not logged: {logged}"
    );
    assert!(
        logged.contains(&format!("endpoint=http://127.0.0.1:{port} ")),
        "the log must name the endpoint's origin: {logged}"
    );
    assert!(
        !logged.contains("/v1/"),
        "the log must not name the endpoint's path: {logged}"
    );
    assert!(!logged.contains(API_KEY), "API key leaked into the log: {logged}");
    let cause = logged.split("cause=").nth(1).expect("the log must carry a cause field");
    assert!(!cause.starts_with([' ', '\n']), "the cause must not be empty: {logged}");

    // A server that promises a longer body than it sends fails the body read instead.
    logs.0.lock().unwrap().clear();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nshort");
        }
    });
    let provider = SimpleHttpProvider::new(query_key_config(&format!("http://127.0.0.1:{port}/v1/"))).unwrap();
    provider.fetch(TARGET).await.unwrap_err();
    let _ = server.join();

    let logged = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(
        logged.contains("bypass provider body read failed"),
        "the body read failure was not logged: {logged}"
    );
    assert!(
        logged.contains(&format!("endpoint=http://127.0.0.1:{port} ")),
        "the log must name the endpoint's origin: {logged}"
    );
    assert!(!logged.contains(API_KEY), "API key leaked into the log: {logged}");

    // A per-account endpoint host carries the key in a label; the `.invalid` name never resolves.
    logs.0.lock().unwrap().clear();
    let endpoint = format!("http://{API_KEY}.vendor.invalid/v1/");
    let provider = SimpleHttpProvider::new(query_key_config(&endpoint)).unwrap();
    provider.fetch(TARGET).await.unwrap_err();

    let logged = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(
        logged.contains("endpoint=http://***.vendor.invalid "),
        "the log must name the endpoint's origin with the key label hidden: {logged}"
    );
    assert!(!logged.contains(API_KEY), "API key leaked into the log: {logged}");
}
