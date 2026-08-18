//! Integration tests for the `Frontier` extension point: a frontier injected through
//! `CrawlEngineBuilder::frontier` must actually drive the crawl, not sit unused beside a
//! working set the engine keeps to itself.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use crawlberg::traits::{Frontier, FrontierEntry};
use crawlberg::{CrawlConfig, CrawlEngine, CrawlError, InMemoryFrontier};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Shared record of what the engine asked the frontier to do.
///
/// `CrawlEngineBuilder::frontier` takes the frontier by value and wraps it in an
/// `Arc<dyn Frontier>` the test can never reach again, so every observation channel has to
/// be cloned out before the instance is handed over.
#[derive(Debug, Default, Clone)]
struct FrontierCalls {
    push: Arc<AtomicUsize>,
    pop_batch: Arc<AtomicUsize>,
    pushed_urls: Arc<Mutex<Vec<String>>>,
    /// The queue the recorder delegates to, kept reachable so a test can inspect what the
    /// frontier still holds once the crawl has returned.
    queue: Arc<InMemoryFrontier>,
}

impl FrontierCalls {
    fn pushed(&self) -> Vec<String> {
        self.pushed_urls.lock().expect("push log must not be poisoned").clone()
    }
}

/// Delegates real queue semantics to `InMemoryFrontier` and records every call.
///
/// ~keep Deliberately does not override `Frontier::isolated`. `CrawlEngine::crawl` routes
/// through `with_isolated_frontier`, which swaps in `isolated()`'s return value when it is
/// `Some` — an override would hand the engine a different instance and this recorder would
/// observe nothing. The trait default returns `None`, which is what this test needs.
#[derive(Debug)]
struct RecordingFrontier {
    inner: Arc<InMemoryFrontier>,
    calls: FrontierCalls,
}

impl RecordingFrontier {
    fn new() -> (Self, FrontierCalls) {
        let calls = FrontierCalls::default();
        (
            Self {
                inner: Arc::clone(&calls.queue),
                calls: calls.clone(),
            },
            calls,
        )
    }
}

#[async_trait]
impl Frontier for RecordingFrontier {
    async fn push(&self, entry: FrontierEntry) -> Result<(), CrawlError> {
        self.calls.push.fetch_add(1, Ordering::SeqCst);
        self.calls
            .pushed_urls
            .lock()
            .expect("push log must not be poisoned")
            .push(entry.url.clone());
        self.inner.push(entry).await
    }

    async fn pop(&self) -> Result<Option<FrontierEntry>, CrawlError> {
        self.inner.pop().await
    }

    // ~keep Must be overridden explicitly: the trait's default `pop_batch` loops over `pop`,
    // so without this the engine's `pop_batch` calls would never be counted.
    async fn pop_batch(&self, n: usize) -> Result<Vec<FrontierEntry>, CrawlError> {
        self.calls.pop_batch.fetch_add(1, Ordering::SeqCst);
        self.inner.pop_batch(n).await
    }

    async fn len(&self) -> Result<usize, CrawlError> {
        self.inner.len().await
    }

    async fn is_seen(&self, url: &str) -> Result<bool, CrawlError> {
        self.inner.is_seen(url).await
    }

    async fn mark_seen(&self, url: &str) -> Result<(), CrawlError> {
        self.inner.mark_seen(url).await
    }
}

/// A frontier that accepts the seed and then refuses every subsequent push.
#[derive(Debug)]
struct FailingFrontier {
    inner: InMemoryFrontier,
    accepted: AtomicUsize,
}

#[async_trait]
impl Frontier for FailingFrontier {
    async fn push(&self, entry: FrontierEntry) -> Result<(), CrawlError> {
        if self.accepted.fetch_add(1, Ordering::SeqCst) == 0 {
            return self.inner.push(entry).await;
        }
        Err(CrawlError::other(format!("backend refused {}", entry.url)))
    }

    async fn pop(&self) -> Result<Option<FrontierEntry>, CrawlError> {
        self.inner.pop().await
    }

    async fn pop_batch(&self, n: usize) -> Result<Vec<FrontierEntry>, CrawlError> {
        self.inner.pop_batch(n).await
    }

    async fn len(&self) -> Result<usize, CrawlError> {
        self.inner.len().await
    }

    async fn is_seen(&self, url: &str) -> Result<bool, CrawlError> {
        self.inner.is_seen(url).await
    }

    async fn mark_seen(&self, url: &str) -> Result<(), CrawlError> {
        self.inner.mark_seen(url).await
    }
}

async fn mount_html(mock: &MockServer, at: &str, body: String) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body)
                .append_header("content-type", "text/html"),
        )
        .mount(mock)
        .await;
}

/// Root links to four children in a deliberately non-alphabetical document order, so a
/// regression that sorts is caught alongside one that reorders.
async fn setup_fanout_mock() -> MockServer {
    let mock = MockServer::start().await;

    mount_html(
        &mock,
        "/",
        r#"<html><body><a href="/delta">d</a><a href="/alpha">a</a>
           <a href="/charlie">c</a><a href="/bravo">b</a></body></html>"#
            .to_owned(),
    )
    .await;

    for child in ["delta", "alpha", "charlie", "bravo"] {
        mount_html(
            &mock,
            &format!("/{child}"),
            format!("<html><body>leaf {child}</body></html>"),
        )
        .await;
    }

    mock
}

fn crawl_config(max_pages: Option<usize>) -> CrawlConfig {
    CrawlConfig {
        max_depth: Some(1),
        max_pages,
        max_concurrent: Some(1),
        ..CrawlConfig::builder().allow_private_networks(true).build()
    }
}

/// Regression test for the frontier queue being dead code.
///
/// The engine used to keep discovered URLs in a local `Vec` and call only
/// `is_seen`/`mark_seen`/`isolated`, so `push`, `pop`, `pop_batch`, `len` and `is_empty`
/// never ran on any custom implementation: a distributed or persistent frontier was accepted
/// by the builder and silently ignored.
#[tokio::test]
async fn should_drive_injected_frontier_push_and_pop_batch_during_crawl() {
    let mock = setup_fanout_mock().await;
    let (frontier, calls) = RecordingFrontier::new();

    let engine = CrawlEngine::builder()
        .config(crawl_config(None))
        .frontier(frontier)
        .build()
        .expect("engine must build");

    let result = engine.crawl(&mock.uri()).await.expect("crawl must succeed");

    // ~keep Guards against a vacuous pass: the call counts below prove nothing if the crawl
    // never got past the seed page.
    assert_eq!(
        result.pages.len(),
        5,
        "expected the root plus four children, got {:?}",
        result.pages.iter().map(|p| p.url.as_str()).collect::<Vec<_>>()
    );

    assert!(
        calls.push.load(Ordering::SeqCst) >= 4,
        "every discovered link must be enqueued through Frontier::push, but only {} push calls \
         were made for four discovered links",
        calls.push.load(Ordering::SeqCst)
    );
    assert!(
        calls.pop_batch.load(Ordering::SeqCst) > 0,
        "the engine's selection window must be refilled through Frontier::pop_batch; zero calls \
         means the injected frontier is still being bypassed"
    );
}

/// The engine must keep using the injected instance rather than a replacement produced by
/// `isolated()`, otherwise nothing a custom frontier does is observable.
#[tokio::test]
async fn should_keep_injected_frontier_when_isolated_returns_none() {
    let mock = setup_fanout_mock().await;
    let (frontier, calls) = RecordingFrontier::new();

    let engine = CrawlEngine::builder()
        .config(crawl_config(None))
        .frontier(frontier)
        .build()
        .expect("engine must build");

    engine.crawl(&mock.uri()).await.expect("crawl must succeed");

    assert!(
        !calls.pushed().is_empty(),
        "the injected frontier recorded nothing, so with_isolated_frontier replaced it"
    );
}

/// Discovered links must reach the frontier in document order.
///
/// `max_pages = 1` makes this a single-fetch fixture: discovery runs before the page is
/// pushed and the loop is cancelled, so the push log holds exactly the root's four children
/// with no interleaving from a second page.
#[tokio::test]
async fn should_push_discovered_links_in_document_order() {
    let mock = setup_fanout_mock().await;
    let base = mock.uri();
    let (frontier, calls) = RecordingFrontier::new();

    let engine = CrawlEngine::builder()
        .config(crawl_config(Some(1)))
        .frontier(frontier)
        .build()
        .expect("engine must build");

    engine.crawl(&base).await.expect("crawl must succeed");

    let discovered: Vec<String> = calls
        .pushed()
        .into_iter()
        .filter_map(|url| url.strip_prefix(&base).map(str::to_owned))
        .filter(|path| !path.is_empty() && path != "/")
        .collect();

    assert_eq!(
        discovered,
        vec![
            "/delta".to_owned(),
            "/alpha".to_owned(),
            "/charlie".to_owned(),
            "/bravo".to_owned()
        ],
        "links must reach the frontier in document order, neither in SSRF-validation \
         completion order nor sorted"
    );
}

/// A crawl stopped by `max_pages` must not lose the URLs it had already queued. Entries the
/// loop had popped into its window but not fetched are pushed back, so everything unvisited
/// is still in the frontier when `crawl()` returns.
///
/// ~keep `max_concurrent = 3` is what makes this meaningful: the window is sized to the
/// concurrency limit, so a single-fetch crawl would never hold a popped-but-unfetched entry
/// and the spill path would go unexercised.
#[tokio::test]
async fn should_leave_unvisited_urls_in_the_frontier_when_max_pages_stops_the_crawl() {
    let mock = setup_fanout_mock().await;
    let (frontier, calls) = RecordingFrontier::new();

    let config = CrawlConfig {
        max_depth: Some(1),
        max_pages: Some(2),
        max_concurrent: Some(3),
        ..CrawlConfig::builder().allow_private_networks(true).build()
    };
    let engine = CrawlEngine::builder()
        .config(config)
        .frontier(frontier)
        .build()
        .expect("engine must build");

    let result = engine.crawl(&mock.uri()).await.expect("crawl must succeed");
    assert_eq!(result.pages.len(), 2, "max_pages must cap the crawl at two pages");

    let remaining = calls.queue.len().await.expect("len must succeed");
    assert!(
        remaining > 0,
        "the four discovered children minus those fetched must still be queued, but the \
         frontier is empty: entries popped into the window were dropped instead of returned"
    );
}

/// A frontier that cannot accept a URL must fail the crawl rather than silently truncate it.
#[tokio::test]
async fn should_fail_the_crawl_when_the_frontier_rejects_a_push() {
    let mock = setup_fanout_mock().await;

    let engine = CrawlEngine::builder()
        .config(crawl_config(None))
        .frontier(FailingFrontier {
            inner: InMemoryFrontier::new(),
            accepted: AtomicUsize::new(0),
        })
        .build()
        .expect("engine must build");

    let error = engine
        .crawl(&mock.uri())
        .await
        .expect_err("a frontier that rejects a push must fail the crawl");

    let rendered = error.to_string();
    assert!(
        rendered.contains("frontier"),
        "the error must say the frontier push failed, got: {rendered}"
    );
}
