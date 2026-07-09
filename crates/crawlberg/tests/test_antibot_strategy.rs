//! Integration tests for `AntibotStrategy` and `Decision` dispatch wiring.
//!
//! Each test inserts a mock `AntibotStrategy` into `DispatchProfile` and
//! drives a `scrape` call against a wiremock server to prove that each
//! `Decision` variant is handled correctly by the engine.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use crawlberg::http::HttpResponse;
use crawlberg::{
    AntibotError, AntibotStrategy, BrowserMode, CrawlConfig, Decision, DispatchProfile, EscalationStrategy, WafSignal,
    create_engine, scrape,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn engine_with(config: CrawlConfig) -> crawlberg::CrawlEngineHandle {
    create_engine(Some(config)).expect("engine build must not fail")
}

/// Records every `pre_request` and `post_response` call.
#[derive(Debug)]
struct RecordingStrategy {
    pre_calls: Arc<AtomicUsize>,
    post_calls: Arc<AtomicUsize>,
    decision: Decision,
}

impl RecordingStrategy {
    fn new(decision: Decision) -> (Self, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let pre = Arc::new(AtomicUsize::new(0));
        let post = Arc::new(AtomicUsize::new(0));
        let strategy = Self {
            pre_calls: Arc::clone(&pre),
            post_calls: Arc::clone(&post),
            decision,
        };
        (strategy, pre, post)
    }
}

#[async_trait]
impl AntibotStrategy for RecordingStrategy {
    async fn pre_request(&self, _url: &str) -> Result<(), AntibotError> {
        self.pre_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn post_response(&self, _response: &HttpResponse, _waf_signal: Option<&WafSignal>) -> Decision {
        self.post_calls.fetch_add(1, Ordering::SeqCst);
        self.decision.clone()
    }
}

#[derive(Debug)]
struct FixedDecisionStrategy {
    decision: Decision,
}

#[async_trait]
impl AntibotStrategy for FixedDecisionStrategy {
    async fn pre_request(&self, _url: &str) -> Result<(), AntibotError> {
        Ok(())
    }

    async fn post_response(&self, _response: &HttpResponse, _waf_signal: Option<&WafSignal>) -> Decision {
        self.decision.clone()
    }
}

/// When `Decision::Accept` is returned the engine passes through to the retry
/// policy and the scrape completes successfully.
#[tokio::test]
async fn decision_accept_scrape_succeeds() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/ok"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>hello</body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let (strategy, pre, post) = RecordingStrategy::new(Decision::Accept);

    let handle = engine_with(CrawlConfig {
        dispatch: Some(DispatchProfile {
            antibot_strategy: Some(Arc::new(strategy)),
            strategy: EscalationStrategy::None,
            ..DispatchProfile::default()
        }),
        browser: crawlberg::BrowserConfig {
            mode: BrowserMode::Never,
            ..Default::default()
        },
        ..CrawlConfig::default()
    });

    let result = scrape(&handle, &format!("{}/ok", mock.uri())).await;
    assert!(result.is_ok(), "expected Ok, got {result:?}");

    assert_eq!(pre.load(Ordering::SeqCst), 1, "pre_request should fire once");
    assert_eq!(post.load(Ordering::SeqCst), 1, "post_response should fire once");
}

/// A strategy that returns `Retry` on the first attempt and `Accept` on
/// subsequent ones. The engine must retry and ultimately succeed.
#[derive(Debug)]
struct RetryOnceThenAccept {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl AntibotStrategy for RetryOnceThenAccept {
    async fn pre_request(&self, _url: &str) -> Result<(), AntibotError> {
        Ok(())
    }

    async fn post_response(&self, _response: &HttpResponse, _waf_signal: Option<&WafSignal>) -> Decision {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            Decision::Retry {
                backoff: Duration::from_millis(1),
            }
        } else {
            Decision::Accept
        }
    }
}

#[tokio::test]
async fn decision_retry_causes_second_attempt() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/retry"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>content</body></html>")
                .append_header("content-type", "text/html"),
        )
        .expect(2)
        .mount(&mock)
        .await;

    let calls = Arc::new(AtomicUsize::new(0));
    let strategy = RetryOnceThenAccept { calls };

    let handle = engine_with(CrawlConfig {
        dispatch: Some(DispatchProfile {
            antibot_strategy: Some(Arc::new(strategy)),
            strategy: EscalationStrategy::None,
            max_total_attempts: 5,
            ..DispatchProfile::default()
        }),
        browser: crawlberg::BrowserConfig {
            mode: BrowserMode::Never,
            ..Default::default()
        },
        ..CrawlConfig::default()
    });

    let result = scrape(&handle, &format!("{}/retry", mock.uri())).await;
    assert!(result.is_ok(), "expected Ok after retry, got {result:?}");
}

/// `RotateProxy` is not yet implemented. The engine should log a warning and
/// fall through to `Accept` semantics — the scrape must still succeed.
#[tokio::test]
async fn decision_rotate_proxy_falls_through_to_accept() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/proxy"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>ok</body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let strategy = FixedDecisionStrategy {
        decision: Decision::RotateProxy,
    };

    let handle = engine_with(CrawlConfig {
        dispatch: Some(DispatchProfile {
            antibot_strategy: Some(Arc::new(strategy)),
            strategy: EscalationStrategy::None,
            ..DispatchProfile::default()
        }),
        browser: crawlberg::BrowserConfig {
            mode: BrowserMode::Never,
            ..Default::default()
        },
        ..CrawlConfig::default()
    });

    let result = scrape(&handle, &format!("{}/proxy", mock.uri())).await;
    assert!(result.is_ok(), "RotateProxy should fall through to Accept: {result:?}");
}

/// `EscalateBrowser` with `BrowserMode::Never` and `EscalationStrategy::None`
/// means there is no browser tier to escalate to. The engine must surface an
/// error rather than returning the response body.
#[tokio::test]
async fn decision_escalate_browser_with_no_browser_returns_error() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/escalate"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>challenge</body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let strategy = FixedDecisionStrategy {
        decision: Decision::EscalateBrowser,
    };

    let handle = engine_with(CrawlConfig {
        dispatch: Some(DispatchProfile {
            antibot_strategy: Some(Arc::new(strategy)),
            strategy: EscalationStrategy::None,
            max_total_attempts: 2,
            ..DispatchProfile::default()
        }),
        browser: crawlberg::BrowserConfig {
            mode: BrowserMode::Never,
            ..Default::default()
        },
        ..CrawlConfig::default()
    });

    let result = scrape(&handle, &format!("{}/escalate", mock.uri())).await;
    assert!(
        result.is_err(),
        "expected Err when escalation impossible, got Ok: {result:?}"
    );
}

/// When `antibot_strategy` is `None`, the engine behaves exactly as before:
/// a clean 200 response is returned as Ok.
#[tokio::test]
async fn no_strategy_passes_through_normally() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/plain"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>plain</body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let handle = engine_with(CrawlConfig::default());

    let result = scrape(&handle, &format!("{}/plain", mock.uri())).await;
    assert!(result.is_ok(), "expected Ok without antibot strategy: {result:?}");
}
