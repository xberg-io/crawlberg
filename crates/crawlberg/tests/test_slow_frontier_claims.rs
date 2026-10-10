use std::time::Duration;

use async_trait::async_trait;
use crawlberg::traits::{Frontier, FrontierEntry};
use crawlberg::{CrawlConfig, CrawlEngine, CrawlError, InMemoryFrontier};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

struct SlowFrontier {
    inner: InMemoryFrontier,
    delay: Duration,
}

#[async_trait]
impl Frontier for SlowFrontier {
    async fn push(&self, entry: FrontierEntry) -> Result<(), CrawlError> {
        self.inner.push(entry).await
    }

    async fn pop(&self) -> Result<Option<FrontierEntry>, CrawlError> {
        self.inner.pop().await
    }

    async fn len(&self) -> Result<usize, CrawlError> {
        self.inner.len().await
    }

    async fn is_seen(&self, url: &str) -> Result<bool, CrawlError> {
        let seen = self.inner.is_seen(url).await?;
        tokio::time::sleep(self.delay).await;
        Ok(seen)
    }

    async fn mark_seen(&self, url: &str) -> Result<(), CrawlError> {
        self.inner.mark_seen(url).await
    }
}

async fn redirect_site() -> MockServer {
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(|request: &Request| match request.url.path() {
            "/" => {
                let links: String = (0..8).map(|index| format!("<a href='/x{index}'>link</a>")).collect();
                ResponseTemplate::new(200).set_body_raw(format!("<html><body>{links}</body></html>"), "text/html")
            }
            "/t" => ResponseTemplate::new(200).set_body_raw("<p>shared target words</p>", "text/html"),
            path if path.starts_with("/x") => ResponseTemplate::new(302).insert_header("location", "/t"),
            _ => ResponseTemplate::new(404),
        })
        .mount(&site)
        .await;
    site
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn should_fetch_and_report_one_redirect_target_with_an_unchanged_slow_frontier() {
    for delay in [Duration::from_millis(500), Duration::ZERO] {
        let site = redirect_site().await;
        let mut config = CrawlConfig::builder()
            .allow_private_networks(true)
            .respect_robots_txt(false)
            .stay_on_domain(true)
            .max_depth(1)
            .max_concurrent(8)
            .max_pages(50)
            .build();
        config.rate_limit_ms = Some(0);
        let engine = CrawlEngine::builder()
            .config(config)
            .frontier(SlowFrontier {
                inner: InMemoryFrontier::new(),
                delay,
            })
            .build()
            .expect("engine");
        let result = engine.crawl(&format!("{}/", site.uri())).await.expect("crawl");
        let requests = site.received_requests().await.expect("recorded requests");
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.url.path().starts_with("/x"))
                .count(),
            8
        );
        assert_eq!(
            requests.iter().filter(|request| request.url.path() == "/t").count(),
            1,
            "delay {delay:?}"
        );
        assert_eq!(
            result
                .pages
                .iter()
                .filter(|page| page.final_url.ends_with("/t"))
                .count(),
            1,
            "delay {delay:?}"
        );
    }
}
