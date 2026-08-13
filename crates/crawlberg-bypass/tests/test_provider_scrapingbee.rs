//! Wiremock tests for the ScrapingBee vendor config.
//!
//! Each test constructs a `ProviderConfig` matching `configs/scrapingbee.yaml`
//! in code and overrides the endpoint with the wiremock server URI.

use crawlberg::{BypassProvider, CrawlError};
use crawlberg_bypass::SimpleHttpProvider;
use crawlberg_bypass::config::{
    AuthScheme, CostExtraction, CrawlErrorKind, HttpMethod, ProviderConfig, RequestShape, ResponseKind, ResponseShape,
    StatusOverride, UrlParamLocation,
};
use wiremock::matchers::{header, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn make_scrapingbee_config(endpoint: &str) -> ProviderConfig {
    ProviderConfig {
        vendor_name: "scrapingbee".into(),
        endpoint: endpoint.into(),
        method: HttpMethod::Get,
        auth: AuthScheme::Header {
            name: "Spb-Api-Key".into(),
            value: "test-key".into(),
        },
        request: RequestShape {
            body: None,
            query: vec![
                ("render_js".into(), "true".into()),
                ("premium_proxy".into(), "true".into()),
            ],
            url_param: UrlParamLocation::QueryParam { name: "url".into() },
        },
        response: ResponseShape {
            kind: ResponseKind::RawBody,
            cost_extraction: CostExtraction::Static,
            fallback_cost_usd: Some(0.002),
        },
        status_mapping: vec![
            StatusOverride {
                http: 401,
                error: CrawlErrorKind::Unauthorized,
                message: Some("scrapingbee auth failure: 401".into()),
            },
            StatusOverride {
                http: 403,
                error: CrawlErrorKind::Unauthorized,
                message: Some("scrapingbee auth failure: 403".into()),
            },
            StatusOverride {
                http: 429,
                error: CrawlErrorKind::RateLimited,
                message: Some("scrapingbee rate limited".into()),
            },
        ],
    }
}

#[tokio::test]
async fn fetch_returns_html_on_success() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(header("Spb-Api-Key", "test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>scraped content</html>"))
        .mount(&mock)
        .await;

    let endpoint = format!("{}/api/v1/", mock.uri());
    let config = make_scrapingbee_config(&endpoint);
    let provider = SimpleHttpProvider::new(config).unwrap();

    let response = provider.fetch("https://example.com").await.unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.body, "<html>scraped content</html>");
    assert_eq!(response.content_type, "text/html");
    assert_eq!(response.cost_usd, Some(0.002));
}

#[tokio::test]
async fn fetch_maps_401_to_unauthorized() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(header("Spb-Api-Key", "test-key"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&mock)
        .await;

    let endpoint = format!("{}/api/v1/", mock.uri());
    let config = make_scrapingbee_config(&endpoint);
    let provider = SimpleHttpProvider::new(config).unwrap();

    let result = provider.fetch("https://example.com").await;
    assert!(matches!(result, Err(CrawlError::Unauthorized { .. })));
}

#[tokio::test]
async fn fetch_maps_403_to_unauthorized() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(header("Spb-Api-Key", "test-key"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&mock)
        .await;

    let endpoint = format!("{}/api/v1/", mock.uri());
    let config = make_scrapingbee_config(&endpoint);
    let provider = SimpleHttpProvider::new(config).unwrap();

    let result = provider.fetch("https://example.com").await;
    assert!(matches!(result, Err(CrawlError::Unauthorized { .. })));
}

#[tokio::test]
async fn fetch_maps_429_to_rate_limited() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(header("Spb-Api-Key", "test-key"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&mock)
        .await;

    let endpoint = format!("{}/api/v1/", mock.uri());
    let config = make_scrapingbee_config(&endpoint);
    let provider = SimpleHttpProvider::new(config).unwrap();

    let result = provider.fetch("https://example.com").await;
    assert!(matches!(result, Err(CrawlError::RateLimited { .. })));
}

#[tokio::test]
async fn fetch_maps_500_to_server_error() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(header("Spb-Api-Key", "test-key"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&mock)
        .await;

    let endpoint = format!("{}/api/v1/", mock.uri());
    let config = make_scrapingbee_config(&endpoint);
    let provider = SimpleHttpProvider::new(config).unwrap();

    let result = provider.fetch("https://example.com").await;
    assert!(matches!(result, Err(CrawlError::ServerError { .. })));
}

#[tokio::test]
async fn fetch_sends_get_with_api_key_header() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(header("Spb-Api-Key", "test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>content</html>"))
        .mount(&mock)
        .await;

    let endpoint = format!("{}/api/v1/", mock.uri());
    let config = make_scrapingbee_config(&endpoint);
    let provider = SimpleHttpProvider::new(config).unwrap();

    let response = provider.fetch("https://example.com").await.unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.body, "<html>content</html>");
}
