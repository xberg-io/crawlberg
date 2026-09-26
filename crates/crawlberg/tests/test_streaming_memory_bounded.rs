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

    let engine = engine_with_config(CrawlConfig::builder().allow_private_networks(true).max_depth(1).build());
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

/// Counts `on_complete` calls so a test can wait for a crawl to finish after its stream is gone.
#[derive(Clone)]
struct CompletionCounter {
    completed: std::sync::Arc<tokio::sync::watch::Sender<usize>>,
}

impl CompletionCounter {
    fn new() -> Self {
        Self {
            completed: std::sync::Arc::new(tokio::sync::watch::channel(0).0),
        }
    }

    /// Wait until at least `count` crawls have completed, or until `limit` passes.
    async fn wait_for(&self, count: usize, limit: std::time::Duration) -> bool {
        let mut completed = self.completed.subscribe();
        tokio::time::timeout(limit, completed.wait_for(|done| *done >= count))
            .await
            .is_ok()
    }
}

#[async_trait::async_trait]
impl crawlberg::traits::EventEmitter for CompletionCounter {
    async fn on_page(&self, _event: &crawlberg::traits::PageEvent) {}
    async fn on_error(&self, _event: &crawlberg::traits::ErrorEvent) {}
    async fn on_complete(&self, _event: &crawlberg::traits::CompleteEvent) {
        self.completed.send_modify(|done| *done += 1);
    }
    async fn on_discovered(&self, _url: &str, _depth: usize) {}
}

fn engine_with_counter(
    max_concurrent: usize,
    retry_count: usize,
    counter: &CompletionCounter,
) -> crawlberg::CrawlEngine {
    crawlberg::CrawlEngine::builder()
        .config(
            CrawlConfig::builder()
                .allow_private_networks(true)
                .max_depth(1)
                .max_concurrent(max_concurrent)
                .retry_count(retry_count)
                .retry_initial_delay_ms(100)
                .build(),
        )
        .event_emitter(counter.clone())
        .build()
        .expect("engine build must not fail")
}

async fn request_count(mock: &MockServer, path_prefix: &str) -> usize {
    mock.received_requests()
        .await
        .expect("request recording is on")
        .iter()
        .filter(|request| request.url.path().starts_with(path_prefix))
        .count()
}

/// Wait for the stream's first event and require it to be a page. A crawl that never yields one
/// fails here after `limit` instead of hanging the test.
async fn expect_first_page(
    stream: &mut tokio_stream::wrappers::ReceiverStream<CrawlEvent>,
    limit: std::time::Duration,
) {
    let event = tokio::time::timeout(limit, stream.next())
        .await
        .expect("the stream must yield its first event in time")
        .expect("the stream must yield a first event");
    match event {
        CrawlEvent::Page { .. } => {}
        CrawlEvent::Error { error, .. } => panic!("unexpected error event: {error}"),
        CrawlEvent::Complete { .. } => panic!("the crawl ended before its first page"),
    }
}

/// Wait until the server has received a request under `path_prefix`, or until `limit` passes.
async fn wait_for_request(mock: &MockServer, path_prefix: &str, limit: std::time::Duration) -> bool {
    tokio::time::timeout(limit, async {
        while request_count(mock, path_prefix).await == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .is_ok()
}

/// Dropping the stream must stop the crawl from starting new requests. The loop used to
/// notice a dropped receiver only when it next sent a page, so pages that fail (a 5xx,
/// whose error event send failed silently) kept the crawl fetching the rest of the site,
/// and a fetch in flight at the drop went on to make its retries.
#[tokio::test]
async fn dropping_the_stream_stops_new_requests() {
    const MAX_CONCURRENT: usize = 2;
    let mock = MockServer::start().await;
    let links: String = (0..20).map(|n| format!("<a href=\"/many/p{n}\">p{n}</a>")).collect();
    Mock::given(method("GET"))
        .and(path("/many/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(format!("<html><body>{links}</body></html>"))
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(wiremock::matchers::path_regex(r"^/many/p\d+$"))
        .respond_with(ResponseTemplate::new(503).set_delay(std::time::Duration::from_millis(50)))
        .mount(&mock)
        .await;

    let counter = CompletionCounter::new();
    let engine = engine_with_counter(MAX_CONCURRENT, 2, &counter);
    let mut stream = engine.crawl_stream(&format!("{}/many/", mock.uri()));
    expect_first_page(&mut stream, std::time::Duration::from_secs(10)).await;
    drop(stream);
    let at_drop = request_count(&mock, "/").await;

    assert!(
        counter.wait_for(1, std::time::Duration::from_secs(10)).await,
        "the crawl must finish once its stream is dropped"
    );
    let started_after_drop = request_count(&mock, "/").await - at_drop;
    assert!(
        started_after_drop <= MAX_CONCURRENT,
        "only fetches already in flight at the drop may reach the server, got {started_after_drop} more requests"
    );
}

/// The batch stream must also stop starting seeds once its receiver is gone: a seed crawl
/// that begins after the drop abandons its seed before fetching it. Each seed links to a
/// slow child so the first crawl is still running when the stream is dropped.
#[tokio::test]
async fn dropping_the_batch_stream_stops_new_seeds() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(wiremock::matchers::path_regex(r"^/seed\d+$"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body><a href=\"/slow\">slow</a></body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/slow"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>slow</body></html>")
                .append_header("content-type", "text/html")
                .set_delay(std::time::Duration::from_millis(300)),
        )
        .mount(&mock)
        .await;

    let counter = CompletionCounter::new();
    let engine = engine_with_counter(1, 0, &counter);
    let seeds: Vec<String> = (0..6).map(|n| format!("{}/seed{n}", mock.uri())).collect();
    let seed_refs: Vec<&str> = seeds.iter().map(String::as_str).collect();
    let mut stream = engine.batch_crawl_stream(&seed_refs);
    expect_first_page(&mut stream, std::time::Duration::from_secs(10)).await;
    drop(stream);
    let at_drop = request_count(&mock, "/seed").await;

    assert!(
        counter.wait_for(1, std::time::Duration::from_secs(10)).await,
        "the running crawl must finish once the stream is dropped"
    );
    // ~keep Proving that no further seed starts needs a bounded wait: a second completion
    // ~keep would arrive within milliseconds if the next seed were started.
    counter.wait_for(2, std::time::Duration::from_secs(1)).await;
    let seeds_after_drop = request_count(&mock, "/seed").await - at_drop;
    assert_eq!(seeds_after_drop, 0, "no seed may start after the drop");
}

/// A seed still being fetched when the stream is dropped must not go on to retry.
#[tokio::test]
async fn dropping_the_stream_during_the_seed_fetch_stops_its_retries() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/seed"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&mock)
        .await;

    let counter = CompletionCounter::new();
    let engine = engine_with_counter(1, 3, &counter);
    let stream = engine.crawl_stream(&format!("{}/seed", mock.uri()));
    // ~keep The first attempt reaching the server is the drop point; its retries wait
    // ~keep 100ms, 200ms and 400ms, so a retry cannot land before the drop does.
    assert!(
        wait_for_request(&mock, "/seed", std::time::Duration::from_secs(10)).await,
        "the seed's first attempt must reach the server"
    );
    drop(stream);

    assert!(
        counter.wait_for(1, std::time::Duration::from_secs(10)).await,
        "the crawl must finish once its stream is dropped"
    );
    assert_eq!(
        request_count(&mock, "/seed").await,
        1,
        "the seed must not be retried after the drop"
    );
}

/// Holds the seed page's sink emit until the stream is dropped, so the drop lands while the
/// loop is still processing that page, before it starts the next fetches.
struct HoldSeedPage {
    dropped: std::sync::Arc<tokio::sync::watch::Sender<bool>>,
}

#[async_trait::async_trait]
impl crawlberg::EventSink for HoldSeedPage {
    async fn emit(&self, event: CrawlEvent) {
        if matches!(event, CrawlEvent::Page { .. }) {
            let _ = self.dropped.subscribe().wait_for(|dropped| *dropped).await;
        }
    }
}

/// Slows every budget check made after the drop, so a fetch started before the next check
/// has time to reach the server before the loop could abort it.
struct SlowAfterDrop {
    dropped: std::sync::Arc<tokio::sync::watch::Sender<bool>>,
}

#[async_trait::async_trait]
impl crawlberg::PageBudget for SlowAfterDrop {
    async fn check(&self) -> Result<(), crawlberg::BudgetError> {
        if *self.dropped.borrow() {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }
        Ok(())
    }
}

/// A drop that lands while a page is being processed must stop the crawl before it starts
/// another fetch. On a multi-thread runtime a fetch spawned after the drop runs at once, so
/// aborting it afterwards is too late for the request it already sent.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_drop_during_page_processing_starts_no_further_fetch() {
    let mock = MockServer::start().await;
    let links: String = (0..4).map(|n| format!("<a href=\"/p{n}\">p{n}</a>")).collect();
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(format!("<html><body>{links}</body></html>"))
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(wiremock::matchers::path_regex(r"^/p\d+$"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>child</body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let dropped = std::sync::Arc::new(tokio::sync::watch::channel(false).0);
    let counter = CompletionCounter::new();
    let engine = crawlberg::CrawlEngine::builder()
        .config(
            CrawlConfig::builder()
                .allow_private_networks(true)
                .max_depth(1)
                .max_concurrent(2)
                .build(),
        )
        .event_emitter(counter.clone())
        .event_sink(HoldSeedPage {
            dropped: std::sync::Arc::clone(&dropped),
        })
        .page_budget(SlowAfterDrop {
            dropped: std::sync::Arc::clone(&dropped),
        })
        .build()
        .expect("engine build must not fail");

    let mut stream = engine.crawl_stream(&format!("{}/", mock.uri()));
    expect_first_page(&mut stream, std::time::Duration::from_secs(10)).await;
    drop(stream);
    dropped.send_replace(true);

    assert!(
        counter.wait_for(1, std::time::Duration::from_secs(10)).await,
        "the crawl must finish once its stream is dropped"
    );
    assert_eq!(
        request_count(&mock, "/p").await,
        0,
        "no child page may be requested once the stream is dropped"
    );
}
