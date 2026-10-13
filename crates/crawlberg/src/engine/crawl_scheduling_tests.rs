use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use wiremock::matchers::path;
use wiremock::{Mock, MockServer, ResponseTemplate};

struct SlowTurns {
    calls: AtomicUsize,
    pending: Arc<AtomicUsize>,
    max_pending: AtomicUsize,
    blocked: AtomicBool,
    release: tokio::sync::Notify,
}

struct PendingTurn(Arc<AtomicUsize>);
impl Drop for PendingTurn {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

struct StopAfterFree(Arc<AtomicBool>);
impl CrawlStrategy for StopAfterFree {
    fn select_next(&self, entries: &[FrontierEntry]) -> Option<usize> {
        (!entries.is_empty()).then_some(0)
    }
    fn should_continue(&self, _: &CrawlStats) -> bool {
        !self.0.load(Ordering::Acquire)
    }
    fn on_page_processed(&self, page: &CrawlPageResult) {
        if Url::parse(&page.url).unwrap().path() == "/4" {
            self.0.store(true, Ordering::Release);
        }
    }
}

#[async_trait::async_trait]
impl RateLimiter for SlowTurns {
    async fn acquire(&self, host: &str) -> Result<(), CrawlError> {
        if host == "127.0.0.1" && self.calls.fetch_add(1, Ordering::SeqCst) > 0 && self.blocked.load(Ordering::Acquire)
        {
            let pending = self.pending.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_pending.fetch_max(pending, Ordering::SeqCst);
            let _pending = PendingTurn(Arc::clone(&self.pending));
            self.release.notified().await;
        }
        Ok(())
    }
    async fn record_response(&self, _: &str, _: u16) -> Result<(), CrawlError> {
        Ok(())
    }
    async fn set_crawl_delay(&self, _: &str, _: Duration) -> Result<(), CrawlError> {
        Ok(())
    }
}

#[tokio::test]
async fn should_skip_a_non_head_delayed_host_in_the_builtin_frontier() {
    let site = MockServer::start().await;
    for index in 0..5 {
        Mock::given(path(format!("/{index}")))
            .respond_with(ResponseTemplate::new(200).set_body_raw("<p>page</p>", "text/html"))
            .mount(&site)
            .await;
    }
    let limiter = Arc::new(SlowTurns {
        calls: Default::default(),
        pending: Arc::default(),
        max_pending: Default::default(),
        blocked: true.into(),
        release: Default::default(),
    });
    let mut config = CrawlConfig::builder().allow_private_networks(true).build();
    config.max_concurrent = Some(2);
    config.respect_robots_txt = false;
    let mut engine = CrawlEngine::builder().config(config).build().unwrap();
    engine.rate_limiter = limiter.clone();
    for index in 0..5 {
        let url = if index == 4 {
            site.uri().replace("127.0.0.1", "localhost")
        } else {
            site.uri()
        };
        engine
            .frontier
            .push(FrontierEntry {
                url: format!("{url}/{index}"),
                original_url: None,
                depth: index,
                doc_depth: index as u32,
                priority: index as f64,
            })
            .await
            .unwrap();
    }
    let running = tokio::spawn(async move {
        let mut state = CrawlState::new(5, false);
        state.frontier_pending = 5;
        let robots = RobotsOutcome::AllowAll;
        let tx = None;
        let context = LoopContext {
            exclude_regexes: Arc::from([]),
            include_regexes: Arc::from([]),
            robots: &robots,
            base_host: "127.0.0.1",
            base_host_suffix: ".127.0.0.1",
            max_depth: 10,
            max_pages: 5,
            start_time: Instant::now(),
            tx: &tx,
        };
        engine.run_crawl_loop(&mut state, &mut None, &context).await.unwrap();
        state
    });
    let free = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let requests = site.received_requests().await.unwrap();
            if requests.iter().any(|request| request.url.path() == "/4") {
                assert_eq!(requests.iter().filter(|request| request.url.path() == "/1").count(), 0);
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    if free.is_err() {
        running.abort();
        panic!("the free host behind four queued slow pages must be requested before their second turn");
    }
    limiter.blocked.store(false, Ordering::Release);
    limiter.release.notify_waiters();
    let state = tokio::time::timeout(Duration::from_secs(2), running)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.pages_processed(), 5);
    assert_eq!(site.received_requests().await.unwrap().len(), 5);
}

#[tokio::test]
async fn should_cancel_host_waiters_and_return_exact_metadata_when_the_strategy_stops() {
    let site = MockServer::start().await;
    for index in 0..5 {
        Mock::given(path(format!("/{index}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw("<p>page</p>", "text/html")
                    .set_delay(Duration::from_millis(if index == 4 { 50 } else { 0 })),
            )
            .mount(&site)
            .await;
    }
    let limiter = Arc::new(SlowTurns {
        calls: Default::default(),
        pending: Arc::default(),
        max_pending: Default::default(),
        blocked: true.into(),
        release: Default::default(),
    });
    let stopped = Arc::new(AtomicBool::new(false));
    let mut config = CrawlConfig::builder().allow_private_networks(true).build();
    config.max_concurrent = Some(2);
    config.respect_robots_txt = false;
    let mut engine = CrawlEngine::builder()
        .config(config)
        .strategy(StopAfterFree(stopped))
        .build()
        .unwrap();
    engine.rate_limiter = limiter.clone();
    let mut original = Vec::new();
    for index in 0..5 {
        let origin = if index == 4 {
            site.uri().replace("127.0.0.1", "localhost")
        } else {
            site.uri()
        };
        let entry = FrontierEntry {
            url: format!("{origin}/{index}?raw=%2f"),
            original_url: Some(format!("{origin}/{index}?raw=%2f#source")),
            depth: index,
            doc_depth: index as u32,
            priority: index as f64 + 0.5,
        };
        original.push(entry.clone());
        engine.frontier.push(entry).await.unwrap();
    }
    let mut state = CrawlState::new(5, false);
    state.frontier_pending = 5;
    let robots = RobotsOutcome::AllowAll;
    let tx = None;
    let context = LoopContext {
        exclude_regexes: Arc::from([]),
        include_regexes: Arc::from([]),
        robots: &robots,
        base_host: "127.0.0.1",
        base_host_suffix: ".127.0.0.1",
        max_depth: 10,
        max_pages: 5,
        start_time: Instant::now(),
        tx: &tx,
    };
    tokio::time::timeout(
        Duration::from_secs(2),
        engine.run_crawl_loop(&mut state, &mut None, &context),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        limiter.max_pending.load(Ordering::SeqCst),
        1,
        "one acquire waiter actually ran for the delayed host"
    );
    assert_eq!(
        limiter.pending.load(Ordering::SeqCst),
        0,
        "the per-host acquire future must be dropped at early stop"
    );
    let processed = state.pages.iter().map(|page| page.url.as_str()).collect::<Vec<_>>();
    let mut expected = original
        .into_iter()
        .filter(|entry| !processed.contains(&entry.url.as_str()))
        .collect::<Vec<_>>();
    let mut retained = engine.frontier.pop_batch(5).await.unwrap();
    expected.sort_by(|left, right| left.url.cmp(&right.url));
    retained.sort_by(|left, right| left.url.cmp(&right.url));
    assert_eq!(
        serde_json::to_value(retained).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    assert_eq!(
        site.received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.url.path() == "/1")
            .count(),
        0
    );
}

struct RetryTurns {
    pending: Arc<AtomicUsize>,
    held: Arc<AtomicBool>,
    release: tokio::sync::Notify,
}

#[async_trait::async_trait]
impl RateLimiter for RetryTurns {
    async fn acquire(&self, host: &str) -> Result<(), CrawlError> {
        if host == "127.0.0.1" && self.held.load(Ordering::Acquire) {
            self.pending.fetch_add(1, Ordering::SeqCst);
            let _pending = PendingTurn(self.pending.clone());
            self.release.notified().await;
        }
        Ok(())
    }
    async fn record_response(&self, _: &str, _: u16) -> Result<(), CrawlError> {
        Ok(())
    }
    async fn set_crawl_delay(&self, _: &str, _: Duration) -> Result<(), CrawlError> {
        Ok(())
    }
}

#[tokio::test]
async fn should_admit_a_free_frontier_host_when_configured_retries_wait_inside_both_hops() {
    let site = MockServer::start().await;
    let held = Arc::new(AtomicBool::new(false));
    let hits = Arc::new(AtomicUsize::new(0));
    let signal = Arc::clone(&held);
    let count = Arc::clone(&hits);
    Mock::given(wiremock::matchers::path_regex("^/source/"))
        .respond_with(move |_: &wiremock::Request| {
            if count.fetch_add(1, Ordering::SeqCst) == 1 {
                signal.store(true, Ordering::Release);
            }
            ResponseTemplate::new(429)
                .insert_header("retry-after", "0")
                .set_delay(Duration::from_millis(100))
        })
        .mount(&site)
        .await;
    Mock::given(path("/free"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<p>free</p>", "text/html"))
        .mount(&site)
        .await;
    let limiter = Arc::new(RetryTurns {
        pending: Arc::default(),
        held,
        release: Default::default(),
    });
    let mut config = CrawlConfig::builder().allow_private_networks(true).build();
    config.max_concurrent = Some(2);
    config.respect_robots_txt = false;
    config.retry_count = 1;
    config.retry_initial_delay_ms = 0;
    config.retry_max_delay_ms = 0;
    let mut engine = CrawlEngine::builder().config(config).build().unwrap();
    engine.rate_limiter = limiter.clone();
    for index in 0..3 {
        let url = if index < 2 {
            format!("{}/source/{index}", site.uri())
        } else {
            format!("{}/free", site.uri().replace("127.0.0.1", "localhost"))
        };
        engine
            .frontier
            .push(FrontierEntry {
                original_url: None,
                url,
                depth: 0,
                doc_depth: 0,
                priority: 1.0,
            })
            .await
            .unwrap();
    }
    let running = tokio::spawn(async move {
        let mut state = CrawlState::new(3, false);
        state.frontier_pending = 3;
        let robots = RobotsOutcome::AllowAll;
        let tx = None;
        let context = LoopContext {
            exclude_regexes: Arc::from([]),
            include_regexes: Arc::from([]),
            robots: &robots,
            base_host: "127.0.0.1",
            base_host_suffix: ".127.0.0.1",
            max_depth: 10,
            max_pages: 3,
            start_time: Instant::now(),
            tx: &tx,
        };
        engine.run_crawl_loop(&mut state, &mut None, &context).await.unwrap();
        state
    });
    let free = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if site
                .received_requests()
                .await
                .unwrap()
                .iter()
                .any(|request| request.url.path() == "/free")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    if free.is_err() {
        running.abort();
        assert_eq!(
            hits.load(Ordering::SeqCst),
            2,
            "both original attempts reached the server"
        );
        panic!("the free frontier host never starts while both configured retry hops wait");
    }
    limiter.held.store(false, Ordering::Release);
    limiter.release.notify_waiters();
    let state = tokio::time::timeout(Duration::from_secs(2), running)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.pages_processed(), 1);
    assert_eq!(state.pages_failed, 2);
    assert_eq!(site.received_requests().await.unwrap().len(), 5);
}

struct CountingLegacyFrontier {
    inner: Arc<crate::defaults::InMemoryFrontier>,
    popped: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Frontier for CountingLegacyFrontier {
    async fn push(&self, entry: FrontierEntry) -> Result<(), CrawlError> {
        self.inner.push(entry).await
    }
    async fn pop(&self) -> Result<Option<FrontierEntry>, CrawlError> {
        let entry = self.inner.pop().await?;
        self.popped.fetch_add(usize::from(entry.is_some()), Ordering::SeqCst);
        Ok(entry)
    }
    async fn pop_batch(&self, n: usize) -> Result<Vec<FrontierEntry>, CrawlError> {
        let entries = self.inner.pop_batch(n).await?;
        self.popped.fetch_add(entries.len(), Ordering::SeqCst);
        Ok(entries)
    }
    async fn len(&self) -> Result<usize, CrawlError> {
        self.inner.len().await
    }
    async fn is_seen(&self, key: &str) -> Result<bool, CrawlError> {
        self.inner.is_seen(key).await
    }
    async fn mark_seen(&self, key: &str) -> Result<(), CrawlError> {
        self.inner.mark_seen(key).await
    }
    async fn claim(&self, key: &str) -> Result<bool, CrawlError> {
        self.inner.claim(key).await
    }
}

#[tokio::test]
async fn should_keep_an_unsupported_custom_frontier_inside_the_original_selection_window() {
    const ENTRIES: usize = 100;
    const CONCURRENCY: usize = 2;
    let site = MockServer::start().await;
    Mock::given(path("/0"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<p>page</p>", "text/html"))
        .mount(&site)
        .await;
    let inner = Arc::new(crate::defaults::InMemoryFrontier::new());
    let popped = Arc::new(AtomicUsize::new(0));
    let limiter = Arc::new(SlowTurns {
        calls: Default::default(),
        pending: Arc::default(),
        max_pending: Default::default(),
        blocked: true.into(),
        release: Default::default(),
    });
    let mut config = CrawlConfig::builder().allow_private_networks(true).build();
    config.max_concurrent = Some(CONCURRENCY);
    config.respect_robots_txt = false;
    let mut engine = CrawlEngine::builder()
        .config(config)
        .frontier(CountingLegacyFrontier {
            inner: inner.clone(),
            popped: popped.clone(),
        })
        .build()
        .unwrap();
    engine.rate_limiter = limiter.clone();
    for index in 0..ENTRIES {
        engine
            .frontier
            .push(FrontierEntry {
                url: format!("{}/{index}", site.uri()),
                original_url: None,
                depth: 0,
                doc_depth: index as u32,
                priority: index as f64,
            })
            .await
            .unwrap();
    }
    let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
    let running = tokio::spawn(async move {
        let mut state = CrawlState::new(ENTRIES, true);
        state.frontier_pending = ENTRIES;
        let robots = RobotsOutcome::AllowAll;
        let tx = Some(sender);
        let context = LoopContext {
            exclude_regexes: Arc::from([]),
            include_regexes: Arc::from([]),
            robots: &robots,
            base_host: "127.0.0.1",
            base_host_suffix: ".127.0.0.1",
            max_depth: 1,
            max_pages: ENTRIES,
            start_time: Instant::now(),
            tx: &tx,
        };
        engine.run_crawl_loop(&mut state, &mut None, &context).await.unwrap();
    });
    tokio::time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .unwrap()
        .expect("the first actual page must be processed");
    let _ = tokio::time::timeout(Duration::from_secs(1), async {
        while popped.load(Ordering::SeqCst) < 10 {
            tokio::task::yield_now().await;
        }
    })
    .await;
    drop(receiver);
    tokio::time::timeout(Duration::from_secs(2), running)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        limiter.max_pending.load(Ordering::SeqCst),
        1,
        "the custom limiter's delayed waiter actually ran"
    );
    assert_eq!(limiter.pending.load(Ordering::SeqCst), 0);
    assert_eq!(site.received_requests().await.unwrap().len(), 1);
    let fetched = popped.load(Ordering::SeqCst);
    assert!(
        fetched <= 1 + 2 * CONCURRENCY,
        "legacy fallback drained {fetched} entries rather than retaining its bounded window"
    );
    assert_eq!(inner.len().await.unwrap(), ENTRIES - 1);
}

struct AdmissionOnlyLimiter {
    pending: Arc<AtomicUsize>,
    release: tokio::sync::Notify,
}
#[async_trait::async_trait]
impl RateLimiter for AdmissionOnlyLimiter {
    async fn acquire(&self, _: &str) -> Result<(), CrawlError> {
        self.pending.fetch_add(1, Ordering::SeqCst);
        let _pending = PendingTurn(Arc::clone(&self.pending));
        self.release.notified().await;
        Ok(())
    }
    async fn record_response(&self, _: &str, _: u16) -> Result<(), CrawlError> {
        Ok(())
    }
    async fn set_crawl_delay(&self, _: &str, _: Duration) -> Result<(), CrawlError> {
        Ok(())
    }
}

#[tokio::test]
async fn should_bound_pending_distinct_hosts_for_an_unsupported_custom_frontier() {
    const ENTRIES: usize = 100;
    const CONCURRENCY: usize = 2;
    let inner = Arc::new(crate::defaults::InMemoryFrontier::new());
    let popped = Arc::new(AtomicUsize::new(0));
    let pending = Arc::new(AtomicUsize::new(0));
    let mut config = CrawlConfig::builder().allow_private_networks(true).build();
    config.max_concurrent = Some(CONCURRENCY);
    config.respect_robots_txt = false;
    let mut engine = CrawlEngine::builder()
        .config(config)
        .frontier(CountingLegacyFrontier {
            inner: inner.clone(),
            popped: popped.clone(),
        })
        .build()
        .unwrap();
    engine.rate_limiter = Arc::new(AdmissionOnlyLimiter {
        pending: pending.clone(),
        release: Default::default(),
    });
    for index in 0..ENTRIES {
        engine
            .frontier
            .push(FrontierEntry {
                url: format!("http://host{index}.example.test/"),
                original_url: None,
                depth: 0,
                doc_depth: index as u32,
                priority: index as f64,
            })
            .await
            .unwrap();
    }
    let (sender, receiver) = tokio::sync::mpsc::channel(4);
    let running = tokio::spawn(async move {
        let mut state = CrawlState::new(ENTRIES, true);
        state.frontier_pending = ENTRIES;
        let robots = RobotsOutcome::AllowAll;
        let tx = Some(sender);
        let context = LoopContext {
            exclude_regexes: Arc::from([]),
            include_regexes: Arc::from([]),
            robots: &robots,
            base_host: "example.test",
            base_host_suffix: ".example.test",
            max_depth: 1,
            max_pages: ENTRIES,
            start_time: Instant::now(),
            tx: &tx,
        };
        engine.run_crawl_loop(&mut state, &mut None, &context).await.unwrap();
        state.pages_processed()
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while pending.load(Ordering::SeqCst) < CONCURRENCY {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the configured number of initial custom admissions must actually wait");
    let _ = tokio::time::timeout(Duration::from_secs(1), async {
        while popped.load(Ordering::SeqCst) < 10 {
            tokio::task::yield_now().await;
        }
    })
    .await;
    drop(receiver);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), running)
            .await
            .unwrap()
            .unwrap(),
        0
    );
    assert_eq!(pending.load(Ordering::SeqCst), 0);
    let fetched = popped.load(Ordering::SeqCst);
    assert!(
        fetched <= 2 * CONCURRENCY,
        "unsupported fallback admitted {fetched} parked host states instead of retaining the original bounded window"
    );
    assert_eq!(inner.len().await.unwrap(), ENTRIES);
}

struct BatchRetryTurns {
    held: Arc<AtomicBool>,
    hits: Arc<AtomicUsize>,
    sources_started: Arc<tokio::sync::Notify>,
    release: tokio::sync::Notify,
}

#[async_trait::async_trait]
impl RateLimiter for BatchRetryTurns {
    async fn acquire(&self, host: &str) -> Result<(), CrawlError> {
        if host == "localhost." {
            let started = self.sources_started.notified();
            if self.hits.load(Ordering::Acquire) < 2 {
                started.await;
            }
        } else if self.held.load(Ordering::Acquire) {
            self.release.notified().await;
        }
        Ok(())
    }
    async fn record_response(&self, _: &str, _: u16) -> Result<(), CrawlError> {
        Ok(())
    }
    async fn set_crawl_delay(&self, _: &str, _: Duration) -> Result<(), CrawlError> {
        Ok(())
    }
}

#[tokio::test]
async fn should_admit_a_free_batch_scrape_host_while_configured_retries_wait() {
    assert_post_429_batch_host_wait_fairness(RetryBatchKind::Scrape).await;
}

#[tokio::test]
async fn should_admit_a_free_public_batch_crawl_host_while_429_retries_wait() {
    assert_post_429_batch_host_wait_fairness(RetryBatchKind::Crawl).await;
}

#[tokio::test]
async fn should_admit_a_free_public_batch_crawl_stream_host_while_429_retries_wait() {
    assert_post_429_batch_host_wait_fairness(RetryBatchKind::Stream).await;
}

enum RetryBatchKind {
    Scrape,
    Crawl,
    Stream,
}

async fn run_post_429_batch(kind: RetryBatchKind, engine: CrawlEngine, urls: [String; 3]) -> Vec<bool> {
    let urls = urls.iter().map(String::as_str).collect::<Vec<_>>();
    match kind {
        RetryBatchKind::Scrape => engine
            .batch_scrape(&urls)
            .await
            .into_iter()
            .map(|(_, result)| result.is_ok())
            .collect(),
        RetryBatchKind::Crawl => engine
            .batch_crawl(&urls)
            .await
            .into_iter()
            .map(|(_, result)| {
                let result = result.unwrap();
                assert_eq!(result.pages.len(), usize::from(result.error.is_none()));
                result.error.is_none()
            })
            .collect(),
        RetryBatchKind::Stream => {
            use tokio_stream::StreamExt;
            let events = engine.batch_crawl_stream(&urls).collect::<Vec<_>>().await;
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, CrawlEvent::Page { .. }))
                    .count(),
                1
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, CrawlEvent::Error { .. }))
                    .count(),
                2
            );
            assert_eq!(events.len(), 6);
            events
                .into_iter()
                .filter_map(|event| match event {
                    CrawlEvent::Complete { pages_crawled } => {
                        assert!(pages_crawled <= 1);
                        Some(pages_crawled == 1)
                    }
                    _ => None,
                })
                .collect()
        }
    }
}

async fn assert_post_429_batch_host_wait_fairness(kind: RetryBatchKind) {
    let site = MockServer::start().await;
    let held = Arc::new(AtomicBool::new(false));
    let hits = Arc::new(AtomicUsize::new(0));
    let signal = held.clone();
    let count = hits.clone();
    let sources_started = Arc::new(tokio::sync::Notify::new());
    let notify = sources_started.clone();
    Mock::given(wiremock::matchers::path_regex("^/source/"))
        .respond_with(move |_: &wiremock::Request| {
            if count.fetch_add(1, Ordering::SeqCst) == 1 {
                signal.store(true, Ordering::Release);
                notify.notify_waiters();
            }
            ResponseTemplate::new(429)
                .insert_header("retry-after", "0")
                .set_delay(Duration::from_millis(100))
        })
        .mount(&site)
        .await;
    Mock::given(path("/free"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<p>free</p>", "text/html"))
        .mount(&site)
        .await;
    let limiter = Arc::new(BatchRetryTurns {
        held,
        hits: hits.clone(),
        sources_started,
        release: Default::default(),
    });
    let mut config = CrawlConfig::builder().allow_private_networks(true).build();
    config.max_concurrent = Some(2);
    config.respect_robots_txt = false;
    config.retry_count = 1;
    config.retry_initial_delay_ms = 0;
    config.retry_max_delay_ms = 0;
    let mut engine = CrawlEngine::builder().config(config).build().unwrap();
    engine.rate_limiter = limiter.clone();
    let urls = [
        format!("{}/source/0", site.uri()),
        format!("{}/source/1", site.uri().replace("127.0.0.1", "localhost")),
        format!("{}/free", site.uri().replace("127.0.0.1", "localhost.")),
    ];
    let running = tokio::spawn(run_post_429_batch(kind, engine, urls));
    let free = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if site
                .received_requests()
                .await
                .unwrap()
                .iter()
                .any(|request| request.url.path() == "/free")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    if free.is_err() {
        running.abort();
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        panic!("the free batch host never starts while both post-429 retry host turns wait");
    }
    limiter.held.store(false, Ordering::Release);
    limiter.release.notify_waiters();
    let results = tokio::time::timeout(Duration::from_secs(2), running)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(results.len(), 3);
    assert_eq!(results.iter().filter(|success| **success).count(), 1);
    assert_eq!(site.received_requests().await.unwrap().len(), 5);
}

#[tokio::test]
async fn should_cancel_parked_retry_state_and_restore_exact_original_entries() {
    let site = MockServer::start().await;
    let held = Arc::new(AtomicBool::new(false));
    let hits = Arc::new(AtomicUsize::new(0));
    let signal = Arc::clone(&held);
    let count = Arc::clone(&hits);
    Mock::given(wiremock::matchers::path_regex("^/source/"))
        .respond_with(move |_: &wiremock::Request| {
            if count.fetch_add(1, Ordering::SeqCst) == 1 {
                signal.store(true, Ordering::Release);
            }
            ResponseTemplate::new(429)
                .insert_header("retry-after", "0")
                .set_delay(Duration::from_millis(100))
        })
        .mount(&site)
        .await;
    Mock::given(path("/free"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<p>free</p>", "text/html"))
        .mount(&site)
        .await;
    let limiter = Arc::new(RetryTurns {
        pending: Arc::default(),
        held,
        release: Default::default(),
    });
    let mut config = CrawlConfig::builder().allow_private_networks(true).build();
    config.max_concurrent = Some(2);
    config.respect_robots_txt = false;
    config.retry_count = 1;
    config.retry_initial_delay_ms = 0;
    config.retry_max_delay_ms = 0;
    let mut engine = CrawlEngine::builder().config(config).build().unwrap();
    engine.rate_limiter = limiter.clone();
    for index in 0..3 {
        let url = if index < 2 {
            format!("{}/source/{index}", site.uri())
        } else {
            format!("{}/free", site.uri().replace("127.0.0.1", "localhost"))
        };
        engine
            .frontier
            .push(FrontierEntry {
                original_url: None,
                url,
                depth: 0,
                doc_depth: index as u32,
                priority: index as f64,
            })
            .await
            .unwrap();
    }
    let frontier = engine.frontier.clone();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(8);
    let running = tokio::spawn(async move {
        let mut state = CrawlState::new(3, true);
        state.frontier_pending = 3;
        let robots = RobotsOutcome::AllowAll;
        let tx = Some(sender);
        let context = LoopContext {
            exclude_regexes: Arc::from([]),
            include_regexes: Arc::from([]),
            robots: &robots,
            base_host: "127.0.0.1",
            base_host_suffix: ".127.0.0.1",
            max_depth: 10,
            max_pages: 3,
            start_time: Instant::now(),
            tx: &tx,
        };
        engine.run_crawl_loop(&mut state, &mut None, &context).await.unwrap();
        state
    });
    let free = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if site
                .received_requests()
                .await
                .unwrap()
                .iter()
                .any(|request| request.url.path() == "/free")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    if free.is_err() {
        running.abort();
        assert_eq!(
            hits.load(Ordering::SeqCst),
            2,
            "both original attempts reached the server"
        );
        panic!("the free frontier host never starts while both configured retry hops wait");
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(event) = receiver.recv().await {
            if let CrawlEvent::Page { result: page } = event {
                assert!(page.url.ends_with("/free"));
                break;
            }
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while limiter.pending.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    drop(receiver);
    let state = tokio::time::timeout(Duration::from_secs(2), running)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.pages_processed(), 1);
    assert_eq!(state.pages_failed, 0);
    assert_eq!(limiter.pending.load(Ordering::SeqCst), 0);
    let entries = frontier.pop_batch(3).await.unwrap();
    assert_eq!(entries.len(), 2);
    let restored = entries
        .into_iter()
        .map(|entry| serde_json::to_value(entry).unwrap())
        .collect::<Vec<_>>();
    for index in 0..2 {
        let expected = FrontierEntry {
            url: format!("{}/source/{index}", site.uri()),
            original_url: None,
            depth: 0,
            doc_depth: index as u32,
            priority: index as f64,
        };
        assert!(restored.contains(&serde_json::to_value(expected).unwrap()));
    }
    assert_eq!(site.received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn should_keep_inner_wire_activity_leases_until_all_live_hops_finish() {
    use futures::{StreamExt, stream::FuturesUnordered};
    let site = Arc::new(MockServer::start().await);
    Mock::given(wiremock::matchers::path_regex("^/child/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw("<p>child</p>", "text/html")
                .set_delay(Duration::from_secs(10)),
        )
        .mount(&site)
        .await;
    Mock::given(path("/free"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<p>free</p>", "text/html"))
        .mount(&site)
        .await;
    let seeds = Arc::new(Semaphore::new(2));
    let third_started = Arc::new(AtomicBool::new(false));
    let jobs = FuturesUnordered::new();
    for family in 0..3 {
        let mut config = CrawlConfig::builder().allow_private_networks(true).build();
        config.max_concurrent = Some(2);
        config.respect_robots_txt = false;
        config.rate_limit_ms = Some(0);
        let mut engine = CrawlEngine::builder().config(config).build().unwrap();
        engine.crawl_schedule = Some(super::super::cooperative::CooperativeContext::new(seeds.clone()));
        let count = if family == 2 { 1 } else { 2 };
        for child in 0..count {
            let url = if family == 2 {
                format!("{}/free", site.uri())
            } else {
                format!("{}/child/{family}/{child}", site.uri())
            };
            engine
                .frontier
                .push(FrontierEntry {
                    original_url: None,
                    url,
                    depth: 0,
                    doc_depth: 0,
                    priority: 1.0,
                })
                .await
                .unwrap();
        }
        let site = site.clone();
        let third_started = third_started.clone();
        jobs.push(async move {
            if family == 2 {
                while site.received_requests().await.unwrap().len() < 4 {
                    tokio::task::yield_now().await;
                }
                third_started.store(true, Ordering::Release);
            }
            let mut state = CrawlState::new(count, false);
            state.frontier_pending = count;
            let robots = RobotsOutcome::AllowAll;
            let tx = None;
            let context = LoopContext {
                exclude_regexes: Arc::from([]),
                include_regexes: Arc::from([]),
                robots: &robots,
                base_host: "127.0.0.1",
                base_host_suffix: ".127.0.0.1",
                max_depth: 10,
                max_pages: count,
                start_time: Instant::now(),
                tx: &tx,
            };
            engine.run_crawl_loop(&mut state, &mut None, &context).await.unwrap();
            state
        });
    }
    let running = tokio::spawn(async move { jobs.collect::<Vec<_>>().await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while !third_started.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("four real inner requests must occupy both active crawl contexts");
    assert_eq!(seeds.available_permits(), 0);
    let requests = site.received_requests().await.unwrap();
    assert_eq!(requests.len(), 4);
    for expected in ["/child/0/0", "/child/0/1", "/child/1/0", "/child/1/1"] {
        assert!(requests.iter().any(|request| request.url.path() == expected));
    }
    running.abort();
    assert!(matches!(running.await, Err(error) if error.is_cancelled()));
    tokio::time::timeout(Duration::from_secs(2), async {
        while seeds.available_permits() != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancellation drops all inner activity leases");
    assert_eq!(site.received_requests().await.unwrap().len(), 4);
}

#[tokio::test]
async fn should_stop_a_stream_waiting_for_a_crawl_activity_slot_without_starting_wire_work() {
    let site = MockServer::start().await;
    let seeds = Arc::new(Semaphore::new(1));
    let occupied = seeds.clone().acquire_owned().await.unwrap();
    let schedule = super::super::cooperative::CooperativeContext::new(seeds.clone());
    let inner = Arc::new(crate::defaults::InMemoryFrontier::new());
    let popped = Arc::new(AtomicUsize::new(0));
    let mut config = CrawlConfig::builder().allow_private_networks(true).build();
    config.max_concurrent = Some(2);
    config.respect_robots_txt = false;
    let mut engine = CrawlEngine::builder()
        .config(config)
        .frontier(CountingLegacyFrontier {
            inner: inner.clone(),
            popped: popped.clone(),
        })
        .build()
        .unwrap();
    engine.crawl_schedule = Some(schedule.clone());
    let original = FrontierEntry {
        url: format!("{}/original%2fpath", site.uri()),
        original_url: Some(format!("{}/original%2fpath#source", site.uri())),
        depth: 0,
        doc_depth: 7,
        priority: 0.375,
    };
    engine.frontier.push(original.clone()).await.unwrap();
    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    let mut running = tokio::spawn(async move {
        let mut state = CrawlState::new(1, true);
        state.frontier_pending = 1;
        let robots = RobotsOutcome::AllowAll;
        let tx = Some(sender);
        let context = LoopContext {
            exclude_regexes: Arc::from([]),
            include_regexes: Arc::from([]),
            robots: &robots,
            base_host: "127.0.0.1",
            base_host_suffix: ".127.0.0.1",
            max_depth: 10,
            max_pages: 1,
            start_time: Instant::now(),
            tx: &tx,
        };
        engine.run_crawl_loop(&mut state, &mut None, &context).await.unwrap();
        state
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while popped.load(Ordering::SeqCst) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the frontier entry must actually be admitted while its activity slot is unavailable");
    drop(receiver);
    let stopped = tokio::time::timeout(Duration::from_secs(1), &mut running).await;
    if stopped.is_err() {
        running.abort();
        let _ = running.await;
        assert_eq!(site.received_requests().await.unwrap().len(), 0);
        panic!("receiver closure did not cancel the unavailable activity slot wait");
    }
    let state = stopped.unwrap().unwrap();
    assert_eq!(state.pages_processed(), 0);
    assert_eq!(state.pages_failed, 0);
    let restored = inner.pop_batch(2).await.unwrap();
    assert_eq!(restored.len(), 1);
    assert_eq!(
        serde_json::to_value(&restored[0]).unwrap(),
        serde_json::to_value(original).unwrap()
    );
    assert_eq!(site.received_requests().await.unwrap().len(), 0);
    assert_eq!(seeds.available_permits(), 0);
    drop(occupied);
    let activity = tokio::time::timeout(Duration::from_secs(1), schedule.activity())
        .await
        .unwrap()
        .unwrap();
    schedule.complete();
    drop(activity);
    assert_eq!(seeds.available_permits(), 1);
}
