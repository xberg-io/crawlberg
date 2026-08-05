//! Integration tests for Bug 1 — escalation with no browser target.
//!
//! When `BrowserMode::Never` is set (or `escalation_strategy = None`), the
//! engine must surface HTTP 403 / WAF-block responses as `Err(CrawlError::...)`
//! rather than silently returning `Ok`.
//!
//! Tests are written RED-first: they prove the bug exists before the fix lands.

use std::sync::OnceLock;

use crawlberg::{BrowserMode, CrawlConfig, CrawlError, DispatchProfile, EscalationStrategy, create_engine, scrape};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

static ALLOW_PRIVATE: OnceLock<()> = OnceLock::new();

/// Opts into the SSRF policy's private-network allowance so wiremock's
/// 127.0.0.1 servers are reachable. Without this, `create_engine`'s SSRF
/// check rejects the loopback URL before the Forbidden/WafBlocked logic
/// under test is ever reached.
fn allow_private_network() {
    ALLOW_PRIVATE.get_or_init(|| {
        // ~keep SAFETY: OnceLock writes this env var once before any network call is made.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "1");
        }
    });
}

fn engine_with(config: CrawlConfig) -> crawlberg::CrawlEngineHandle {
    allow_private_network();
    create_engine(Some(config)).expect("engine build must not fail")
}

/// `BrowserOnly` strategy with `BrowserMode::Never`: the policy wants to escalate
/// to Browser, but no browser is available. The engine must surface the 403 as
/// `Err(CrawlError::Forbidden)` rather than returning `Ok` or using browser.
#[tokio::test]
async fn browser_only_strategy_browser_never_returns_forbidden_error() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/forbidden"))
        .respond_with(ResponseTemplate::new(403).set_body_string("nope"))
        .mount(&mock)
        .await;

    let handle = engine_with(CrawlConfig {
        dispatch: Some(DispatchProfile {
            strategy: EscalationStrategy::BrowserOnly,
            ..DispatchProfile::default()
        }),
        browser: crawlberg::BrowserConfig {
            mode: BrowserMode::Never,
            ..Default::default()
        },
        ..CrawlConfig::default()
    });

    let url = format!("{}/forbidden", mock.uri());
    let result = scrape(&handle, &url).await;

    assert!(result.is_err(), "expected Err, got Ok: {result:?}");
    let err = result.unwrap_err();
    assert!(
        matches!(err, CrawlError::Forbidden(_) | CrawlError::WafBlocked { .. }),
        "expected Forbidden or WafBlocked, got: {err:?}"
    );
}

/// Same constraint with a Cloudflare-fingerprinted 403. Engine must return
/// `Err(CrawlError::WafBlocked { .. })` rather than escalating to browser.
#[tokio::test]
async fn browser_only_strategy_browser_never_returns_waf_blocked_error() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/cf"))
        .respond_with(
            ResponseTemplate::new(403)
                .insert_header("server", "cloudflare")
                .set_body_string("<html><head><title>Just a moment...</title></head></html>"),
        )
        .mount(&mock)
        .await;

    let handle = engine_with(CrawlConfig {
        dispatch: Some(DispatchProfile {
            strategy: EscalationStrategy::BrowserOnly,
            ..DispatchProfile::default()
        }),
        browser: crawlberg::BrowserConfig {
            mode: BrowserMode::Never,
            ..Default::default()
        },
        ..CrawlConfig::default()
    });

    let url = format!("{}/cf", mock.uri());
    let result = scrape(&handle, &url).await;

    assert!(result.is_err(), "expected Err, got Ok: {result:?}");
    let err = result.unwrap_err();
    assert!(
        matches!(err, CrawlError::Forbidden(_) | CrawlError::WafBlocked { .. }),
        "expected Forbidden or WafBlocked, got: {err:?}"
    );
}

/// `None` strategy must surface errors unconditionally — even without `BrowserMode::Never`.
#[tokio::test]
async fn none_strategy_browser_auto_returns_forbidden_error() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/forbidden"))
        .respond_with(ResponseTemplate::new(403).set_body_string("access denied"))
        .mount(&mock)
        .await;

    let handle = engine_with(CrawlConfig {
        dispatch: Some(DispatchProfile {
            strategy: EscalationStrategy::None,
            ..DispatchProfile::default()
        }),
        browser: crawlberg::BrowserConfig {
            mode: BrowserMode::Never,
            ..Default::default()
        },
        ..CrawlConfig::default()
    });

    let url = format!("{}/forbidden", mock.uri());
    let result = scrape(&handle, &url).await;

    assert!(result.is_err(), "expected Err, got Ok: {result:?}");
    assert!(
        matches!(
            result.unwrap_err(),
            CrawlError::Forbidden(_) | CrawlError::WafBlocked { .. }
        ),
        "expected Forbidden or WafBlocked"
    );
}

/// None strategy + WAF-fingerprinted 403 must return the original WAF error.
#[tokio::test]
async fn none_strategy_returns_waf_blocked_not_ok() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/akamai"))
        .respond_with(
            ResponseTemplate::new(403)
                .insert_header("server", "AkamaiGHost")
                .set_body_string("<html><body>Access Denied</body></html>"),
        )
        .mount(&mock)
        .await;

    let handle = engine_with(CrawlConfig {
        dispatch: Some(DispatchProfile {
            strategy: EscalationStrategy::None,
            ..DispatchProfile::default()
        }),
        browser: crawlberg::BrowserConfig {
            mode: BrowserMode::Never,
            ..Default::default()
        },
        ..CrawlConfig::default()
    });

    let url = format!("{}/akamai", mock.uri());
    let result = scrape(&handle, &url).await;

    assert!(result.is_err(), "expected Err, got Ok: {result:?}");
    let err = result.unwrap_err();
    assert!(
        matches!(err, CrawlError::Forbidden(_) | CrawlError::WafBlocked { .. }),
        "expected Forbidden or WafBlocked, got: {err:?}"
    );
}
