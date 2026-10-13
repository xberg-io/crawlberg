use super::super::CrawlEngine;
use crate::error::CrawlError;
use crate::traits::RateLimiter;
use crate::types::{AttemptOutcome, DispatchProfile, RetryDirective, RetryPolicy};
use futures::FutureExt;
use std::sync::Arc;
use std::time::Duration;
use wiremock::matchers::path;
use wiremock::{Mock, MockServer, ResponseTemplate};

struct PanicLimiter;
#[async_trait::async_trait]
impl RateLimiter for PanicLimiter {
    async fn acquire(&self, host: &str) -> Result<(), CrawlError> {
        if host == "localhost" {
            panic!("custom host admission panicked");
        }
        Ok(())
    }
    async fn record_response(&self, _: &str, _: u16) -> Result<(), CrawlError> {
        Ok(())
    }
    async fn set_crawl_delay(&self, _: &str, _: Duration) -> Result<(), CrawlError> {
        Ok(())
    }
}

#[derive(Debug)]
struct PanicPolicy;
#[async_trait::async_trait]
impl RetryPolicy for PanicPolicy {
    async fn decide(&self, outcome: &AttemptOutcome) -> RetryDirective {
        if outcome.url.ends_with("/panic") {
            panic!("custom retry policy panicked");
        }
        RetryDirective::Stop
    }
    fn name(&self) -> &'static str {
        "panic-test-policy"
    }
}

async fn assert_batch_isolates_custom_panic(pre_acquire: bool) {
    let site = MockServer::start().await;
    for endpoint in ["/panic", "/healthy"] {
        Mock::given(path(endpoint))
            .respond_with(ResponseTemplate::new(200).set_body_raw("<p>page</p>", "text/html"))
            .mount(&site)
            .await;
    }
    let bad = format!("{}/panic", site.uri().replace("127.0.0.1", "localhost"));
    let healthy = format!("{}/healthy", site.uri());
    let mut config = crate::CrawlConfig::builder().allow_private_networks(true).build();
    config.max_concurrent = Some(2);
    config.respect_robots_txt = false;
    config.rate_limit_ms = Some(0);
    if !pre_acquire {
        config.dispatch = Some(DispatchProfile {
            retry_policy: Some(Arc::new(PanicPolicy)),
            ..Default::default()
        });
    }
    let mut engine = CrawlEngine::builder().config(config).build().unwrap();
    if pre_acquire {
        engine.rate_limiter = Arc::new(PanicLimiter);
    }
    let urls = [bad.as_str(), healthy.as_str()];
    let outer = std::panic::AssertUnwindSafe(engine.batch_scrape(&urls))
        .catch_unwind()
        .await;
    let results = outer.expect("one custom panic must not unwind the entire batch");
    assert_eq!(results.len(), 2);
    let bad_result = &results.iter().find(|(url, _)| url == &bad).unwrap().1;
    assert!(
        bad_result.as_ref().unwrap_err().to_string().contains(&bad),
        "panic error preserves the input URL"
    );
    assert_eq!(
        results
            .iter()
            .find(|(url, _)| url == &healthy)
            .unwrap()
            .1
            .as_ref()
            .unwrap()
            .status_code,
        200
    );
    assert_eq!(
        site.received_requests().await.unwrap().len(),
        if pre_acquire { 1 } else { 2 }
    );
}

#[tokio::test]
async fn should_isolate_a_custom_limiter_panic_before_host_admission() {
    assert_batch_isolates_custom_panic(true).await;
}

#[tokio::test]
async fn should_isolate_a_custom_retry_policy_panic_inside_a_cursor_hop() {
    assert_batch_isolates_custom_panic(false).await;
}
