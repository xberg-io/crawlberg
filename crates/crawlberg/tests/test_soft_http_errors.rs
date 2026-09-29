//! Integration tests for the `soft_http_errors` configuration flag.
//!
//! Covers the five scenarios from the task spec:
//! 1. Direct 404 raises when `soft_http_errors` is `false` (default).
//! 2. Direct 404 returns `Ok(ScrapeResult { status_code: 404 })` when enabled.
//! 3. Redirected 404 (302→404) returns `Ok` regardless of the flag.
//! 4. Direct 403 raises when `soft_http_errors` is `false` (default).
//! 5. Direct 403 returns `Ok(ScrapeResult { status_code: 403 })` when enabled.
//! 6. A WAF block reports the status of the refused response: 429, 503 or 403, and 403 for a
//!    2xx refused as a block page.
//! 7. Every soft error page has the same shape, whatever its status: no body, no markdown and no
//!    response metadata.
//! 8. `soft_http_errors` changes only the responses it turns into soft error pages: an empty 400,
//!    418 or 501 comes back as the same full page with the flag on and off.

use crawlberg::{BrowserMode, CrawlConfig, CrawlError, CrawlEvent, ScrapeResult, crawl_stream, create_engine, scrape};
use futures::StreamExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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

fn engine_with_config(mut config: CrawlConfig) -> crawlberg::CrawlEngineHandle {
    config.browser.mode = BrowserMode::Never;
    create_engine(Some(config)).expect("engine build must not fail")
}

/// Asserts `page` has the shape of a soft error page: no body, no markdown, no response metadata.
fn assert_soft_page_shape(page: &ScrapeResult) {
    let status = page.status_code;
    assert!(page.html.is_empty(), "{status}: a soft error page has no body");
    assert!(page.markdown.is_none(), "{status}: a soft error page has no markdown");
    assert!(
        page.response_meta.is_none(),
        "{status}: a soft error page has no response metadata"
    );
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

    let handle = engine_with_config(allow_private_config());
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
        ..allow_private_config()
    });
    let url = format!("{}/not-found", mock.uri());
    let result = scrape(&handle, &url).await;

    assert!(result.is_ok(), "expected Ok, got Err: {:?}", result.err());
    let page = result.unwrap();
    assert_eq!(page.status_code, 404, "status_code must be 404");
    assert_soft_page_shape(&page);
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

    let handle = engine_with_config(allow_private_config());
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

    let handle = engine_with_config(allow_private_config());
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
        ..allow_private_config()
    });
    let url = format!("{}/forbidden", mock.uri());
    let result = scrape(&handle, &url).await;

    assert!(result.is_ok(), "expected Ok, got Err: {:?}", result.err());
    let page = result.unwrap();
    assert_eq!(page.status_code, 403, "status_code must be 403");
    assert_soft_page_shape(&page);
}

/// Scrapes `route` served as `response` with `soft_http_errors` on, checks the page has the soft
/// error page shape, and returns the page's status.
async fn soft_status_of(route: &str, response: ResponseTemplate) -> u16 {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(response)
        .mount(&mock)
        .await;

    let handle = engine_with_config(CrawlConfig {
        soft_http_errors: true,
        ..allow_private_config()
    });
    let page = scrape(&handle, &format!("{}{route}", mock.uri()))
        .await
        .unwrap_or_else(|err| panic!("{route}: expected a soft error page, got Err: {err:?}"));
    assert_soft_page_shape(&page);
    page.status_code
}

/// A 429 WAF block keeps its 429, so a caller can tell a rate limit from a forbidden response.
#[tokio::test]
async fn waf_block_on_429_reports_429_when_soft_errors_enabled() {
    let status = soft_status_of(
        "/blocked-429",
        ResponseTemplate::new(429)
            .append_header("x-px-block", "1")
            .set_body_string("<html>px-captcha</html>"),
    )
    .await;
    assert_eq!(status, 429, "a 429 WAF block must report 429");
}

/// A 503 WAF block keeps its 503.
#[tokio::test]
async fn waf_block_on_503_reports_503_when_soft_errors_enabled() {
    let status = soft_status_of(
        "/blocked-503",
        ResponseTemplate::new(503)
            .append_header("x-datadome", "blocked")
            .set_body_string("<html>challenge</html>"),
    )
    .await;
    assert_eq!(status, 503, "a 503 WAF block must report 503");
}

/// A 2xx refused as a block page reports 403: a 2xx soft error would read as success.
#[tokio::test]
async fn waf_block_on_2xx_reports_403_when_soft_errors_enabled() {
    let status = soft_status_of(
        "/blocked-202",
        ResponseTemplate::new(202)
            .insert_header("content-type", "text/html")
            .set_body_string("<html>cf-chl- x</html>"),
    )
    .await;
    assert_eq!(status, 403, "a 2xx refused as a WAF block must report 403");
}

/// An empty error response the fetch does not refuse is not a soft error page, so the flag must
/// not change it: with the flag on and off it comes back as the same full page.
#[tokio::test]
async fn empty_error_response_the_fetch_accepts_is_the_same_page_whatever_the_flag() {
    for status in [400_u16, 418, 501] {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/empty"))
            .respond_with(ResponseTemplate::new(status))
            .mount(&mock)
            .await;
        let url = format!("{}/empty", mock.uri());

        let mut pages = Vec::new();
        for soft_http_errors in [false, true] {
            let handle = engine_with_config(CrawlConfig {
                soft_http_errors,
                ..allow_private_config()
            });
            let page = scrape(&handle, &url)
                .await
                .unwrap_or_else(|err| panic!("{status} soft={soft_http_errors}: expected a page, got Err: {err:?}"));
            assert_eq!(page.status_code, status, "{status} soft={soft_http_errors}: status");
            assert!(
                page.markdown.is_some(),
                "{status} soft={soft_http_errors}: the page must keep its markdown"
            );
            assert!(
                page.response_meta.is_some(),
                "{status} soft={soft_http_errors}: the page must keep its response metadata"
            );
            pages.push(page);
        }
        let markdown = |page: &ScrapeResult| serde_json::to_string(&page.markdown).expect("markdown serializes");
        assert_eq!(
            markdown(&pages[0]),
            markdown(&pages[1]),
            "{status}: the flag must not change the markdown"
        );
    }
}

/// A 403 WAF block reports 403.
#[tokio::test]
async fn waf_block_on_403_reports_403_when_soft_errors_enabled() {
    let blocked = || {
        ResponseTemplate::new(403)
            .insert_header("content-type", "text/html")
            .set_body_string("<html><title>Attention Required! | Cloudflare</title>cf-chl- x</html>")
    };

    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/blocked-403"))
        .respond_with(blocked())
        .mount(&mock)
        .await;
    let handle = engine_with_config(allow_private_config());
    let refused = scrape(&handle, &format!("{}/blocked-403", mock.uri())).await;
    assert!(
        matches!(refused, Err(CrawlError::WafBlocked { .. })),
        "the fixture must be refused as a WAF block, got {refused:?}"
    );

    let status = soft_status_of("/blocked-403", blocked()).await;
    assert_eq!(status, 403, "a 403 WAF block must report 403");
}

/// A crawl reports a 429 WAF block on a linked page with its 429 status. A 503 WAF block is a
/// server error, which a crawl reports as an error event rather than as a page.
#[tokio::test]
async fn crawl_reports_a_waf_block_with_its_status_when_soft_errors_enabled() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/html")
                .set_body_string(
                    r#"<html><body><a href="/blocked-429">a</a><a href="/blocked-503">b</a></body></html>"#,
                ),
        )
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/blocked-429"))
        .respond_with(
            ResponseTemplate::new(429)
                .append_header("x-px-block", "1")
                .set_body_string("<html>px-captcha</html>"),
        )
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/blocked-503"))
        .respond_with(
            ResponseTemplate::new(503)
                .append_header("x-datadome", "blocked")
                .set_body_string("<html>challenge</html>"),
        )
        .mount(&mock)
        .await;

    let handle = engine_with_config(CrawlConfig {
        soft_http_errors: true,
        ..allow_private_config()
    });
    let mut pages = Vec::new();
    let mut errors = Vec::new();
    let mut pages_crawled = None;
    let mut stream = crawl_stream(&handle, &format!("{}/", mock.uri()))
        .await
        .expect("crawl must start");
    while let Some(event) = stream.next().await {
        match event.expect("a crawl event must not be a transport error") {
            CrawlEvent::Page { result } => pages.push((result.url.clone(), result.status_code)),
            CrawlEvent::Error { url, error } => errors.push((url, error)),
            CrawlEvent::Complete { pages_crawled: count } => pages_crawled = Some(count),
        }
    }

    let blocked = pages
        .iter()
        .find(|(url, _)| url.ends_with("/blocked-429"))
        .unwrap_or_else(|| panic!("the 429 block must be a page, got {pages:?}"));
    assert_eq!(blocked.1, 429, "a 429 WAF block must report 429");
    assert!(
        !pages.iter().any(|(url, _)| url.ends_with("/blocked-503")),
        "a 503 WAF block must not be a page, got {pages:?}"
    );
    assert_eq!(
        errors
            .iter()
            .filter(|(url, _)| url.ends_with("/blocked-503"))
            .map(|(_, error)| error.as_str())
            .collect::<Vec<_>>(),
        vec!["server_error: HTTP 503"],
        "a 503 WAF block must be one error event, got {errors:?}"
    );
    assert_eq!(
        pages_crawled,
        Some(2),
        "the crawl counts the seed and the 429 block, not the 503 block, got pages {pages:?}"
    );
}
