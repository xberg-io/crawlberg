//! Integration tests for the HTTP → Bypass → Browser dispatch chain.
//!
//! Each test instantiates a wiremock server, configures a `CrawlEngine` with a
//! specific `EscalationStrategy` (and optionally a `CountingMockProvider`), and
//! asserts chain behaviour against stubbed responses.
//!
//! The browser tier is not exercised here — browser tests require the `browser`
//! feature and a live Chrome instance.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use crawlberg::{
    AttemptOutcome, BudgetExhausted, BypassProvider, BypassResponse, CrawlConfig, CrawlEngine, CrawlError,
    DispatchProfile, EscalationBudget, EscalationStrategy, RetryDirective, RetryPolicy,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Bypass provider that returns a canned response and counts calls.
#[derive(Debug)]
struct CountingMockProvider {
    response: BypassResponse,
    calls: AtomicU32,
}

impl CountingMockProvider {
    fn new(body: &str) -> Arc<Self> {
        Arc::new(Self {
            response: BypassResponse {
                status: 200,
                content_type: "text/html".into(),
                body: format!("<html><body>{body}</body></html>"),
                body_bytes: format!("<html><body>{body}</body></html>").into_bytes(),
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

fn build_engine(config: CrawlConfig) -> CrawlEngine {
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

/// Extract the markdown text content from a `ScrapeResult`, returning an empty
/// string when no markdown was produced.
fn markdown_content(result: &crawlberg::ScrapeResult) -> &str {
    result.markdown.as_ref().map(|m| m.content.as_str()).unwrap_or("")
}

fn config_with(strategy: EscalationStrategy, provider: Option<Arc<CountingMockProvider>>) -> CrawlConfig {
    CrawlConfig {
        dispatch: Some(DispatchProfile {
            strategy,
            bypass: provider.map(|p| p as _),
            ..DispatchProfile::default()
        }),
        ..allow_private_config()
    }
}

/// HTTP success with `BypassThenBrowser` does not call bypass.
#[tokio::test]
async fn http_success_does_not_escalate() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/ok"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>ok</html>"))
        .mount(&mock)
        .await;

    let provider = CountingMockProvider::new("from bypass");
    let engine = build_engine(config_with(
        EscalationStrategy::BypassThenBrowser,
        Some(provider.clone()),
    ));

    let result = engine.scrape(&format!("{}/ok", mock.uri())).await.unwrap();
    assert!(markdown_content(&result).contains("ok"), "expected 'ok' in markdown");
    assert_eq!(provider.calls(), 0, "bypass must not be called on HTTP success");
}

/// HTTP success on the default path (no bypass configured, strategy BrowserOnly).
#[tokio::test]
async fn http_success_browser_only_strategy() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/page"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>browser-only ok</html>"))
        .mount(&mock)
        .await;

    let engine = build_engine(config_with(EscalationStrategy::BrowserOnly, None));
    let result = engine.scrape(&format!("{}/page", mock.uri())).await.unwrap();
    assert!(
        markdown_content(&result).contains("browser-only ok"),
        "expected page content in markdown"
    );
}

/// WAF-blocked HTTP escalates to bypass under `BypassOnly`.
#[tokio::test]
async fn waf_block_escalates_http_to_bypass() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/blocked"))
        .respond_with(
            ResponseTemplate::new(403)
                .insert_header("server", "cloudflare")
                .set_body_string("<html><head><title>Just a moment...</title></head></html>"),
        )
        .mount(&mock)
        .await;

    let provider = CountingMockProvider::new("vendor-fetched content");
    let engine = build_engine(config_with(EscalationStrategy::BypassOnly, Some(provider.clone())));

    let result = engine.scrape(&format!("{}/blocked", mock.uri())).await.unwrap();
    let markdown = markdown_content(&result);
    assert!(
        markdown.contains("vendor-fetched content"),
        "expected bypass content in markdown, got: {markdown:?}"
    );
    assert_eq!(provider.calls(), 1, "bypass must be called exactly once");
}

/// Explicit `BypassFirst` strategy routes all fetches through the bypass provider.
///
/// Callers must now set `DispatchProfile.strategy = BypassFirst` explicitly —
/// the pre-1.5.12 auto-promotion of `(BrowserOnly + bypass.is_some())` to
/// `BypassFirst` has been removed.
#[tokio::test]
async fn bypass_first_explicit_strategy_routes_through_bypass() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>http response</html>"))
        .mount(&mock)
        .await;

    let provider = CountingMockProvider::new("bypass response");

    let config = CrawlConfig {
        dispatch: Some(DispatchProfile {
            strategy: EscalationStrategy::BypassFirst,
            bypass: Some(provider.clone() as _),
            ..DispatchProfile::default()
        }),
        ..allow_private_config()
    };

    let engine = build_engine(config);
    let result = engine.scrape(&format!("{}/x", mock.uri())).await.unwrap();
    let markdown = markdown_content(&result);
    assert!(
        markdown.contains("bypass response"),
        "BypassFirst strategy must route through bypass; got: {markdown:?}"
    );
    assert_eq!(provider.calls(), 1, "bypass must be called exactly once");
}

/// `EscalationStrategy::None` propagates HTTP errors without escalation.
#[tokio::test]
async fn none_strategy_propagates_http_errors() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/forbidden"))
        .respond_with(ResponseTemplate::new(403).set_body_string("nope"))
        .mount(&mock)
        .await;

    let engine = build_engine(config_with(EscalationStrategy::None, None));
    let err = engine.scrape(&format!("{}/forbidden", mock.uri())).await.unwrap_err();
    assert!(
        matches!(err, CrawlError::Forbidden { .. } | CrawlError::WafBlocked { .. }),
        "expected Forbidden or WafBlocked, got: {err:?}"
    );
}

/// `BypassThenBrowser` skips browser when bypass succeeds after HTTP WAF block.
#[tokio::test]
async fn bypass_then_browser_skips_browser_when_bypass_succeeds() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/cf"))
        .respond_with(
            ResponseTemplate::new(403)
                .insert_header("server", "cloudflare")
                .set_body_string("<html>Just a moment...</html>"),
        )
        .mount(&mock)
        .await;

    let provider = CountingMockProvider::new("vendor content");
    let engine = build_engine(config_with(
        EscalationStrategy::BypassThenBrowser,
        Some(provider.clone()),
    ));

    let result = engine.scrape(&format!("{}/cf", mock.uri())).await.unwrap();
    let markdown = markdown_content(&result);
    assert!(
        markdown.contains("vendor content"),
        "expected bypass content, got: {markdown:?}"
    );
    assert_eq!(provider.calls(), 1, "bypass success must terminate chain");
}

/// `BypassOnly` with no bypass provider configured returns an error rather than panicking.
#[tokio::test]
async fn bypass_only_without_provider_returns_error() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/any"))
        .respond_with(
            ResponseTemplate::new(403)
                .insert_header("server", "cloudflare")
                .set_body_string("<html>blocked</html>"),
        )
        .mount(&mock)
        .await;

    let engine = build_engine(config_with(EscalationStrategy::BypassOnly, None));
    let err = engine.scrape(&format!("{}/any", mock.uri())).await.unwrap_err();
    assert!(
        matches!(
            err,
            CrawlError::Forbidden { .. }
                | CrawlError::WafBlocked { .. }
                | CrawlError::InvalidConfig { .. }
                | CrawlError::Other { .. }
        ),
        "expected escalation-related error, got: {err:?}"
    );
}

/// Budget exhaustion prevents escalation even on WAF block.
#[tokio::test]
async fn zero_budget_prevents_escalation() {
    #[derive(Debug)]
    struct ZeroBudget;

    #[async_trait]
    impl EscalationBudget for ZeroBudget {
        async fn try_consume(&self, _cost_cents: u32) -> Result<(), BudgetExhausted> {
            Err(BudgetExhausted)
        }
    }

    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/cf"))
        .respond_with(
            ResponseTemplate::new(403)
                .insert_header("server", "cloudflare")
                .set_body_string("<html>blocked</html>"),
        )
        .mount(&mock)
        .await;

    let provider = CountingMockProvider::new("bypass would succeed");
    let cfg = CrawlConfig {
        dispatch: Some(DispatchProfile {
            strategy: EscalationStrategy::BypassThenBrowser,
            bypass: Some(provider.clone() as _),
            escalation_budget: Some(Arc::new(ZeroBudget)),
            ..DispatchProfile::default()
        }),
        ..allow_private_config()
    };

    let engine = build_engine(cfg);
    let err = engine.scrape(&format!("{}/cf", mock.uri())).await.unwrap_err();

    assert!(
        matches!(err, CrawlError::Forbidden { .. } | CrawlError::WafBlocked { .. }),
        "budget exhaustion must surface HTTP error; got: {err:?}"
    );
    assert_eq!(provider.calls(), 0, "bypass must not be called when budget exhausted");
}

/// Unlimited budget always allows escalation.
#[tokio::test]
async fn unlimited_budget_allows_escalation() {
    #[derive(Debug)]
    struct AlwaysOkBudget;

    #[async_trait]
    impl EscalationBudget for AlwaysOkBudget {
        async fn try_consume(&self, _cost_cents: u32) -> Result<(), BudgetExhausted> {
            Ok(())
        }
    }

    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/cf"))
        .respond_with(
            ResponseTemplate::new(403)
                .insert_header("server", "cloudflare")
                .set_body_string("<html>Just a moment...</html>"),
        )
        .mount(&mock)
        .await;

    let provider = CountingMockProvider::new("bypass ok");
    let cfg = CrawlConfig {
        dispatch: Some(DispatchProfile {
            strategy: EscalationStrategy::BypassOnly,
            bypass: Some(provider.clone() as _),
            escalation_budget: Some(Arc::new(AlwaysOkBudget)),
            ..DispatchProfile::default()
        }),
        ..allow_private_config()
    };

    let engine = build_engine(cfg);
    let result = engine.scrape(&format!("{}/cf", mock.uri())).await.unwrap();
    let markdown = markdown_content(&result);
    assert!(
        markdown.contains("bypass ok"),
        "expected bypass content with unlimited budget, got: {markdown:?}"
    );
    assert_eq!(provider.calls(), 1);
}

/// Custom `RetryPolicy` that always returns `Retry { backoff_ms: 0 }`.
/// Used to prove the engine's global cap (`max_total_attempts`) terminates
/// the dispatch loop rather than spinning forever.
#[derive(Debug)]
struct AlwaysRetryPolicy;

#[async_trait]
impl RetryPolicy for AlwaysRetryPolicy {
    async fn decide(&self, _outcome: &AttemptOutcome) -> RetryDirective {
        RetryDirective::Retry { backoff_ms: 0 }
    }

    fn name(&self) -> &'static str {
        "always_retry"
    }
}

/// A buggy `RetryPolicy` returning `Retry` forever must be stopped by the
/// engine's `max_total_attempts` cap within a bounded wall-clock time.
#[tokio::test]
async fn buggy_policy_returning_retry_forever_does_not_spin() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/timeout"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&mock)
        .await;

    let config = CrawlConfig {
        dispatch: Some(DispatchProfile {
            strategy: EscalationStrategy::BrowserOnly,
            retry_policy: Some(Arc::new(AlwaysRetryPolicy)),
            max_total_attempts: 5,
            ..DispatchProfile::default()
        }),
        ..allow_private_config()
    };

    let engine = build_engine(config);

    let start = std::time::Instant::now();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        engine.scrape(&format!("{}/timeout", mock.uri())),
    )
    .await;
    let elapsed = start.elapsed();

    assert!(
        result.is_ok(),
        "engine spun past 5s — max_total_attempts cap did not fire; elapsed = {elapsed:?}"
    );
}

/// Wiremock returns HTTP 200 with a Cloudflare Turnstile challenge HTML body.
/// With `waf_classifier` wired, the dispatcher must detect the block-page and
/// escalate to the bypass tier. Without the wiring (Session 1 pre-fix state),
/// the dispatcher would silently return the challenge HTML as extracted content.
#[tokio::test]
async fn turnstile_challenge_html_triggers_escalation() {
    let mock = MockServer::start().await;
    let challenge_html = concat!(
        "<!DOCTYPE html><html><head><title>Just a moment...</title></head>",
        "<body><script src=\"/cdn-cgi/challenge-platform/h/g/orchestrate/chl_page/v1\"></script>",
        "Please verify you are human.</body></html>"
    );
    Mock::given(method("GET"))
        .and(path("/cf-turnstile"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("server", "cloudflare")
                .set_body_string(challenge_html),
        )
        .mount(&mock)
        .await;

    let provider = CountingMockProvider::new("vendor-fetched content");
    let config = CrawlConfig {
        dispatch: Some(DispatchProfile {
            strategy: EscalationStrategy::BypassOnly,
            bypass: Some(provider.clone() as _),
            waf_classifier: Some(Arc::new(crawlberg::TomlClassifier::builtin())),
            ..DispatchProfile::default()
        }),
        ..allow_private_config()
    };

    let engine = build_engine(config);
    let result = engine.scrape(&format!("{}/cf-turnstile", mock.uri())).await.unwrap();
    let markdown = markdown_content(&result);
    assert!(
        markdown.contains("vendor-fetched"),
        "engine must escalate to bypass when 200 returns a CF challenge; got: {markdown:?}"
    );
    assert_eq!(provider.calls(), 1, "bypass must be called exactly once");
}

/// Records whether `post_response` was invoked and whether the `HttpResponse`
/// it received actually carries the mocked body, so a broken
/// `needs_http_resp_for_hooks` gate (built only when a WAF classifier OR an
/// antibot strategy is configured) is caught even when no classifier is wired.
#[derive(Debug, Default)]
struct RecordingAntibotStrategy {
    called: std::sync::atomic::AtomicBool,
    saw_body: std::sync::Mutex<Option<String>>,
}

#[async_trait]
impl crawlberg::AntibotStrategy for RecordingAntibotStrategy {
    async fn pre_request(&self, _url: &str) -> Result<(), crawlberg::AntibotError> {
        Ok(())
    }

    async fn post_response(
        &self,
        response: &crawlberg::http::HttpResponse,
        _waf_signal: Option<&crawlberg::WafSignal>,
    ) -> crawlberg::Decision {
        self.called.store(true, Ordering::SeqCst);
        *self.saw_body.lock().unwrap() = Some(response.body.clone());
        crawlberg::Decision::Accept
    }
}

/// With only an `antibot_strategy` configured (no `waf_classifier`), the
/// dispatcher must still build `HttpResponse` and invoke `post_response` with
/// the real fetched body. Regression test for the allocation-gating change in
/// `fetch_response`: `needs_http_resp_for_hooks` must be true whenever either
/// hook is configured, not only when a WAF classifier is present.
#[tokio::test]
async fn antibot_strategy_receives_response_body_without_waf_classifier() {
    let mock = MockServer::start().await;
    let body = "<html><body>plain content, no WAF signature</body></html>";
    Mock::given(method("GET"))
        .and(path("/antibot-only"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(&mock)
        .await;

    let strategy = Arc::new(RecordingAntibotStrategy::default());
    let config = CrawlConfig {
        dispatch: Some(DispatchProfile {
            antibot_strategy: Some(strategy.clone() as _),
            waf_classifier: None,
            ..DispatchProfile::default()
        }),
        ..allow_private_config()
    };

    let engine = build_engine(config);
    engine.scrape(&format!("{}/antibot-only", mock.uri())).await.unwrap();

    assert!(
        strategy.called.load(Ordering::SeqCst),
        "post_response must fire when antibot_strategy is configured, even without a waf_classifier"
    );
    assert_eq!(
        strategy.saw_body.lock().unwrap().as_deref(),
        Some(body),
        "post_response must receive the real fetched body, not an empty/default HttpResponse"
    );
}

/// Wiremock returns an HTML body with meaningful text content.
/// The retry policy records the `content_density` from `AttemptOutcome` and the
/// test asserts that it is in the expected range (> 0.2) rather than being the
/// previously hardcoded 0.0.
#[tokio::test]
async fn content_density_populated_for_html_response() {
    use std::sync::Mutex;

    let mock = MockServer::start().await;
    let html_body = "<html><body><p>Hello world</p><p>More content here</p></body></html>";
    Mock::given(method("GET"))
        .and(path("/dense"))
        .respond_with(ResponseTemplate::new(200).set_body_string(html_body))
        .mount(&mock)
        .await;

    #[derive(Debug)]
    struct RecordingPolicy(Arc<Mutex<f32>>);

    #[async_trait::async_trait]
    impl RetryPolicy for RecordingPolicy {
        async fn decide(&self, outcome: &AttemptOutcome) -> RetryDirective {
            *self.0.lock().unwrap() = outcome.content_density;
            RetryDirective::Stop
        }

        fn name(&self) -> &'static str {
            "recording"
        }
    }

    let recorded = Arc::new(Mutex::new(-1.0_f32));
    let config = CrawlConfig {
        dispatch: Some(DispatchProfile {
            retry_policy: Some(Arc::new(RecordingPolicy(recorded.clone()))),
            ..DispatchProfile::default()
        }),
        ..allow_private_config()
    };
    let engine = build_engine(config);
    let _ = engine.scrape(&format!("{}/dense", mock.uri())).await.unwrap();

    let observed = *recorded.lock().unwrap();
    assert!(
        observed > 0.2 && observed < 0.8,
        "expected content_density in (0.2, 0.8) for an HTML body with real text, got {observed}"
    );
}

// --- crawlberg#169: a challenge served with 503/429 must escalate, not be retried blindly ---

/// A config that lists the challenge statuses in `retry_codes` and retries fast, so the
/// escalate-vs-retry choice is actually contested rather than decided by an empty allowlist.
fn contested_retry_config(strategy: EscalationStrategy, provider: Arc<CountingMockProvider>) -> CrawlConfig {
    CrawlConfig {
        retry_count: 3,
        retry_codes: vec![429, 503],
        retry_initial_delay_ms: 1,
        retry_max_delay_ms: 5,
        ..config_with(strategy, Some(provider))
    }
}

/// A Cloudflare challenge served with 503 escalates to the next tier on the first attempt,
/// even though 503 is in `retry_codes`.
///
/// Escalation and retry are different directives and only one can be returned: re-issuing the
/// identical JS-less request reproduces the challenge, so the retry budget would be spent for
/// nothing and the browser tier never reached. The single-request assertion is what pins that
/// choice — without it the test would pass on a policy that retried three times first.
#[tokio::test]
async fn a_503_cloudflare_challenge_escalates_instead_of_being_retried() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/chl"))
        .respond_with(
            ResponseTemplate::new(503)
                .insert_header("server", "cloudflare")
                .insert_header("content-type", "text/html")
                .set_body_string(
                    "<html><head><title>Just a moment...</title></head><body>\
                     <script src=\"/cdn-cgi/challenge-platform/h/g/orchestrate/chl_page/v1\"></script>\
                     </body></html>",
                ),
        )
        .mount(&mock)
        .await;

    let provider = CountingMockProvider::new("vendor-fetched content");
    let engine = build_engine(contested_retry_config(EscalationStrategy::BypassOnly, provider.clone()));

    let result = engine.scrape(&format!("{}/chl", mock.uri())).await.unwrap();
    let markdown = markdown_content(&result);
    assert!(
        markdown.contains("vendor-fetched content"),
        "a 503 challenge must be served from the escalated tier, got: {markdown:?}"
    );
    assert_eq!(provider.calls(), 1, "the bypass tier must be reached exactly once");
    assert_eq!(
        mock.received_requests().await.unwrap().len(),
        1,
        "the challenge must not be re-requested before escalating"
    );
}

/// The same for a 429 challenge, and for a vendor identified by a response header alone —
/// which also proves the header check runs before any body is read.
#[tokio::test]
async fn a_429_datadome_challenge_escalates_from_its_headers_alone() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/dd"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("x-datadome", "blocked")
                .insert_header("content-type", "text/html")
                .set_body_string("<html></html>"),
        )
        .mount(&mock)
        .await;

    let provider = CountingMockProvider::new("datadome bypassed");
    let engine = build_engine(contested_retry_config(EscalationStrategy::BypassOnly, provider.clone()));

    let result = engine.scrape(&format!("{}/dd", mock.uri())).await.unwrap();
    let markdown = markdown_content(&result);
    assert!(
        markdown.contains("datadome bypassed"),
        "a 429 challenge must be served from the escalated tier, got: {markdown:?}"
    );
    assert_eq!(provider.calls(), 1, "the bypass tier must be reached exactly once");
    assert_eq!(
        mock.received_requests().await.unwrap().len(),
        1,
        "the challenge must not be re-requested before escalating"
    );
}

/// Guard, not a red-green test: a 503 that carries no WAF fingerprint must keep the
/// crawlberg#84 behaviour exactly — retried for as long as `retry_codes` allows, never
/// escalated, and surfaced as a `ServerError`. It passes with and without the
/// challenge-status change; its job is to fail if that change ever widens to plain 503s.
///
/// ~keep The request count is the assertion that matters. A version of this test that only
/// checked the error variant would also pass if the 503 escalated first and the bypass tier
/// then failed.
#[tokio::test]
async fn a_503_without_a_waf_signal_is_retried_and_never_escalates() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/plain"))
        .respond_with(
            ResponseTemplate::new(503)
                .insert_header("content-type", "text/html")
                .set_body_string("<html><body><h1>Service Unavailable</h1></body></html>"),
        )
        .mount(&mock)
        .await;

    let provider = CountingMockProvider::new("must not be used");
    let engine = build_engine(contested_retry_config(EscalationStrategy::BypassOnly, provider.clone()));

    let error = engine.scrape(&format!("{}/plain", mock.uri())).await.unwrap_err();
    assert!(
        matches!(error, CrawlError::ServerError { .. }),
        "a plain 503 must stay a ServerError, got: {error:?}"
    );
    assert_eq!(provider.calls(), 0, "a plain 503 must not escalate");
    assert_eq!(
        mock.received_requests().await.unwrap().len(),
        4,
        "retry_count=3 with 503 in retry_codes must send 4 requests"
    );
}
