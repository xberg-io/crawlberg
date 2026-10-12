use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use crawlberg::InMemoryFrontier;
use crawlberg::traits::{CrawlStats, CrawlStrategy, Frontier, FrontierEntry, RateLimiter};
use crawlberg::{BrowserMode, CrawlConfig, CrawlEngine, CrawlError, CrawlPageResult, HostMatcher, ProxyConfig};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct WaitingHost {
    calls: AtomicUsize,
    pending: Arc<AtomicBool>,
    release: tokio::sync::Notify,
}

#[async_trait::async_trait]
impl RateLimiter for WaitingHost {
    async fn acquire(&self, host: &str) -> Result<(), CrawlError> {
        if host == "slow.example.test" && self.calls.fetch_add(1, Ordering::SeqCst) > 0 {
            self.pending.store(true, Ordering::SeqCst);
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

struct ObservedFrontier {
    inner: InMemoryFrontier,
    pending: Arc<AtomicBool>,
    waiting_entries_popped: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Frontier for ObservedFrontier {
    async fn push(&self, entry: FrontierEntry) -> Result<(), CrawlError> {
        self.inner.push(entry).await
    }
    async fn pop(&self) -> Result<Option<FrontierEntry>, CrawlError> {
        self.inner.pop().await
    }
    async fn pop_batch(&self, n: usize) -> Result<Vec<FrontierEntry>, CrawlError> {
        self.inner.pop_batch(n).await
    }
    async fn pop_batch_excluding_hosts(
        &self,
        n: usize,
        hosts: &[String],
    ) -> Result<Option<Vec<FrontierEntry>>, CrawlError> {
        let entries = self.inner.pop_batch_excluding_hosts(n, hosts).await?;
        if self.pending.load(Ordering::SeqCst) {
            self.waiting_entries_popped.fetch_add(
                entries
                    .as_ref()
                    .unwrap()
                    .iter()
                    .filter(|entry| entry.url.starts_with("http://slow.example.test/"))
                    .count(),
                Ordering::SeqCst,
            );
        }
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

struct StopAfterFree(Arc<AtomicBool>);
impl CrawlStrategy for StopAfterFree {
    fn select_next(&self, entries: &[FrontierEntry]) -> Option<usize> {
        (!entries.is_empty()).then_some(0)
    }
    fn should_continue(&self, _: &CrawlStats) -> bool {
        !self.0.load(Ordering::SeqCst)
    }
    fn on_page_processed(&self, page: &CrawlPageResult) {
        if page.url == "http://free.slow.example.test/free" {
            self.0.store(true, Ordering::SeqCst);
        }
    }
}

#[tokio::test]
async fn should_leave_known_waiting_hosts_in_the_frontier_during_engine_refill() {
    let site = MockServer::start().await;
    let mut body = (0..40)
        .map(|index| format!("<a href=\"/blocked{index}\">blocked</a>"))
        .collect::<String>();
    body.push_str("<a href=\"http://free.slow.example.test/free\">free</a>");
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/html"))
        .mount(&site)
        .await;
    Mock::given(method("GET"))
        .and(path("/free"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<p>free</p>", "text/html"))
        .mount(&site)
        .await;
    let pending = Arc::new(AtomicBool::new(false));
    let popped = Arc::new(AtomicUsize::new(0));
    let mut config = CrawlConfig::builder()
        .allow_private_networks(true)
        .max_concurrent(2)
        .build();
    config.max_depth = Some(1);
    config.max_pages = Some(100);
    config.allow_subdomains = true;
    config.respect_robots_txt = false;
    config.browser.mode = BrowserMode::Never;
    config.ssrf.allowlist = vec![
        HostMatcher::exact("slow.example.test"),
        HostMatcher::exact("free.slow.example.test"),
    ];
    config.proxy = Some(ProxyConfig {
        url: site.uri(),
        username: None,
        password: None,
    });
    let engine = CrawlEngine::builder()
        .config(config)
        .frontier(ObservedFrontier {
            inner: InMemoryFrontier::new(),
            pending: pending.clone(),
            waiting_entries_popped: popped.clone(),
        })
        .rate_limiter(WaitingHost {
            calls: AtomicUsize::new(0),
            pending: pending.clone(),
            release: Default::default(),
        })
        .strategy(StopAfterFree(Arc::default()))
        .build()
        .unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(3), engine.crawl("http://slow.example.test/"))
        .await
        .expect("a free frontier host must progress while its predecessors wait")
        .unwrap();
    assert!(
        pending.load(Ordering::SeqCst),
        "the slow host's real limiter acquisition must have waited"
    );
    assert_eq!(outcome.pages.len(), 2);
    assert_eq!(site.received_requests().await.unwrap().len(), 2);
    assert_eq!(
        popped.load(Ordering::SeqCst),
        0,
        "refill must not drain entries belonging to a host whose acquire waits"
    );
}
