//! A vendor that takes its API key as a query parameter must not see that key
//! surface in any field of the response or in any error string.

use std::io::{Read, Write};
use std::net::TcpListener;

use crawlberg::BypassProvider;
use crawlberg_bypass::SimpleHttpProvider;
use crawlberg_bypass::config::{
    AuthScheme, CostExtraction, HttpMethod, ProviderConfig, RequestShape, ResponseKind, ResponseShape, UrlParamLocation,
};
use wiremock::matchers::{method, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

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

#[tokio::test]
async fn success_response_carries_no_api_key() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(query_param("api_key", API_KEY))
        .and(query_param("url", TARGET))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>page</html>"))
        .mount(&mock)
        .await;

    let provider = SimpleHttpProvider::new(query_key_config(&format!("{}/v1/", mock.uri()))).unwrap();
    let response = provider.fetch(TARGET).await.unwrap();

    assert_eq!(response.body, "<html>page</html>");
    assert!(
        !format!("{response:?}").contains(API_KEY),
        "API key leaked into the response: {response:?}"
    );
    // The vendor reports no resolved URL, so the field stays empty rather than naming the vendor's endpoint.
    assert_eq!(response.final_url, "");
}

/// A guard, not a regression test: the status-error message never held the request URL, so
/// this passes with the `final_url`/`without_url` fix reverted. It pins the message to its
/// exact text instead, so a future change that appends the request URL — which carries a
/// query-parameter key — fails here rather than shipping.
// ~keep Keep the assertion exact. Weakening it to `!contains(API_KEY)` makes this test
// ~keep vacuous again: the message it guards has never contained a URL to redact.
#[tokio::test]
async fn status_error_names_only_the_vendor_and_status() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&mock)
        .await;

    let provider = SimpleHttpProvider::new(query_key_config(&format!("{}/v1/", mock.uri()))).unwrap();
    let err = provider.fetch(TARGET).await.unwrap_err();

    assert_eq!(err.to_string(), "server_error: querykey upstream 500");
}

#[tokio::test]
async fn send_error_carries_no_api_key() {
    // Bind and drop a listener so the port is closed and the connection is refused.
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();

    let provider = SimpleHttpProvider::new(query_key_config(&format!("http://127.0.0.1:{port}/v1/"))).unwrap();
    let err = provider.fetch(TARGET).await.unwrap_err();

    assert!(
        err.to_string().contains("request send failed"),
        "unexpected error: {err}"
    );
    assert!(
        !err.to_string().contains(API_KEY),
        "API key leaked into the error: {err}"
    );
}

#[tokio::test]
async fn body_read_error_carries_no_api_key() {
    // A server that promises a longer body than it sends, then closes the connection.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    // ~keep Every step is best-effort: the client may tear the connection down before or
    // ~keep during the write, and an `unwrap` here would panic the thread and resurface at
    // ~keep `join` as a failure of whichever assertion followed it.
    let server = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nshort");
        }
    });

    let provider = SimpleHttpProvider::new(query_key_config(&format!("http://127.0.0.1:{port}/v1/"))).unwrap();
    let err = provider.fetch(TARGET).await.unwrap_err();
    // ~keep The server thread's outcome is not what this test asserts; do not let it fail here.
    let _ = server.join();

    assert!(
        err.to_string().contains("response body read failed"),
        "unexpected error: {err}"
    );
    assert!(
        !err.to_string().contains(API_KEY),
        "API key leaked into the error: {err}"
    );
}
