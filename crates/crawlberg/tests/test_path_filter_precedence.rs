//! Integration tests for `include_paths` / `exclude_paths` precedence in the crawl loop.
//!
//! ~keep Precedence is derived from `CrawlEngine::crawl` (engine/mod.rs) and the
//! sequential loop in `engine/crawl_loop.rs`: for each discovered URL, the exclude
//! check runs first and rejects immediately on a match; only URLs that survive it are
//! then subjected to the include check. So when a path matches BOTH an include and an
//! exclude pattern, exclude wins. The seed URL (depth 0) is exempt from the include
//! check entirely — only `entry.depth > 0` URLs are required to match an include
//! pattern.

use crawlberg::{CrawlConfig, crawl, create_engine};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// When a discovered URL's path matches both an include and an exclude pattern,
/// the exclude pattern wins and the URL is filtered out.
#[tokio::test]
async fn should_exclude_url_that_matches_both_include_and_exclude_patterns() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"<html><body><a href="/blog/post-1">Post</a></body></html>"#)
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/blog/post-1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>Post 1</body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let base = CrawlConfig::builder().allow_private_networks(true).build();
    let config = CrawlConfig {
        max_depth: Some(1),
        include_paths: vec!["^/blog".to_owned()],
        exclude_paths: vec!["^/blog".to_owned()],
        ..base
    };
    let handle = create_engine(Some(config)).unwrap();

    let result = crawl(&handle, &mock.uri()).await.unwrap();

    let urls: Vec<&str> = result.pages.iter().map(|p| p.url.as_str()).collect();
    assert_eq!(
        result.pages.len(),
        1,
        "exclude must win over include for a path matching both patterns; expected only the \
         seed page, got: {urls:?}"
    );
    assert!(
        !urls.iter().any(|u| u.contains("/blog/post-1")),
        "/blog/post-1 matches both include and exclude and must be filtered out by exclude, \
         but pages were: {urls:?}"
    );
}

/// The seed URL (depth 0) is never subjected to the include-path check, only
/// discovered child links (depth > 0) are.
#[tokio::test]
async fn should_not_apply_include_paths_to_the_seed_url() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"<html><body><a href="/blog/post-1">Post</a></body></html>"#)
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/blog/post-1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>Post 1</body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let base = CrawlConfig::builder().allow_private_networks(true).build();
    let config = CrawlConfig {
        max_depth: Some(1),
        include_paths: vec!["^/blog".to_owned()],
        ..base
    };
    let handle = create_engine(Some(config)).unwrap();

    let result = crawl(&handle, &mock.uri()).await.unwrap();

    let urls: Vec<&str> = result.pages.iter().map(|p| p.url.as_str()).collect();
    assert_eq!(
        result.pages.len(),
        2,
        "seed (\"/\") does not match include_paths but must still be crawled, and the child \
         /blog/post-1 matches include_paths and must also be crawled; got: {urls:?}"
    );
}

/// A path that matches only `exclude_paths` (and no `include_paths` is configured) is
/// filtered out, while unrelated paths pass through unfiltered.
#[tokio::test]
async fn should_filter_url_matching_exclude_paths_when_no_include_paths_configured() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(
                    r#"<html><body><a href="/admin/panel">Admin</a><a href="/public">Public</a></body></html>"#,
                )
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/public"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>Public</body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let base = CrawlConfig::builder().allow_private_networks(true).build();
    let config = CrawlConfig {
        max_depth: Some(1),
        exclude_paths: vec!["^/admin".to_owned()],
        ..base
    };
    let handle = create_engine(Some(config)).unwrap();

    let result = crawl(&handle, &mock.uri()).await.unwrap();

    let urls: Vec<&str> = result.pages.iter().map(|p| p.url.as_str()).collect();
    assert_eq!(
        result.pages.len(),
        2,
        "seed and /public must be crawled while /admin/panel is excluded, got: {urls:?}"
    );
    assert!(
        !urls.iter().any(|u| u.contains("/admin/panel")),
        "/admin/panel matches exclude_paths and must not appear in results: {urls:?}"
    );
}
