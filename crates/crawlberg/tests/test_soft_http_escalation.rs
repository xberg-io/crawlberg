use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use crawlberg::{
    AttemptOutcome, BrowserMode, BypassProvider, BypassResponse, CrawlConfig, CrawlEngine, CrawlError, DispatchProfile,
    EscalationReason, EscalationStrategy, RetryDirective, RetryPolicy, Tier,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Debug, Default)]
struct CountingBypass {
    calls: AtomicU32,
}

#[async_trait]
impl BypassProvider for CountingBypass {
    async fn fetch(&self, _url: &str) -> Result<BypassResponse, CrawlError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let body = "<html><body>bypass page</body></html>".to_owned();
        Ok(BypassResponse {
            status: 200,
            content_type: "text/html".into(),
            body_bytes: body.clone().into_bytes(),
            body_kind: crawlberg::BypassBody::Bytes,
            body,
            headers: Default::default(),
            final_url: String::new(),
            cost_usd: None,
            vendor_request_id: None,
        })
    }

    fn vendor_name(&self) -> &'static str {
        "counting"
    }
}

#[derive(Debug)]
struct RefuseHttpSuccess;

#[async_trait]
impl RetryPolicy for RefuseHttpSuccess {
    async fn decide(&self, outcome: &AttemptOutcome) -> RetryDirective {
        if outcome.error.is_none() && outcome.previous_tier == Tier::Http {
            RetryDirective::Escalate {
                reason: EscalationReason::WafBlocked {
                    vendor: "unknown".into(),
                },
            }
        } else {
            RetryDirective::Stop
        }
    }

    fn name(&self) -> &'static str {
        "refuse-http-success"
    }
}

fn engine(bypass: Arc<CountingBypass>, retry_policy: Option<Arc<dyn RetryPolicy>>) -> CrawlEngine {
    let mut config = CrawlConfig::builder().allow_private_networks(true).build();
    config.soft_http_errors = true;
    config.browser.mode = BrowserMode::Never;
    config.dispatch = Some(DispatchProfile {
        retry_policy,
        strategy: EscalationStrategy::BypassOnly,
        bypass: Some(bypass),
        ..DispatchProfile::default()
    });
    CrawlEngine::builder().config(config).build().unwrap()
}

#[tokio::test]
async fn soft_http_errors_escalates_default_and_custom_waf_refusals_equally() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/default"))
        .respond_with(
            ResponseTemplate::new(403)
                .insert_header("content-type", "text/html")
                .set_body_string("<html><title>Attention Required! | Cloudflare</title>cf-chl- x</html>"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/custom"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/html")
                .set_body_string("<html><body>origin page</body></html>"),
        )
        .mount(&server)
        .await;

    let default_bypass = Arc::new(CountingBypass::default());
    let custom_bypass = Arc::new(CountingBypass::default());
    let default = engine(default_bypass.clone(), None)
        .scrape(&format!("{}/default", server.uri()))
        .await
        .unwrap();
    let custom = engine(custom_bypass.clone(), Some(Arc::new(RefuseHttpSuccess)))
        .scrape(&format!("{}/custom", server.uri()))
        .await
        .unwrap();

    assert_eq!(
        default_bypass.calls.load(Ordering::SeqCst),
        1,
        "soft error reporting must not skip the default policy's requested escalation"
    );
    assert_eq!(
        custom_bypass.calls.load(Ordering::SeqCst),
        1,
        "the equivalent custom refusal must escalate exactly once"
    );
    assert_eq!(
        (default.status_code, default.html, default.browser_used),
        (custom.status_code, custom.html, custom.browser_used),
        "policy selection must not change the result after equivalent escalation"
    );
}
