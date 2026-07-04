//! Tests for memory-bounded streaming crawl behavior.
//!
//! Verifies that `crawl_stream()` emits page events without accumulating all pages in memory,
//! while `crawl()` continues to return a full `CrawlResult` with all pages.

use crawlberg::{CrawlConfig, CrawlEvent, crawl, crawl_stream, create_engine};
use tokio_stream::StreamExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn engine_with_config(config: CrawlConfig) -> crawlberg::CrawlEngineHandle {
    create_engine(Some(config)).expect("engine build must not fail")
}

fn default_engine() -> crawlberg::CrawlEngineHandle {
    // Allow loopback so MockServer is reachable under the default SSRF policy.
    engine_with_config(CrawlConfig::builder().allow_private_networks(true).build())
}

/// Create a mock server with N linked pages (chain: /page0 -> /page1 -> /page2 -> ...).
async fn setup_mock_chain(n: usize) -> (MockServer, String) {
    let mock = MockServer::start().await;

    for i in 0..n {
        let next_path = if i + 1 < n {
            format!("/page{}", i + 1)
        } else {
            String::new()
        };

        let body = if next_path.is_empty() {
            format!("<html><body>Page {}</body></html>", i)
        } else {
            format!("<html><body>Page {}<a href=\"{}\">next</a></body></html>", i, next_path)
        };

        Mock::given(method("GET"))
            .and(path(format!("/page{}", i).as_str()))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(body)
                    .append_header("content-type", "text/html"),
            )
            .mount(&mock)
            .await;
    }

    let start_url = format!("{}/page0", mock.uri());
    (mock, start_url)
}

/// Verify that `crawl_stream()` emits one Page event per crawled page,
/// a final Complete event, and does NOT accumulate pages in the result.
#[tokio::test]
async fn streaming_crawl_emits_events_without_accumulation() {
    let (_mock, url) = setup_mock_chain(3).await;

    let engine = default_engine();
    let stream_result = crawl_stream(&engine, &url).await;
    assert!(stream_result.is_ok(), "crawl_stream must not fail");

    let mut stream = stream_result.unwrap();

    let mut page_count = 0;
    let mut complete_event = None;

    while let Some(event_result) = stream.next().await {
        let event = event_result.expect("stream event must not have transport error");
        match event {
            CrawlEvent::Page { result: _ } => {
                page_count += 1;
            }
            CrawlEvent::Complete { pages_crawled } => {
                complete_event = Some(pages_crawled);
                break;
            }
            CrawlEvent::Error { url: _, error } => {
                panic!("unexpected error event: {}", error);
            }
        }
    }

    assert_eq!(page_count, 3, "should emit 3 Page events");
    assert_eq!(complete_event, Some(3), "Complete event should report 3 pages_crawled");
}

/// Verify that `crawl()` (non-streaming) still returns all pages in the result.
#[tokio::test]
async fn non_streaming_crawl_returns_all_pages() {
    let (_mock, url) = setup_mock_chain(3).await;

    let engine = default_engine();
    let result = crawl(&engine, &url).await.expect("crawl must succeed");

    assert_eq!(
        result.pages.len(),
        3,
        "non-streaming crawl should return all 3 pages in result"
    );
}

/// Verify that streaming crawl respects max_pages limit and emits correct count.
#[tokio::test]
async fn streaming_crawl_respects_max_pages() {
    let (_mock, url) = setup_mock_chain(5).await;

    let engine = engine_with_config(CrawlConfig::builder().allow_private_networks(true).max_pages(2).build());
    let stream_result = crawl_stream(&engine, &url).await;
    assert!(stream_result.is_ok(), "crawl_stream must not fail");

    let mut stream = stream_result.unwrap();

    let mut page_count = 0;
    let mut complete_event = None;

    while let Some(event_result) = stream.next().await {
        let event = event_result.expect("stream event must not have transport error");
        match event {
            CrawlEvent::Page { result: _ } => {
                page_count += 1;
            }
            CrawlEvent::Complete { pages_crawled } => {
                complete_event = Some(pages_crawled);
                break;
            }
            CrawlEvent::Error { url: _, error } => {
                panic!("unexpected error event: {}", error);
            }
        }
    }

    assert_eq!(page_count, 2, "should emit 2 Page events (limited by max_pages)");
    assert_eq!(complete_event, Some(2), "Complete event should report 2 pages_crawled");
}

/// Verify that non-streaming crawl also respects max_pages and returns only max_pages.
#[tokio::test]
async fn non_streaming_crawl_respects_max_pages() {
    let (_mock, url) = setup_mock_chain(5).await;

    let engine = engine_with_config(CrawlConfig::builder().allow_private_networks(true).max_pages(2).build());
    let result = crawl(&engine, &url).await.expect("crawl must succeed");

    assert_eq!(
        result.pages.len(),
        2,
        "non-streaming crawl should return only 2 pages (limited by max_pages)"
    );
}

/// Verify that streaming correctly reports pages_crawled even with depth limit.
#[tokio::test]
async fn streaming_crawl_with_depth_limit() {
    let (_mock, url) = setup_mock_chain(5).await;

    let engine = engine_with_config(
        CrawlConfig::builder()
            .allow_private_networks(true)
            .max_depth(1) // Only crawl seed + 1 level
            .build(),
    );
    let stream_result = crawl_stream(&engine, &url).await;
    assert!(stream_result.is_ok(), "crawl_stream must not fail");

    let mut stream = stream_result.unwrap();

    let mut page_count = 0;
    let mut complete_event = None;

    while let Some(event_result) = stream.next().await {
        let event = event_result.expect("stream event must not have transport error");
        match event {
            CrawlEvent::Page { result: _ } => {
                page_count += 1;
            }
            CrawlEvent::Complete { pages_crawled } => {
                complete_event = Some(pages_crawled);
                break;
            }
            CrawlEvent::Error { url: _, error } => {
                panic!("unexpected error event: {}", error);
            }
        }
    }

    assert_eq!(page_count, 2, "should emit 2 Page events (depth 0 + 1)");
    assert_eq!(complete_event, Some(2), "Complete event should report 2 pages_crawled");
}

/// Verify that streaming correctly reports exactly one Complete event.
/// This is a regression test for the bug where Complete was being emitted twice
/// (once in crawl_with_sender and once in batch.rs).
#[tokio::test]
async fn streaming_crawl_emits_exactly_one_complete_event() {
    let (_mock, url) = setup_mock_chain(3).await;

    let engine = default_engine();
    let stream_result = crawl_stream(&engine, &url).await;
    assert!(stream_result.is_ok(), "crawl_stream must not fail");

    let mut stream = stream_result.unwrap();

    let mut page_count = 0;
    let mut complete_count = 0;

    while let Some(event_result) = stream.next().await {
        let event = event_result.expect("stream event must not have transport error");
        match event {
            CrawlEvent::Page { result: _ } => {
                page_count += 1;
            }
            CrawlEvent::Complete { pages_crawled } => {
                complete_count += 1;
                // Verify the count matches the number of Page events
                assert_eq!(
                    pages_crawled, page_count,
                    "Complete event pages_crawled must equal number of Page events emitted"
                );
            }
            CrawlEvent::Error { url: _, error } => {
                panic!("unexpected error event: {}", error);
            }
        }
    }

    assert_eq!(page_count, 3, "should emit 3 Page events");
    assert_eq!(complete_count, 1, "should emit exactly 1 Complete event");
}

/// Regression: a seed that cannot be fetched (connection refused) must still emit
/// a terminal Complete after the Error. `batch.rs` used to emit Complete on every
/// Ok return; now that the authoritative Complete lives in the crawl loop, the
/// early-error return path must emit it too — otherwise streaming consumers lose
/// the canonical end-of-stream marker on seed/redirect failure.
#[tokio::test]
async fn streaming_crawl_seed_error_still_emits_complete() {
    // Bind then drop a listener to obtain a port that is (almost certainly) closed.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind probe socket");
    let port = listener.local_addr().expect("probe addr").port();
    drop(listener);
    let url = format!("http://127.0.0.1:{}/page0", port);

    let engine = default_engine();
    let stream_result = crawl_stream(&engine, &url).await;
    assert!(stream_result.is_ok(), "crawl_stream must not fail to start");
    let mut stream = stream_result.unwrap();

    let mut error_count = 0;
    let mut complete_count = 0;
    while let Some(event_result) = stream.next().await {
        let event = event_result.expect("stream event must not have transport error");
        match event {
            CrawlEvent::Error { .. } => error_count += 1,
            CrawlEvent::Complete { pages_crawled } => {
                complete_count += 1;
                assert_eq!(pages_crawled, 0, "seed-error crawl should report 0 pages_crawled");
            }
            CrawlEvent::Page { .. } => panic!("no Page event should be emitted for an unreachable seed"),
        }
    }

    assert!(error_count >= 1, "should emit an Error for the unreachable seed");
    assert_eq!(complete_count, 1, "should emit exactly one Complete even on seed error");
}
