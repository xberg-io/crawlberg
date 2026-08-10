//! Verifies that BypassProvider trait can be implemented and attached to CrawlConfig.
//!
//! Full integration tests with real HTTP responses live in the
//! crawlberg-bypass crate's wiremock suite.

use std::sync::Arc;

use async_trait::async_trait;
use crawlberg::{BypassProvider, BypassResponse, CrawlConfig, CrawlError, DispatchProfile, DynBypassProvider};

/// Test provider that returns a canned response.
#[derive(Debug)]
struct TestProvider;

#[async_trait]
impl BypassProvider for TestProvider {
    async fn fetch(&self, _url: &str) -> Result<BypassResponse, CrawlError> {
        Ok(BypassResponse {
            status: 200,
            content_type: "text/html".to_string(),
            body: "<html>test</html>".to_string(),
            body_bytes: b"<html>test</html>".to_vec(),
            headers: std::collections::HashMap::new(),
            final_url: String::new(),
            cost_usd: Some(0.001),
            vendor_request_id: None,
        })
    }

    fn vendor_name(&self) -> &'static str {
        "test"
    }
}

#[test]
fn bypass_provider_can_be_attached_to_crawl_config() {
    let provider: DynBypassProvider = Arc::new(TestProvider);

    let config = CrawlConfig {
        dispatch: Some(DispatchProfile {
            bypass: Some(provider),
            ..DispatchProfile::default()
        }),
        ..Default::default()
    };

    let bypass = config.dispatch.as_ref().and_then(|d| d.bypass.as_ref());
    assert!(
        bypass.is_some(),
        "bypass provider should be attached via DispatchProfile"
    );
    assert_eq!(
        bypass.unwrap().vendor_name(),
        "test",
        "vendor_name should be accessible"
    );
}

#[tokio::test]
async fn bypass_provider_returns_response_with_cost() {
    let provider = TestProvider;
    let resp = provider.fetch("https://example.com").await.unwrap();
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, "<html>test</html>");
    assert_eq!(resp.cost_usd, Some(0.001));
    assert_eq!(resp.vendor_request_id, None, "TestProvider never sets a vendor request id");
}
