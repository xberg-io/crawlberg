//! Integration tests for robots.txt *enforcement over HTTP*.
//!
//! ~keep The robots unit tests in `src/robots.rs` only exercise the parser against string
//! literals, so three defects in the fetch path shipped undetected (#43, #44, #45). Every
//! `MockServer` here binds an ephemeral port, which is exactly the condition that makes the
//! wrong-port robots URL reproduce.

use std::time::Duration;

use crawlberg::{CrawlConfig, crawl, create_engine};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ROBOTS_DISALLOW_PRIVATE: &str = "User-agent: *\nDisallow: /private/\n";
const ROBOTS_DISALLOW_ALL: &str = "User-agent: *\nDisallow: /\n";

/// Mount an HTML page, expecting it to be requested exactly `expected` times.
async fn mount_html_expecting(mock: &MockServer, at: &str, body: &str, expected: u64) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body.to_owned())
                .append_header("content-type", "text/html"),
        )
        .expect(expected)
        .mount(mock)
        .await;
}

/// Mount a `/robots.txt` answering `status`, expecting exactly `expected` requests.
async fn mount_robots(mock: &MockServer, status: u16, body: &str, expected: u64) {
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(status)
                .set_body_string(body.to_owned())
                .append_header("content-type", "text/plain"),
        )
        .expect(expected)
        .mount(mock)
        .await;
}

fn robots_config() -> CrawlConfig {
    CrawlConfig::builder()
        .respect_robots_txt(true)
        .allow_private_networks(true)
        .max_pages(10)
        .request_timeout(Duration::from_secs(5))
        .build()
}

fn crawled_paths(result: &crawlberg::CrawlResult, base: &str) -> Vec<String> {
    result
        .pages
        .iter()
        .map(|page| match page.url.strip_prefix(base).unwrap_or(&page.url) {
            "" => "/".to_owned(),
            rest => rest.to_owned(),
        })
        .collect()
}

/// #43: the robots URL must carry the seed's port, so the seed's own server receives the request.
#[tokio::test]
async fn should_request_robots_txt_on_the_seeds_own_port() {
    let mock = MockServer::start().await;
    mount_robots(&mock, 200, ROBOTS_DISALLOW_PRIVATE, 1).await;
    mount_html_expecting(&mock, "/", "<html><body>root</body></html>", 1).await;

    let engine = create_engine(Some(robots_config())).expect("engine");
    let result = crawl(&engine, &mock.uri()).await.expect("crawl");

    assert_eq!(result.pages.len(), 1, "the seed page should be crawled");
    // ~keep The `.expect(1)` on /robots.txt is the real assertion; it is verified on drop.
    drop(mock);
}

/// #43: a disallow rule served on a non-default port must actually be honoured.
#[tokio::test]
async fn should_honour_disallow_rules_on_a_non_default_port() {
    let mock = MockServer::start().await;
    mount_robots(&mock, 200, ROBOTS_DISALLOW_PRIVATE, 1).await;
    mount_html_expecting(
        &mock,
        "/",
        r#"<html><body><a href="/private/secret.html">x</a><a href="/public.html">y</a></body></html>"#,
        1,
    )
    .await;
    mount_html_expecting(&mock, "/public.html", "<html><body>public</body></html>", 1).await;
    Mock::given(method("GET"))
        .and(path("/private/secret.html"))
        .respond_with(ResponseTemplate::new(200).set_body_string("secret"))
        .expect(0)
        .mount(&mock)
        .await;

    let engine = create_engine(Some(robots_config())).expect("engine");
    let result = crawl(&engine, &mock.uri()).await.expect("crawl");

    let paths = crawled_paths(&result, &mock.uri());
    assert!(
        !paths.iter().any(|p| p.starts_with("/private/")),
        "disallowed path was crawled: {paths:?}"
    );
    assert_eq!(paths.len(), 2, "expected the seed and /public.html, got {paths:?}");
}

/// #44: RFC 9309 2.3.1.4 — an unreachable robots.txt (5xx) means assume complete disallow.
#[tokio::test]
async fn should_crawl_nothing_when_robots_txt_answers_503() {
    let mock = MockServer::start().await;
    mount_robots(&mock, 503, "", 1).await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"<html><body><a href="/private/secret.html">x</a></body></html>"#)
                .append_header("content-type", "text/html"),
        )
        .expect(0)
        .mount(&mock)
        .await;

    let engine = create_engine(Some(robots_config())).expect("engine");
    let result = crawl(&engine, &mock.uri()).await.expect("crawl");

    assert!(result.pages.is_empty(), "a 5xx robots.txt must block the whole site");
    assert!(result.was_skipped, "the fail-closed decision must be reported");
    let error = result.error.expect("error must record why nothing was crawled");
    assert!(error.contains("robots_unreachable"), "unexpected error: {error}");
}

/// #44: a 4xx robots.txt is "unavailable", which permits crawling with no rules.
#[tokio::test]
async fn should_crawl_everything_when_robots_txt_answers_404() {
    let mock = MockServer::start().await;
    mount_robots(&mock, 404, "Not Found", 1).await;
    mount_html_expecting(
        &mock,
        "/",
        r#"<html><body><a href="/private/secret.html">x</a></body></html>"#,
        1,
    )
    .await;
    mount_html_expecting(&mock, "/private/secret.html", "<html><body>secret</body></html>", 1).await;

    let engine = create_engine(Some(robots_config())).expect("engine");
    let result = crawl(&engine, &mock.uri()).await.expect("crawl");

    assert_eq!(result.pages.len(), 2, "404 robots.txt must allow the whole site");
    assert!(result.error.is_none(), "a missing robots.txt is not an error");
}

/// #45: the seed must be fetched once, not once to resolve redirects and again as page 0.
#[tokio::test]
async fn should_fetch_the_seed_exactly_once() {
    let mock = MockServer::start().await;
    let config = CrawlConfig::builder()
        .respect_robots_txt(false)
        .allow_private_networks(true)
        .max_pages(1)
        .build();
    mount_html_expecting(&mock, "/", "<html><body>root</body></html>", 1).await;

    let engine = create_engine(Some(config)).expect("engine");
    let result = crawl(&engine, &mock.uri()).await.expect("crawl");

    assert_eq!(result.pages.len(), 1, "max_pages(1) must yield exactly one page");
}

/// #45: robots.txt must be read before the seed is requested, so a disallowed seed is never fetched.
#[tokio::test]
async fn should_not_request_a_seed_that_robots_disallows() {
    let mock = MockServer::start().await;
    mount_robots(&mock, 200, ROBOTS_DISALLOW_ALL, 1).await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html><body>root</body></html>"))
        .expect(0)
        .mount(&mock)
        .await;

    let engine = create_engine(Some(robots_config())).expect("engine");
    let result = crawl(&engine, &mock.uri()).await.expect("crawl");

    assert!(result.pages.is_empty(), "a disallowed seed must not be crawled");
    assert!(result.was_skipped, "skipping the seed must be reported");
}

/// #44 (same defect class): `scrape` reports robots status rather than enforcing it, so
/// failing closed means reporting `is_allowed = false` when the policy cannot be read.
#[tokio::test]
async fn should_report_is_allowed_false_when_robots_txt_is_unreachable_for_scrape() {
    let mock = MockServer::start().await;
    mount_robots(&mock, 503, "", 1).await;
    mount_html_expecting(&mock, "/page", "<html><body>page</body></html>", 1).await;

    let engine = create_engine(Some(robots_config())).expect("engine");
    let result = crawlberg::scrape(&engine, &format!("{}/page", mock.uri()))
        .await
        .expect("scrape");

    assert!(
        !result.is_allowed,
        "an unreachable robots.txt must be reported as is_allowed=false, not silently true"
    );
}

/// An HTTP error page must never have its body parsed as a robots.txt policy.
#[tokio::test]
async fn should_not_parse_an_http_error_page_body_as_robots_txt() {
    let mock = MockServer::start().await;
    // ~keep 451 is not one of the statuses `http_fetch` maps to an error, so it arrives as
    // an `Ok` response whose body used to be parsed as though it were the site's policy.
    mount_robots(&mock, 451, ROBOTS_DISALLOW_ALL, 1).await;
    mount_html_expecting(&mock, "/page", "<html><body>page</body></html>", 1).await;

    let engine = create_engine(Some(robots_config())).expect("engine");
    let result = crawlberg::scrape(&engine, &format!("{}/page", mock.uri()))
        .await
        .expect("scrape");

    assert!(
        result.is_allowed,
        "451 is a 4xx 'unavailable' status; its error-page body is not a robots.txt policy"
    );
}
