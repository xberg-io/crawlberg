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

#[tokio::test]
async fn status_error_carries_no_api_key() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&mock)
        .await;

    let provider = SimpleHttpProvider::new(query_key_config(&format!("{}/v1/", mock.uri()))).unwrap();
    let err = provider.fetch(TARGET).await.unwrap_err();

    assert!(
        !err.to_string().contains(API_KEY),
        "API key leaked into the error: {err}"
    );
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
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf).unwrap();
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nshort")
            .unwrap();
    });

    let provider = SimpleHttpProvider::new(query_key_config(&format!("http://127.0.0.1:{port}/v1/"))).unwrap();
    let err = provider.fetch(TARGET).await.unwrap_err();
    server.join().unwrap();

    assert!(
        err.to_string().contains("response body read failed"),
        "unexpected error: {err}"
    );
    assert!(
        !err.to_string().contains(API_KEY),
        "API key leaked into the error: {err}"
    );
}
