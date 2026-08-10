//! Integration tests for batch_crawl: multiple seed URLs crawled concurrently.

use crawlberg::{CrawlConfig, batch_crawl, create_engine};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn test_batch_crawl_multiple_seeds() {
    let mock = MockServer::start().await;

    for name in ["a", "b", "c"] {
        Mock::given(method("GET"))
            .and(path(format!("/{name}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(format!("<html><body>{name}</body></html>"))
                    .append_header("content-type", "text/html"),
            )
            .mount(&mock)
            .await;
    }

    let base = CrawlConfig::builder().allow_private_networks(true).build();
    let config = CrawlConfig {
        max_depth: Some(0),
        ..base
    };
    let handle = create_engine(Some(config)).unwrap();

    let urls: Vec<String> = ["a", "b", "c"].iter().map(|n| format!("{}/{n}", mock.uri())).collect();

    let results = batch_crawl(&handle, urls).await.expect("batch_crawl should succeed");
    assert_eq!(results.total_count, 3);
    assert_eq!(results.completed_count, 3, "all 3 should succeed");
    assert_eq!(results.failed_count, 0);

    for result in &results.results {
        let crawl = result.result.as_ref().unwrap_or_else(|| {
            panic!(
                "{} failed: {}",
                result.url,
                result.error.as_deref().unwrap_or("unknown")
            )
        });
        assert!(!crawl.pages.is_empty(), "{} should have at least 1 page", result.url);
    }
}

/// Regression test for the frontier race under `batch_crawl`: concurrent `crawl()` calls
/// on one engine previously shared a single `Arc<dyn Frontier>`, so one task's
/// `mark_seen()` for a discovered link could make a sibling task treat that same link as
/// already seen before the sibling ever enqueued it — non-deterministically truncating
/// its page count. Each `batch_crawl` seed now gets an isolated frontier, so running the
/// same link structure many times concurrently must deterministically return the full
/// page count every time.
#[tokio::test]
async fn test_batch_crawl_concurrent_identical_seeds_do_not_share_frontier_state() {
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
                .set_body_string("<html><body>Page B</body></html>")
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
        .mount(&mock)
        .await;

    let base = CrawlConfig::builder().allow_private_networks(true).build();
    let config = CrawlConfig {
        max_depth: Some(1),
        ..base
    };
    let handle = create_engine(Some(config)).unwrap();

    const CONCURRENT_IDENTICAL_SEEDS: usize = 8;
    let urls: Vec<String> = std::iter::repeat_n(mock.uri(), CONCURRENT_IDENTICAL_SEEDS).collect();

    let results = batch_crawl(&handle, urls).await.expect("batch_crawl should succeed");
    assert_eq!(results.total_count, CONCURRENT_IDENTICAL_SEEDS);
    assert_eq!(
        results.completed_count, CONCURRENT_IDENTICAL_SEEDS,
        "all identical concurrent seeds should succeed"
    );

    for result in &results.results {
        let crawl = result.result.as_ref().unwrap_or_else(|| {
            panic!(
                "{} failed: {}",
                result.url,
                result.error.as_deref().unwrap_or("unknown")
            )
        });
        assert_eq!(
            crawl.pages.len(),
            3,
            "each concurrent crawl of the same seed must independently discover root + 2 \
             children (3 pages); a shared frontier would non-deterministically truncate \
             this to fewer pages, got: {:?}",
            crawl.pages.iter().map(|p| p.url.as_str()).collect::<Vec<_>>()
        );
    }
}

#[tokio::test]
async fn test_batch_crawl_partial_failure() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/ok"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>OK</body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/fail"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&mock)
        .await;

    let base = CrawlConfig::builder().allow_private_networks(true).build();
    let config = CrawlConfig {
        max_depth: Some(0),
        ..base
    };
    let handle = create_engine(Some(config)).unwrap();

    let urls = vec![format!("{}/ok", mock.uri()), format!("{}/fail", mock.uri())];

    let results = batch_crawl(&handle, urls).await.expect("batch_crawl should succeed");
    assert_eq!(results.total_count, 2);

    assert!(results.completed_count >= 1, "at least one seed should succeed");
}
