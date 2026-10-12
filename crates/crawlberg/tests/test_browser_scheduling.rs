#![cfg(any(feature = "browser", feature = "browser-native"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use crawlberg::traits::RateLimiter;
use crawlberg::{BrowserMode, CrawlConfig, CrawlEngine, CrawlError, CrawlEvent};
#[cfg(feature = "browser")]
use crawlberg::{BrowserPool, BrowserPoolConfig};
use tokio::sync::Notify;
use tokio_stream::StreamExt;
use wiremock::matchers::path;
use wiremock::{Mock, MockServer, ResponseTemplate};

const PENDING_TIMEOUT: Duration = Duration::from_secs(10);
const FREE_TIMEOUT: Duration = Duration::from_secs(5);
const BATCH_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Default)]
struct AdmissionProbe {
    slow_requests: AtomicUsize,
    free_requests: AtomicUsize,
    slow_turns: AtomicUsize,
    other_turns: AtomicUsize,
    target_wait_reached: AtomicBool,
    target_wait: Notify,
    released: AtomicBool,
    parked: Notify,
    release: Notify,
    free_seen: Notify,
}

#[derive(Clone)]
struct RecordedBrowserAdmission(Arc<AdmissionProbe>, &'static str);

#[async_trait::async_trait]
impl RateLimiter for RecordedBrowserAdmission {
    async fn acquire(&self, host: &str) -> Result<(), CrawlError> {
        let probe = &self.0;
        if host == self.1 {
            probe.slow_turns.fetch_add(1, Ordering::SeqCst);
            if probe.slow_requests.load(Ordering::SeqCst) > 0 && !probe.released.load(Ordering::SeqCst) {
                probe.target_wait_reached.store(true, Ordering::SeqCst);
                probe.target_wait.notify_waiters();
                probe.parked.notify_one();
                probe.release.notified().await;
            }
        // ~keep The free source-host turn starts only after the redirected browser acquire is pending.
        } else if probe.other_turns.fetch_add(1, Ordering::SeqCst) > 0 {
            loop {
                let pending = probe.target_wait.notified();
                if probe.target_wait_reached.load(Ordering::SeqCst) {
                    break;
                }
                pending.await;
            }
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

async fn fixture(probe: &Arc<AdmissionProbe>) -> MockServer {
    let site = MockServer::start().await;
    let slow = probe.clone();
    Mock::given(path("/slow"))
        .respond_with(move |_: &wiremock::Request| {
            let status = if slow.slow_requests.fetch_add(1, Ordering::SeqCst) == 0 {
                429
            } else {
                200
            };
            ResponseTemplate::new(status).set_body_raw("<p>retried</p>", "text/html")
        })
        .mount(&site)
        .await;
    let free = probe.clone();
    Mock::given(path("/free"))
        .respond_with(move |_: &wiremock::Request| {
            free.free_requests.fetch_add(1, Ordering::SeqCst);
            free.free_seen.notify_one();
            ResponseTemplate::new(200).set_body_raw("<p>ready</p>", "text/html")
        })
        .mount(&site)
        .await;
    site
}

#[derive(Clone, Copy)]
enum BatchKind {
    Scrape,
    #[cfg(feature = "browser")]
    Screenshot,
    Crawl,
    Stream,
}

async fn execute_scrapes(engine: CrawlEngine, slow: String, free: String, screenshot: bool) {
    let results = engine.batch_scrape(&[&slow, &free]).await;
    assert_eq!(results.len(), 2);
    for (_, result) in results {
        let result = result.expect("scrape succeeds");
        assert_eq!(result.status_code, 200);
        if screenshot {
            let png = result
                .screenshot
                .expect("screenshot remains attached to the resumed result");
            assert!(png.len() > 8);
            assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        }
    }
}

async fn execute(engine: CrawlEngine, slow: String, free: String, kind: BatchKind) {
    match kind {
        BatchKind::Scrape => execute_scrapes(engine, slow, free, false).await,
        #[cfg(feature = "browser")]
        BatchKind::Screenshot => execute_scrapes(engine, slow, free, true).await,
        BatchKind::Crawl => {
            let results = engine.batch_crawl(&[&slow, &free]).await;
            assert_eq!(results.len(), 2);
            for (_, result) in results {
                let result = result.expect("crawl succeeds");
                assert_eq!(result.error, None);
                assert_eq!(result.pages.len(), 1);
                assert_eq!(result.pages[0].status_code, 200);
            }
        }
        BatchKind::Stream => {
            let events = engine.batch_crawl_stream(&[&slow, &free]).collect::<Vec<_>>().await;
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, CrawlEvent::Page { .. }))
                    .count(),
                2
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, CrawlEvent::Complete { .. }))
                    .count(),
                2
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, CrawlEvent::Error { .. }))
                    .count(),
                0
            );
        }
    }
}

async fn assert_browser_retry_wait_leaves_room_for_a_ready_host(kind: BatchKind) {
    let probe = Arc::new(AdmissionProbe::default());
    let site = fixture(&probe).await;
    let mut config = CrawlConfig::builder()
        .allow_private_networks(true)
        .max_pages(1)
        .max_depth(1)
        .max_concurrent(1)
        .build();
    #[cfg(feature = "browser")]
    {
        config.capture_screenshot = matches!(kind, BatchKind::Screenshot);
    }
    config.browser.mode = BrowserMode::Always;
    #[cfg(all(feature = "browser-native", not(feature = "browser")))]
    {
        config.browser.backend = crawlberg::BrowserBackend::Native;
    }
    config.browser.session_affinity = false;
    config.retry_count = 1;
    config.retry_codes = vec![429];
    config.retry_initial_delay_ms = 0;
    config.retry_max_delay_ms = 0;
    config.respect_robots_txt = false;
    #[cfg(feature = "browser")]
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    let builder = CrawlEngine::builder()
        .config(config)
        .rate_limiter(RecordedBrowserAdmission(probe.clone(), "127.0.0.1"));
    #[cfg(feature = "browser")]
    let builder = builder.with_browser_pool(pool.clone());
    let engine = builder.build().expect("engine");
    let slow = format!("{}/slow", site.uri());
    let free = format!("{}/free", site.uri().replace("127.0.0.1", "localhost"));
    let running = tokio::spawn(execute(engine, slow, free, kind));
    let parked = tokio::time::timeout(PENDING_TIMEOUT, probe.parked.notified()).await;
    let slow_before_release = probe.slow_requests.load(Ordering::SeqCst);
    let free_before_release = tokio::time::timeout(FREE_TIMEOUT, probe.free_seen.notified()).await;
    probe.released.store(true, Ordering::SeqCst);
    probe.release.notify_one();
    let complete = tokio::time::timeout(BATCH_TIMEOUT, running).await;
    #[cfg(feature = "browser")]
    pool.shutdown().await;
    assert!(
        parked.is_ok(),
        "the actual post-429 browser turn must reach the limiter"
    );
    assert_eq!(slow_before_release, 1);
    complete.expect("batch completes").expect("batch joins");
    assert!(
        free_before_release.is_ok(),
        "the ready browser host must run before the delayed retry is released"
    );
    assert_eq!(probe.slow_requests.load(Ordering::SeqCst), 2);
    assert_eq!(probe.free_requests.load(Ordering::SeqCst), 1);
    assert_eq!(probe.slow_turns.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn should_scrape_a_ready_browser_host_while_another_hosts_retry_turn_waits() {
    assert_browser_retry_wait_leaves_room_for_a_ready_host(BatchKind::Scrape).await;
}

#[tokio::test]
async fn should_crawl_a_ready_browser_host_while_another_hosts_retry_turn_waits() {
    assert_browser_retry_wait_leaves_room_for_a_ready_host(BatchKind::Crawl).await;
}

#[tokio::test]
async fn should_stream_a_ready_browser_host_while_another_hosts_retry_turn_waits() {
    assert_browser_retry_wait_leaves_room_for_a_ready_host(BatchKind::Stream).await;
}

#[cfg(feature = "browser")]
#[tokio::test]
async fn should_keep_screenshots_while_a_browser_retry_turn_leaves_room_for_a_ready_host() {
    assert_browser_retry_wait_leaves_room_for_a_ready_host(BatchKind::Screenshot).await;
}

async fn assert_redirect_fallback_wait_leaves_room_for_a_ready_host(kind: BatchKind) {
    let probe = Arc::new(AdmissionProbe::default());
    let site = MockServer::start().await;
    let target = format!("{}/target", site.uri().replace("127.0.0.1", "localhost"));
    Mock::given(path("/source"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", &target))
        .mount(&site)
        .await;
    let slow = probe.clone();
    Mock::given(path("/target"))
        .respond_with(move |_: &wiremock::Request| {
            if slow.slow_requests.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(403).set_body_string("blocked")
            } else {
                ResponseTemplate::new(200).set_body_raw("<p>browser fallback</p>", "text/html")
            }
        })
        .mount(&site)
        .await;
    let free = probe.clone();
    Mock::given(path("/free"))
        .respond_with(move |_: &wiremock::Request| {
            free.free_requests.fetch_add(1, Ordering::SeqCst);
            free.free_seen.notify_one();
            ResponseTemplate::new(200).set_body_raw("<p>ready</p>", "text/html")
        })
        .mount(&site)
        .await;
    let mut config = CrawlConfig::builder()
        .allow_private_networks(true)
        .max_pages(1)
        .max_depth(1)
        .max_concurrent(1)
        .build();
    config.browser.mode = BrowserMode::Auto;
    #[cfg(all(feature = "browser-native", not(feature = "browser")))]
    {
        config.browser.backend = crawlberg::BrowserBackend::Native;
    }
    config.user_agent = Some("scheduling-browser-agent".into());
    config.user_agents = vec!["scheduling-http-agent".into()];
    config.browser.session_affinity = false;
    config.respect_robots_txt = false;
    #[cfg(feature = "browser")]
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    let builder = CrawlEngine::builder()
        .config(config)
        .rate_limiter(RecordedBrowserAdmission(probe.clone(), "localhost"));
    #[cfg(feature = "browser")]
    let builder = builder.with_browser_pool(pool.clone());
    let engine = builder.build().expect("engine");
    let source = format!("{}/source", site.uri());
    let free = format!("{}/free", site.uri());
    let running = tokio::spawn(execute(engine, source, free, kind));
    let parked = tokio::time::timeout(PENDING_TIMEOUT, probe.parked.notified()).await;
    let target_before_release = probe.slow_requests.load(Ordering::SeqCst);
    let free_before_release = tokio::time::timeout(FREE_TIMEOUT, probe.free_seen.notified()).await;
    probe.released.store(true, Ordering::SeqCst);
    probe.release.notify_one();
    let complete = tokio::time::timeout(BATCH_TIMEOUT, running).await;
    #[cfg(feature = "browser")]
    pool.shutdown().await;
    assert!(
        parked.is_ok(),
        "redirect target browser fallback must reach its actual limiter"
    );
    assert_eq!(target_before_release, 1);
    complete.expect("batch completes").expect("batch joins");
    assert!(
        free_before_release.is_ok(),
        "the source host remains ready while its redirect target waits"
    );
    assert_eq!(probe.slow_requests.load(Ordering::SeqCst), 2);
    assert_eq!(probe.free_requests.load(Ordering::SeqCst), 1);
    assert_eq!(probe.slow_turns.load(Ordering::SeqCst), 2);
    let requests = site.received_requests().await.expect("recorded wire requests");
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.url.path() == "/source")
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.url.path() == "/target"
                && request.headers.get("user-agent").and_then(|agent| agent.to_str().ok())
                    == Some("scheduling-browser-agent"))
            .count(),
        1
    );
}

#[tokio::test]
async fn should_scrape_a_ready_source_host_while_its_redirect_targets_browser_fallback_waits() {
    assert_redirect_fallback_wait_leaves_room_for_a_ready_host(BatchKind::Scrape).await;
}

#[tokio::test]
async fn should_crawl_a_ready_source_host_while_its_redirect_targets_browser_fallback_waits() {
    assert_redirect_fallback_wait_leaves_room_for_a_ready_host(BatchKind::Crawl).await;
}

#[tokio::test]
async fn should_stream_a_ready_source_host_while_its_redirect_targets_browser_fallback_waits() {
    assert_redirect_fallback_wait_leaves_room_for_a_ready_host(BatchKind::Stream).await;
}
