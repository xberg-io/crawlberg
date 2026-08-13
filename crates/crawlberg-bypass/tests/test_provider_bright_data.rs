//! Wiremock tests for the Bright Data vendor config.
//!
//! Each test constructs a `ProviderConfig` matching `configs/bright_data.yaml`
//! in code and overrides the endpoint with the wiremock server URI.

use crawlberg::{BypassProvider, CrawlError};
use crawlberg_bypass::SimpleHttpProvider;
use crawlberg_bypass::config::{
    AuthScheme, CostExtraction, CrawlErrorKind, HttpMethod, ProviderConfig, RequestBody, RequestShape, ResponseKind,
    ResponseShape, StatusOverride, UrlParamLocation,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn make_bright_data_config(endpoint: &str) -> ProviderConfig {
    ProviderConfig {
        vendor_name: "bright_data".into(),
        endpoint: endpoint.into(),
        method: HttpMethod::Post,
        auth: AuthScheme::Bearer {
            token: "test-token".into(),
        },
        request: RequestShape {
            body: Some(RequestBody::Json {
                template: r#"{"url": "{{url}}", "format": "raw", "country": "us"}"#.into(),
            }),
            query: Vec::new(),
            url_param: UrlParamLocation::BodyField,
        },
        response: ResponseShape {
            kind: ResponseKind::RawBody,
            cost_extraction: CostExtraction::Static,
            fallback_cost_usd: Some(0.003),
        },
        status_mapping: vec![
            StatusOverride {
                http: 401,
                error: CrawlErrorKind::Unauthorized,
                message: Some("bright_data auth failure: 401".into()),
            },
            StatusOverride {
                http: 402,
                error: CrawlErrorKind::Unauthorized,
                message: Some("bright_data quota exceeded: 402".into()),
            },
            StatusOverride {
                http: 429,
                error: CrawlErrorKind::RateLimited,
                message: Some("bright_data rate limited".into()),
            },
        ],
    }
}

#[tokio::test]
async fn fetch_returns_body_on_success() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/request"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>real content</html>"))
        .mount(&mock)
        .await;

    let endpoint = format!("{}/request", mock.uri());
    let config = make_bright_data_config(&endpoint);
    let provider = SimpleHttpProvider::new(config).unwrap();

    let response = provider.fetch("https://example.com").await.unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.body, "<html>real content</html>");
    assert_eq!(response.content_type, "text/html");
    assert_eq!(response.cost_usd, Some(0.003));
}

#[tokio::test]
async fn fetch_maps_401_to_unauthorized() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/request"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&mock)
        .await;

    let endpoint = format!("{}/request", mock.uri());
    let config = make_bright_data_config(&endpoint);
    let provider = SimpleHttpProvider::new(config).unwrap();

    let result = provider.fetch("https://example.com").await;
    assert!(matches!(result, Err(CrawlError::Unauthorized { .. })));
}

#[tokio::test]
async fn fetch_maps_402_to_unauthorized() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/request"))
        .respond_with(ResponseTemplate::new(402))
        .mount(&mock)
        .await;

    let endpoint = format!("{}/request", mock.uri());
    let config = make_bright_data_config(&endpoint);
    let provider = SimpleHttpProvider::new(config).unwrap();

    let result = provider.fetch("https://example.com").await;
    assert!(matches!(result, Err(CrawlError::Unauthorized { .. })));
}

#[tokio::test]
async fn fetch_maps_429_to_rate_limited() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/request"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&mock)
        .await;

    let endpoint = format!("{}/request", mock.uri());
    let config = make_bright_data_config(&endpoint);
    let provider = SimpleHttpProvider::new(config).unwrap();

    let result = provider.fetch("https://example.com").await;
    assert!(matches!(result, Err(CrawlError::RateLimited { .. })));
}

#[tokio::test]
async fn fetch_maps_500_to_server_error() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/request"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&mock)
        .await;

    let endpoint = format!("{}/request", mock.uri());
    let config = make_bright_data_config(&endpoint);
    let provider = SimpleHttpProvider::new(config).unwrap();

    let result = provider.fetch("https://example.com").await;
    assert!(matches!(result, Err(CrawlError::ServerError { .. })));
}

#[tokio::test]
async fn fetch_sends_post_with_url_in_body() {
    use wiremock::matchers::body_string_contains;

    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/request"))
        .and(body_string_contains("https://example.com"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>ok</html>"))
        .mount(&mock)
        .await;

    let endpoint = format!("{}/request", mock.uri());
    let config = make_bright_data_config(&endpoint);
    let provider = SimpleHttpProvider::new(config).unwrap();

    let response = provider.fetch("https://example.com").await.unwrap();
    assert_eq!(response.status, 200);
}

#[tokio::test]
async fn fetch_includes_bearer_auth_header() {
    use wiremock::matchers::header_regex;

    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/request"))
        .and(header_regex("authorization", "Bearer .*"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>auth ok</html>"))
        .mount(&mock)
        .await;

    let endpoint = format!("{}/request", mock.uri());
    let config = make_bright_data_config(&endpoint);
    let provider = SimpleHttpProvider::new(config).unwrap();

    let response = provider.fetch("https://example.com").await.unwrap();
    assert_eq!(response.status, 200);
}
