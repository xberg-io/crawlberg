//! Integration tests for the `soft_http_errors` configuration flag.
//!
//! Covers the five scenarios from the task spec:
//! 1. Direct 404 raises when `soft_http_errors` is `false` (default).
//! 2. Direct 404 returns `Ok(ScrapeResult { status_code: 404 })` when enabled.
//! 3. Redirected 404 (302→404) returns `Ok` regardless of the flag.
//! 4. Direct 403 raises when `soft_http_errors` is `false` (default).
//! 5. Direct 403 returns `Ok(ScrapeResult { status_code: 403 })` when enabled.

use std::sync::OnceLock;

use crawlberg::{BrowserMode, CrawlConfig, CrawlError, create_engine, scrape};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

static ALLOW_PRIVATE: OnceLock<()> = OnceLock::new();

/// Opts into the SSRF policy's private-network allowance so wiremock's
/// 127.0.0.1 servers are reachable. Without this, `create_engine`'s SSRF
/// check rejects the loopback URL before the soft-error semantics under
/// test are ever reached.
fn allow_private_network() {
    ALLOW_PRIVATE.get_or_init(|| {
        // ~keep SAFETY: OnceLock writes this env var once before any network call is made.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "1");
        }
    });
}

fn engine_with_config(mut config: CrawlConfig) -> crawlberg::CrawlEngineHandle {
    config.browser.mode = BrowserMode::Never;
    allow_private_network();
    create_engine(Some(config)).expect("engine build must not fail")
}

/// With the default config (`soft_http_errors = false`), a bare 404 response
/// must propagate as `Err(CrawlError::NotFound)`.
#[tokio::test]
async fn direct_404_raises_when_soft_errors_disabled() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/not-found"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&mock)
        .await;

    let handle = engine_with_config(CrawlConfig::default());
    let url = format!("{}/not-found", mock.uri());
    let result = scrape(&handle, &url).await;

    assert!(result.is_err(), "expected Err, got Ok: {result:?}");
    assert!(
        matches!(result.unwrap_err(), CrawlError::NotFound { .. }),
        "expected CrawlError::NotFound"
    );
}

/// With `soft_http_errors = true`, a bare 404 must be returned as
/// `Ok(ScrapeResult { status_code: 404, .. })` rather than an error.
#[tokio::test]
async fn direct_404_returns_result_when_soft_errors_enabled() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/not-found"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&mock)
        .await;

    let handle = engine_with_config(CrawlConfig {
        soft_http_errors: true,
        ..CrawlConfig::default()
    });
    let url = format!("{}/not-found", mock.uri());
    let result = scrape(&handle, &url).await;

    assert!(result.is_ok(), "expected Ok, got Err: {:?}", result.err());
    let page = result.unwrap();
    assert_eq!(page.status_code, 404, "status_code must be 404");
    assert!(page.html.is_empty(), "body must be empty for synthesised 404");
}

/// A 302→404 chain must always surface as `Ok(ScrapeResult { status_code: 404 })`
/// regardless of the `soft_http_errors` setting, because the caller opted into
/// redirect-following.
#[tokio::test]
async fn redirected_404_returns_result_regardless_of_soft_errors() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/start"))
        .respond_with(
            ResponseTemplate::new(302)
                .append_header("location", "/not-found")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/not-found"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&mock)
        .await;

    let handle = engine_with_config(CrawlConfig::default());
    let url = format!("{}/start", mock.uri());
    let result = scrape(&handle, &url).await;

    assert!(
        result.is_ok(),
        "redirected 404 must return Ok regardless of soft_http_errors: {:?}",
        result.err()
    );
    let page = result.unwrap();
    assert_eq!(page.status_code, 404, "status_code must be 404 after redirect chain");
}

/// With the default config (`soft_http_errors = false`), a bare 403 response
/// must propagate as `Err(CrawlError::Forbidden)`.
#[tokio::test]
async fn direct_403_raises_when_soft_errors_disabled() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/forbidden"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&mock)
        .await;

    let handle = engine_with_config(CrawlConfig::default());
    let url = format!("{}/forbidden", mock.uri());
    let result = scrape(&handle, &url).await;

    assert!(result.is_err(), "expected Err, got Ok: {result:?}");
    assert!(
        matches!(result.unwrap_err(), CrawlError::Forbidden { .. }),
        "expected CrawlError::Forbidden"
    );
}

/// With `soft_http_errors = true`, a bare 403 must be returned as
/// `Ok(ScrapeResult { status_code: 403, .. })` rather than an error.
#[tokio::test]
async fn direct_403_returns_result_when_soft_errors_enabled() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/forbidden"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&mock)
        .await;

    let handle = engine_with_config(CrawlConfig {
        soft_http_errors: true,
        ..CrawlConfig::default()
    });
    let url = format!("{}/forbidden", mock.uri());
    let result = scrape(&handle, &url).await;

    assert!(result.is_ok(), "expected Ok, got Err: {:?}", result.err());
    let page = result.unwrap();
    assert_eq!(page.status_code, 403, "status_code must be 403");
    assert!(page.html.is_empty(), "body must be empty for synthesised 403");
}
