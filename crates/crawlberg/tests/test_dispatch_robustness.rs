//! Dispatch robustness integration tests (Wave 6, commit 1.5.8b).
//!
//! Coverage gaps addressed:
//!
//! - T11 / M8: `soft_http_errors` × `BypassThenBrowser` interaction.
//!   ~keep Pins that `soft_http_errors = true` does not short-circuit escalation
//!   ~keep when a plain 403 arrives (no WAF signal, no challenge body).
//!
//! Tests T10 (EWMA oscillation) and T12 (LearningRetryPolicy invalid URL) are
//! NOT included here because `EwmaDomainState` and `LearningRetryPolicy` are
//! in `pub(crate) mod defaults` and are not re-exported at the crate root.
//! Integration tests (separate crate) cannot reach them without a visibility
//! change. Those gaps must be covered by unit tests inside
//! `crates/crawlberg/src/defaults/domain_state.rs` or by a future re-export.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use crawlberg::{
    BrowserMode, BypassProvider, BypassResponse, CrawlConfig, CrawlEngine, CrawlError, DispatchProfile,
    EscalationStrategy,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Bypass provider that counts calls, used to assert whether escalation fired.
#[derive(Debug)]
struct CountingMockProvider {
    response: BypassResponse,
    calls: AtomicU32,
}

impl CountingMockProvider {
    fn new(body: &str) -> Arc<Self> {
        let html = format!("<html><body>{body}</body></html>");
        Arc::new(Self {
            response: BypassResponse {
                status: 200,
                content_type: "text/html".into(),
                body: html.clone(),
                body_bytes: html.into_bytes(),
                headers: Default::default(),
                final_url: String::new(),
                cost_usd: Some(0.0015),
                vendor_request_id: None,
            },
            calls: AtomicU32::new(0),
        })
    }

    fn calls(&self) -> u32 {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl BypassProvider for CountingMockProvider {
    async fn fetch(&self, _url: &str) -> Result<BypassResponse, CrawlError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.response.clone())
    }

    fn vendor_name(&self) -> &'static str {
        "mock"
    }
}

/// Build a `CrawlEngine` with `BypassThenBrowser` strategy, the given bypass
/// provider, and the given `soft_http_errors` flag. `BrowserMode::Never` is
/// set to keep these tests focused on the HTTP→Bypass escalation edge; browser
/// escalation requires a live Chrome instance and the `browser` feature.
fn build_engine(provider: Arc<CountingMockProvider>, soft_http_errors: bool) -> CrawlEngine {
    let config = CrawlConfig {
        soft_http_errors,
        browser: crawlberg::BrowserConfig {
            mode: BrowserMode::Never,
            ..Default::default()
        },
        dispatch: Some(DispatchProfile {
            strategy: EscalationStrategy::BypassThenBrowser,
            bypass: Some(provider as _),
            ..DispatchProfile::default()
        }),
        ..allow_private_config()
    };
    CrawlEngine::builder().config(config).build().unwrap()
}

/// Builds a `CrawlConfig` whose SSRF policy permits private networks, so wiremock's
/// 127.0.0.1 servers are reachable.
///
// ~keep Uses the `allow_private_networks` config seam rather than the
// `CRAWLBERG_ALLOW_PRIVATE_NETWORK` env var: writing that variable is a process-global mutation
// that races every concurrent `std::env::var` read (`SsrfPolicy::from_env`, reached from
// `CrawlConfig::default()`) in this binary's other tests, aborting the process on glibc
// with no failing test name.
fn allow_private_config() -> CrawlConfig {
    CrawlConfig::builder().allow_private_networks(true).build()
}

/// ~keep `soft_http_errors` changes only a terminal refusal. A default-policy
/// ~keep escalation still reaches the bypass tier, whose successful response wins.
#[tokio::test]
async fn soft_http_403_escalates_before_soft_error_reporting() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/forbidden"))
        .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
        .mount(&mock)
        .await;

    let provider = CountingMockProvider::new("bypass content");
    let engine = build_engine(provider.clone(), true);

    let result = engine.scrape(&format!("{}/forbidden", mock.uri())).await;

    let page = result.unwrap_or_else(|err| panic!("bypass success must propagate as Ok, got {err:?}"));
    assert_eq!(
        page.status_code, 200,
        "the bypass response must replace the refused 403"
    );
    assert_eq!(
        page.html, "<html><body>bypass content</body></html>",
        "the result must contain the bypass response body"
    );
    assert!(
        !page.browser_used,
        "the bypass tier must not be reported as browser rendering"
    );
    assert_eq!(
        provider.calls(),
        1,
        "soft error reporting must not skip default-policy escalation"
    );
}

/// When `soft_http_errors = false`, a plain 403 must NOT be short-circuited.
/// The default `SimpleRetryPolicy` maps `CrawlError::Forbidden` →
/// `RetryDirective::Escalate`, so the engine must escalate to the bypass tier
/// and call the bypass provider exactly once.
#[tokio::test]
async fn soft_http_403_does_escalate_when_soft_errors_disabled() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/forbidden"))
        .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
        .mount(&mock)
        .await;

    let provider = CountingMockProvider::new("bypass content");
    let engine = build_engine(provider.clone(), false);

    let result = engine.scrape(&format!("{}/forbidden", mock.uri())).await;

    assert!(
        result.is_ok(),
        "soft_http_errors = false + BypassThenBrowser: bypass success must propagate as Ok; \
         got Err: {:?}",
        result.err()
    );
    assert_eq!(
        provider.calls(),
        1,
        "bypass must be called exactly once when soft_http_errors = false \
         and BypassThenBrowser strategy is active"
    );
}
