//! Integration tests for frontier deduplication: verifying duplicate URLs are not re-fetched.

use crawlberg::{CrawlConfig, crawl, create_engine};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Regression test for the max_depth=1 crawl returning only 1 page instead of 3.
///
/// Root at `/` links to `/page1` and `/page2` (depth=1). With max_depth=1 the
/// engine must fetch the root AND both children, yielding 3 pages total.
/// Previously, children discovered at depth=0 were silently dropped when
/// SSRF validation rejected their loopback addresses.
#[tokio::test]
async fn test_max_depth_one_returns_root_plus_children() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(
                    r#"<html><body><h1>Home</h1><a href="/page1">P1</a><a href="/page2">P2</a></body></html>"#,
                )
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/page1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body><h1>Page 1</h1></body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/page2"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body><h1>Page 2</h1></body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let base = CrawlConfig::builder().allow_private_networks(true).build();
    let config = CrawlConfig {
        max_depth: Some(1),
        ..base
    };
    let handle = create_engine(Some(config)).unwrap();

    let result = crawl(&handle, &mock.uri()).await.unwrap();
    assert_eq!(
        result.pages.len(),
        3,
        "max_depth=1 must return root + 2 children (3 total), got: {:?}",
        result.pages.iter().map(|p| p.url.as_str()).collect::<Vec<_>>()
    );
}

/// Regression test: `InMemoryFrontier.seen` must not leak across `crawl()` calls on a
/// reused engine. `CrawlEngine::clone()` shares the same `Arc<dyn Frontier>`, so without
/// per-call isolation the second `crawl()` call would find every URL from the first call
/// already marked "seen" and silently return a truncated result.
#[tokio::test]
async fn test_two_sequential_crawls_on_one_engine_return_same_page_count() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(
                    r#"<html><body><h1>Home</h1><a href="/page1">P1</a><a href="/page2">P2</a></body></html>"#,
                )
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/page1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body><h1>Page 1</h1></body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/page2"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body><h1>Page 2</h1></body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let base = CrawlConfig::builder().allow_private_networks(true).build();
    let config = CrawlConfig {
        max_depth: Some(1),
        ..base
    };
    let handle = create_engine(Some(config)).unwrap();

    let first = crawl(&handle, &mock.uri()).await.unwrap();
    let second = crawl(&handle, &mock.uri()).await.unwrap();

    assert_eq!(
        first.pages.len(),
        3,
        "first crawl must return root + 2 children, got: {:?}",
        first.pages.iter().map(|p| p.url.as_str()).collect::<Vec<_>>()
    );
    assert_eq!(
        second.pages.len(),
        first.pages.len(),
        "second crawl on the same reused engine must return the same page count as the \
         first ({}), got {}: {:?} — frontier `seen` state leaked across crawl() calls",
        first.pages.len(),
        second.pages.len(),
        second.pages.iter().map(|p| p.url.as_str()).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn test_duplicate_links_deduplicated() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body><a href=\"/b\">B</a><a href=\"/c\">C</a></body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/b"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body><a href=\"/c\">C again</a></body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/c"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>Page C</body></html>")
                .append_header("content-type", "text/html"),
        )
        .expect(1)
        .mount(&mock)
        .await;

    let base = CrawlConfig::builder().allow_private_networks(true).build();
    let config = CrawlConfig {
        max_depth: Some(2),
        max_concurrent: Some(1),
        ..base
    };
    let handle = create_engine(Some(config)).unwrap();

    let result = crawl(&handle, &mock.uri()).await.unwrap();
    assert_eq!(
        result.pages.len(),
        3,
        "should crawl exactly 3 unique pages, got: {:?}",
        result.pages.iter().map(|p| &p.url).collect::<Vec<_>>()
    );
}
