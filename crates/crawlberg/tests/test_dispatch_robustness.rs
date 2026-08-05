//! Dispatch robustness integration tests (Wave 6, commit 1.5.8b).
//!
//! Coverage gaps addressed:
//!
//! - T11 / M8: `soft_http_errors` × `BypassThenBrowser` interaction.
//!   Pins the semantics of whether `soft_http_errors = true` short-circuits
//!   escalation when a plain 403 arrives (no WAF signal, no challenge body).
//!
//! Tests T10 (EWMA oscillation) and T12 (LearningRetryPolicy invalid URL) are
//! NOT included here because `EwmaDomainState` and `LearningRetryPolicy` are
//! in `pub(crate) mod defaults` and are not re-exported at the crate root.
//! Integration tests (separate crate) cannot reach them without a visibility
//! change. Those gaps must be covered by unit tests inside
//! `crates/crawlberg/src/defaults/domain_state.rs` or by a future re-export.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};

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
        ..CrawlConfig::default()
    };
    allow_private_network();
    CrawlEngine::builder().config(config).build().unwrap()
}

static ALLOW_PRIVATE: OnceLock<()> = OnceLock::new();

/// Opts into the SSRF policy's private-network allowance so wiremock's
/// 127.0.0.1 servers are reachable. Without this, the engine's SSRF check
/// rejects the loopback URL before the escalation semantics under test run.
fn allow_private_network() {
    ALLOW_PRIVATE.get_or_init(|| {
        // ~keep SAFETY: OnceLock writes this env var once before any network call is made.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "1");
        }
    });
}

/// When `soft_http_errors = true`, a plain 403 (no WAF signal, no challenge
/// body) must be short-circuited to `Ok(ScrapeResult { status_code: 403 })`
/// **before** the retry policy runs. The bypass provider must NOT be called.
///
/// Implementation note: `engine/mod.rs` checks `soft_http_errors` at the
/// `Err(CrawlError::Forbidden)` arm and returns early with a synthesised
/// response. Escalation is a code path after that arm — it is never reached.
#[tokio::test]
async fn soft_http_403_does_not_escalate_when_soft_errors_enabled() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/forbidden"))
        .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
        .mount(&mock)
        .await;

    let provider = CountingMockProvider::new("bypass content");
    let engine = build_engine(provider.clone(), true);

    let result = engine.scrape(&format!("{}/forbidden", mock.uri())).await;

    assert!(
        result.is_ok(),
        "soft_http_errors = true must convert 403 to Ok; got Err: {:?}",
        result.err()
    );
    let page = result.unwrap();
    assert_eq!(
        page.status_code, 403,
        "synthesised response must carry status_code = 403"
    );
    assert_eq!(
        provider.calls(),
        0,
        "bypass must NOT be called when soft_http_errors short-circuits escalation"
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
