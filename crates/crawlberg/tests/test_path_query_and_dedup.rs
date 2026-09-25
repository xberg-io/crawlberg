//! Integration tests for crawlberg#61 (path patterns matching the query string) and
//! crawlberg#65 (keeping the query in the dedup key, and stripping tracking parameters).

use crawlberg::{CrawlConfig, CrawlResult, crawl, create_engine};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn mount_html(mock: &MockServer, at: &str, body: &str) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body.to_owned())
                .append_header("content-type", "text/html"),
        )
        .mount(mock)
        .await;
}

async fn mount_redirect(mock: &MockServer, at: &str, to: &str) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(ResponseTemplate::new(302).append_header("location", to.to_owned()))
        .mount(mock)
        .await;
}

/// Every path the server was asked for, in arrival order.
async fn request_log(mock: &MockServer) -> Vec<String> {
    mock.received_requests()
        .await
        .expect("the mock server records its requests")
        .iter()
        .map(|request| request.url.path().to_owned())
        .collect()
}

async fn crawl_seed(config: CrawlConfig, seed: &str) -> CrawlResult {
    let engine = create_engine(Some(config)).expect("engine builds");
    crawl(&engine, seed).await.expect("crawl runs")
}

fn base_config() -> crawlberg::CrawlConfigBuilder {
    CrawlConfig::builder().allow_private_networks(true).max_pages(10)
}

// ---------------------------------------------------------------------------------------
// #61: exclude_paths / include_paths matching the query string
// ---------------------------------------------------------------------------------------

/// Characterizes today's behaviour: `exclude_paths` matches `path()` alone, so a pattern
/// that only matches the query string never excludes anything.
#[tokio::test]
async fn exclude_paths_ignores_the_query_string_by_default() {
    let mock = MockServer::start().await;
    mount_html(&mock, "/", r#"<html><body><a href="/blog?p=42">Post</a></body></html>"#).await;
    mount_html(&mock, "/blog", "<html><body>post</body></html>").await;

    let config = base_config()
        .max_depth(1)
        .exclude_paths(vec![r"\?p=\d+".to_owned()])
        .build();
    let result = crawl_seed(config, &format!("{}/", mock.uri())).await;

    let urls: Vec<&str> = result.pages.iter().map(|p| p.url.as_str()).collect();
    assert_eq!(
        result.pages.len(),
        2,
        "path-only matching must not see the query string, so /blog?p=42 must still be fetched, got: {urls:?}"
    );
}

/// With `path_patterns_match_query` enabled, `exclude_paths` now sees `path?query` and
/// excludes the reporter's case.
#[tokio::test]
async fn exclude_paths_matches_the_query_string_when_match_query_is_enabled() {
    let mock = MockServer::start().await;
    mount_html(&mock, "/", r#"<html><body><a href="/blog?p=42">Post</a></body></html>"#).await;
    mount_html(&mock, "/blog", "<html><body>post</body></html>").await;

    let config = base_config()
        .max_depth(1)
        .exclude_paths(vec![r"\?p=\d+".to_owned()])
        .path_patterns_match_query(true)
        .build();
    let result = crawl_seed(config, &format!("{}/", mock.uri())).await;

    let urls: Vec<&str> = result.pages.iter().map(|p| p.url.as_str()).collect();
    assert_eq!(
        result.pages.len(),
        1,
        "with path_patterns_match_query on, /blog?p=42 must be excluded, got: {urls:?}"
    );
}

/// `include_paths` matching only the query string never admits anything by default.
#[tokio::test]
async fn include_paths_ignores_the_query_string_by_default() {
    let mock = MockServer::start().await;
    mount_html(&mock, "/", r#"<html><body><a href="/blog?p=42">Post</a></body></html>"#).await;
    mount_html(&mock, "/blog", "<html><body>post</body></html>").await;

    let config = base_config()
        .max_depth(1)
        .include_paths(vec![r"\?p=\d+".to_owned()])
        .build();
    let result = crawl_seed(config, &format!("{}/", mock.uri())).await;

    let urls: Vec<&str> = result.pages.iter().map(|p| p.url.as_str()).collect();
    assert_eq!(
        result.pages.len(),
        1,
        "path-only matching must not see the query string, so /blog?p=42 must be filtered out, got: {urls:?}"
    );
}

/// With `path_patterns_match_query` enabled, `include_paths` now admits the reporter's case.
#[tokio::test]
async fn include_paths_matches_the_query_string_when_match_query_is_enabled() {
    let mock = MockServer::start().await;
    mount_html(&mock, "/", r#"<html><body><a href="/blog?p=42">Post</a></body></html>"#).await;
    mount_html(&mock, "/blog", "<html><body>post</body></html>").await;

    let config = base_config()
        .max_depth(1)
        .include_paths(vec![r"\?p=\d+".to_owned()])
        .path_patterns_match_query(true)
        .build();
    let result = crawl_seed(config, &format!("{}/", mock.uri())).await;

    let urls: Vec<&str> = result.pages.iter().map(|p| p.url.as_str()).collect();
    assert_eq!(
        result.pages.len(),
        2,
        "with path_patterns_match_query on, /blog?p=42 must be admitted, got: {urls:?}"
    );
}

// ---------------------------------------------------------------------------------------
// #61: the redirect-hop asymmetry (exclude applied to every hop, include never applied)
// ---------------------------------------------------------------------------------------

/// A redirect target that fails `include_paths` must now be refused, exactly like the
/// existing exclude refusal (no error, `state.error` untouched) rather than requested.
///
/// ~keep Decision: `include_paths` applies to a genuine redirect target (a hop reached only
/// after at least one redirect), never to the chain's own starting URL -- the starting URL
/// was already vetted (or deliberately exempted, at depth 0) by whichever check enqueued it,
/// before this policy ever saw it. See `RedirectPolicy::include_regexes`.
#[tokio::test]
async fn redirect_target_failing_include_paths_is_never_requested() {
    let mock = MockServer::start().await;
    mount_redirect(&mock, "/", "/other.html").await;
    mount_html(&mock, "/other.html", "<html><body>other</body></html>").await;

    let config = base_config().include_paths(vec!["^/docs".to_owned()]).build();
    let result = crawl_seed(config, &format!("{}/", mock.uri())).await;

    let log = request_log(&mock).await;
    assert!(
        !log.iter().any(|entry| entry == "/other.html"),
        "a redirect target failing include_paths must never be requested, got {log:?}"
    );
    assert_eq!(result.pages.len(), 0, "a filtered redirect target yields no page");
    assert!(
        result.error.is_none(),
        "a filtered redirect target must not be reported as an error, got {:?}",
        result.error
    );
}

/// A redirect target that satisfies `include_paths` is requested normally.
#[tokio::test]
async fn redirect_target_satisfying_include_paths_is_requested() {
    let mock = MockServer::start().await;
    mount_redirect(&mock, "/", "/docs/page.html").await;
    mount_html(&mock, "/docs/page.html", "<html><body>docs</body></html>").await;

    let config = base_config().include_paths(vec!["^/docs".to_owned()]).build();
    let result = crawl_seed(config, &format!("{}/", mock.uri())).await;

    let log = request_log(&mock).await;
    assert!(
        log.iter().any(|entry| entry == "/docs/page.html"),
        "a redirect target matching include_paths must be requested, got {log:?}"
    );
    assert_eq!(result.pages.len(), 1, "the redirect target is the crawl's one page");
}

// ---------------------------------------------------------------------------------------
// #65: dedup key including the query string
// ---------------------------------------------------------------------------------------

/// Characterizes today's behaviour: `/item?id=1` and `/item?id=2` collapse to one dedup
/// key, so only the first is ever fetched.
#[tokio::test]
async fn distinct_queries_collapse_to_one_page_by_default() {
    let mock = MockServer::start().await;
    mount_html(
        &mock,
        "/",
        r#"<html><body><a href="/item?id=1">1</a><a href="/item?id=2">2</a></body></html>"#,
    )
    .await;
    mount_html(&mock, "/item", "<html><body>item</body></html>").await;

    let config = base_config().max_depth(1).build();
    let result = crawl_seed(config, &format!("{}/", mock.uri())).await;

    assert_eq!(
        result.pages.len(),
        2,
        "/item?id=1 and /item?id=2 must collapse to a single dedup key by default, got: {:?}",
        result.pages.iter().map(|p| &p.url).collect::<Vec<_>>()
    );
}

/// With `dedup_include_query` enabled, `/item?id=1` and `/item?id=2` are both fetched.
#[tokio::test]
async fn distinct_queries_are_both_fetched_when_dedup_include_query_is_enabled() {
    let mock = MockServer::start().await;
    mount_html(
        &mock,
        "/",
        r#"<html><body><a href="/item?id=1">1</a><a href="/item?id=2">2</a></body></html>"#,
    )
    .await;
    mount_html(&mock, "/item", "<html><body>item</body></html>").await;

    let config = base_config().max_depth(1).dedup_include_query(true).build();
    let result = crawl_seed(config, &format!("{}/", mock.uri())).await;

    assert_eq!(
        result.pages.len(),
        3,
        "/item?id=1 and /item?id=2 must both be fetched as distinct pages, got: {:?}",
        result.pages.iter().map(|p| &p.url).collect::<Vec<_>>()
    );
}

/// Query-parameter order must not create two dedup keys for what is otherwise the same URL.
#[tokio::test]
async fn query_parameter_order_does_not_create_two_dedup_keys() {
    let mock = MockServer::start().await;
    mount_html(
        &mock,
        "/",
        r#"<html><body><a href="/item?a=1&b=2">first</a><a href="/item?b=2&a=1">second</a></body></html>"#,
    )
    .await;
    mount_html(&mock, "/item", "<html><body>item</body></html>").await;

    let config = base_config().max_depth(1).dedup_include_query(true).build();
    let result = crawl_seed(config, &format!("{}/", mock.uri())).await;

    assert_eq!(
        result.pages.len(),
        2,
        "?a=1&b=2 and ?b=2&a=1 must be treated as one dedup key, got: {:?}",
        result.pages.iter().map(|p| &p.url).collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------------------
// #65: tracking-parameter stripping
// ---------------------------------------------------------------------------------------

/// Tracking parameters are stripped from the URL that is fetched and reported, not just
/// from the dedup key.
#[tokio::test]
async fn tracking_parameters_are_stripped_from_fetched_and_reported_url() {
    let mock = MockServer::start().await;
    mount_html(
        &mock,
        "/",
        r#"<html><body><a href="/promo?utm_source=newsletter">Promo</a></body></html>"#,
    )
    .await;
    mount_html(&mock, "/promo", "<html><body>promo</body></html>").await;

    let config = base_config().max_depth(1).strip_tracking_params(true).build();
    let result = crawl_seed(config, &format!("{}/", mock.uri())).await;

    let log = request_log(&mock).await;
    assert!(
        log.iter().any(|entry| entry == "/promo"),
        "the fetched request must carry no tracking parameter, got {log:?}"
    );
    assert!(
        !log.iter().any(|entry| entry.contains("utm_source")),
        "utm_source must not survive into the fetched request path, got {log:?}"
    );

    let urls: Vec<&str> = result.pages.iter().map(|p| p.url.as_str()).collect();
    assert!(
        urls.iter().any(|u| u.ends_with("/promo")),
        "/promo?utm_source=newsletter must be reported as /promo, got: {urls:?}"
    );
    assert!(
        !urls.iter().any(|u| u.contains("utm_source")),
        "utm_source must not survive into the reported URL, got: {urls:?}"
    );
}

/// `strip_tracking_params` off (the default) leaves tracking parameters untouched.
#[tokio::test]
async fn tracking_parameters_are_kept_by_default() {
    let mock = MockServer::start().await;
    mount_html(
        &mock,
        "/",
        r#"<html><body><a href="/promo?utm_source=newsletter">Promo</a></body></html>"#,
    )
    .await;
    mount_html(&mock, "/promo", "<html><body>promo</body></html>").await;

    let config = base_config().max_depth(1).build();
    let result = crawl_seed(config, &format!("{}/", mock.uri())).await;

    let urls: Vec<&str> = result.pages.iter().map(|p| p.url.as_str()).collect();
    assert!(
        urls.iter().any(|u| u.contains("utm_source=newsletter")),
        "the default must not strip utm_source, got: {urls:?}"
    );
}

// ---------------------------------------------------------------------------------------
// Plumbing: each new config field actually reaches the code that consumes it
// ---------------------------------------------------------------------------------------

/// `path_patterns_match_query` reaches `helpers::passes_path_patterns` via `should_fetch_url`.
#[tokio::test]
async fn path_patterns_match_query_field_reaches_should_fetch_url() {
    let mock = MockServer::start().await;
    mount_html(&mock, "/", r#"<html><body><a href="/x?y=1">x</a></body></html>"#).await;
    mount_html(&mock, "/x", "<html><body>x</body></html>").await;

    let off = base_config()
        .max_depth(1)
        .exclude_paths(vec![r"\?y=1".to_owned()])
        .build();
    let with_off = crawl_seed(off, &format!("{}/", mock.uri())).await;
    assert_eq!(
        with_off.pages.len(),
        2,
        "the field defaulting to false must leave path-only matching"
    );

    let mock_on = MockServer::start().await;
    mount_html(&mock_on, "/", r#"<html><body><a href="/x?y=1">x</a></body></html>"#).await;
    mount_html(&mock_on, "/x", "<html><body>x</body></html>").await;
    let on = base_config()
        .max_depth(1)
        .exclude_paths(vec![r"\?y=1".to_owned()])
        .path_patterns_match_query(true)
        .build();
    let with_on = crawl_seed(on, &format!("{}/", mock_on.uri())).await;
    assert_eq!(
        with_on.pages.len(),
        1,
        "setting the field to true must flip the observed match behaviour"
    );
}

/// `dedup_include_query` reaches `normalize::normalize_url_for_dedup` via the frontier.
#[tokio::test]
async fn dedup_include_query_field_reaches_the_frontier_dedup_key() {
    let mock = MockServer::start().await;
    mount_html(
        &mock,
        "/",
        r#"<html><body><a href="/item?id=1">1</a><a href="/item?id=2">2</a></body></html>"#,
    )
    .await;
    mount_html(&mock, "/item", "<html><body>item</body></html>").await;

    let off = base_config().max_depth(1).build();
    let with_off = crawl_seed(off, &format!("{}/", mock.uri())).await;
    assert_eq!(
        with_off.pages.len(),
        2,
        "the field defaulting to false must collapse the queries"
    );

    let mock_on = MockServer::start().await;
    mount_html(
        &mock_on,
        "/",
        r#"<html><body><a href="/item?id=1">1</a><a href="/item?id=2">2</a></body></html>"#,
    )
    .await;
    mount_html(&mock_on, "/item", "<html><body>item</body></html>").await;
    let on = base_config().max_depth(1).dedup_include_query(true).build();
    let with_on = crawl_seed(on, &format!("{}/", mock_on.uri())).await;
    assert_eq!(
        with_on.pages.len(),
        3,
        "setting the field to true must flip the observed dedup behaviour"
    );
}

/// `strip_tracking_params` and `tracking_params` reach `normalize::strip_tracking_params`
/// via link discovery.
#[tokio::test]
async fn strip_tracking_params_and_tracking_params_fields_reach_link_discovery() {
    let mock = MockServer::start().await;
    mount_html(
        &mock,
        "/",
        r#"<html><body><a href="/promo?custom_tag=1">Promo</a></body></html>"#,
    )
    .await;
    mount_html(&mock, "/promo", "<html><body>promo</body></html>").await;

    let off = base_config().max_depth(1).strip_tracking_params(true).build();
    let with_off = crawl_seed(off, &format!("{}/", mock.uri())).await;
    let urls_off: Vec<&str> = with_off.pages.iter().map(|p| p.url.as_str()).collect();
    assert!(
        urls_off.iter().any(|u| u.contains("custom_tag")),
        "the default tracking_params list must not touch an unrelated param, got: {urls_off:?}"
    );

    let mock_on = MockServer::start().await;
    mount_html(
        &mock_on,
        "/",
        r#"<html><body><a href="/promo?custom_tag=1">Promo</a></body></html>"#,
    )
    .await;
    mount_html(&mock_on, "/promo", "<html><body>promo</body></html>").await;
    let on = base_config()
        .max_depth(1)
        .strip_tracking_params(true)
        .tracking_params(vec!["custom_tag".to_owned()])
        .build();
    let with_on = crawl_seed(on, &format!("{}/", mock_on.uri())).await;
    let urls_on: Vec<&str> = with_on.pages.iter().map(|p| p.url.as_str()).collect();
    assert!(
        !urls_on.iter().any(|u| u.contains("custom_tag")),
        "setting tracking_params to include custom_tag must strip it, got: {urls_on:?}"
    );
}
