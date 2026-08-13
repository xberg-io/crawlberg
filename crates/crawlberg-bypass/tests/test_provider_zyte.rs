//! Wiremock tests for the Zyte vendor config.
//!
//! Each test constructs a `ProviderConfig` matching `configs/zyte.yaml`
//! in code and overrides the endpoint with the wiremock server URI.

use crawlberg::{BypassProvider, CrawlError};
use crawlberg_bypass::SimpleHttpProvider;
use crawlberg_bypass::config::{
    AuthScheme, CostExtraction, CrawlErrorKind, HttpMethod, ProviderConfig, RequestBody, RequestShape, ResponseKind,
    ResponseShape, StatusOverride, UrlParamLocation,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn make_zyte_config(endpoint: &str) -> ProviderConfig {
    ProviderConfig {
        vendor_name: "zyte".into(),
        endpoint: endpoint.into(),
        method: HttpMethod::Post,
        auth: AuthScheme::BasicUsername {
            username: "test-key".into(),
        },
        request: RequestShape {
            body: Some(RequestBody::Json {
                template: r#"{"url": "{{url}}", "browserHtml": true}"#.into(),
            }),
            query: Vec::new(),
            url_param: UrlParamLocation::BodyField,
        },
        response: ResponseShape {
            kind: ResponseKind::JsonField {
                html_field: "browserHtml".into(),
            },
            cost_extraction: CostExtraction::Static,
            fallback_cost_usd: Some(0.003),
        },
        status_mapping: vec![
            StatusOverride {
                http: 401,
                error: CrawlErrorKind::Unauthorized,
                message: Some("zyte auth failure".into()),
            },
            StatusOverride {
                http: 429,
                error: CrawlErrorKind::RateLimited,
                message: Some("zyte rate limited".into()),
            },
        ],
    }
}

#[tokio::test]
async fn fetch_returns_browser_html_on_success() {
    let mock = MockServer::start().await;
    let response_json = serde_json::json!({
        "browserHtml": "<html>rendered content</html>"
    });
    Mock::given(method("POST"))
        .and(path("/v1/extract"))
        .respond_with(ResponseTemplate::new(200).set_body_string(response_json.to_string()))
        .mount(&mock)
        .await;

    let endpoint = format!("{}/v1/extract", mock.uri());
    let config = make_zyte_config(&endpoint);
    let provider = SimpleHttpProvider::new(config).unwrap();

    let response = provider.fetch("https://example.com").await.unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.body, "<html>rendered content</html>");
    assert_eq!(response.content_type, "text/html");
    assert_eq!(response.cost_usd, Some(0.003));
}

#[tokio::test]
async fn fetch_maps_401_to_unauthorized() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/extract"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&mock)
        .await;

    let endpoint = format!("{}/v1/extract", mock.uri());
    let config = make_zyte_config(&endpoint);
    let provider = SimpleHttpProvider::new(config).unwrap();

    let result = provider.fetch("https://example.com").await;
    assert!(matches!(result, Err(CrawlError::Unauthorized { .. })));
}

#[tokio::test]
async fn fetch_maps_429_to_rate_limited() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/extract"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&mock)
        .await;

    let endpoint = format!("{}/v1/extract", mock.uri());
    let config = make_zyte_config(&endpoint);
    let provider = SimpleHttpProvider::new(config).unwrap();

    let result = provider.fetch("https://example.com").await;
    assert!(matches!(result, Err(CrawlError::RateLimited { .. })));
}

#[tokio::test]
async fn fetch_maps_500_to_server_error() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/extract"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&mock)
        .await;

    let endpoint = format!("{}/v1/extract", mock.uri());
    let config = make_zyte_config(&endpoint);
    let provider = SimpleHttpProvider::new(config).unwrap();

    let result = provider.fetch("https://example.com").await;
    assert!(matches!(result, Err(CrawlError::ServerError { .. })));
}

#[tokio::test]
async fn fetch_extracts_browser_html_json_field() {
    let mock = MockServer::start().await;
    let response_json = serde_json::json!({
        "browserHtml": "<html>js-rendered</html>",
        "url": "https://example.com"
    });
    Mock::given(method("POST"))
        .and(path("/v1/extract"))
        .respond_with(ResponseTemplate::new(200).set_body_string(response_json.to_string()))
        .mount(&mock)
        .await;

    let endpoint = format!("{}/v1/extract", mock.uri());
    let config = make_zyte_config(&endpoint);
    let provider = SimpleHttpProvider::new(config).unwrap();

    let response = provider.fetch("https://example.com").await.unwrap();
    assert_eq!(response.body, "<html>js-rendered</html>");
    assert_eq!(response.body_bytes, b"<html>js-rendered</html>");
}
