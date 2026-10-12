use std::collections::HashSet;
use std::time::Duration;

use crawlberg::{BrowserMode, CrawlConfig, CrawlEngine, HostMatcher, ProxyConfig, StaticProxyProvider};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn should_finish_many_page_batch_crawls_with_more_seeds_than_slots_and_host_delay() {
    assert_many_page_batch_completes(5, Some(50)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn should_finish_many_page_batch_crawls_with_more_seeds_than_slots_and_default_delay() {
    assert_many_page_batch_completes(10, None).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn should_finish_the_same_many_page_fixture_with_seeds_equal_to_slots() {
    assert_many_page_batch_completes(4, Some(50)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn should_finish_ten_delayed_many_page_batch_crawls_with_four_slots() {
    assert_many_page_batch_completes(10, Some(50)).await;
}

#[tokio::test]
async fn should_fetch_the_ordinary_local_mock_page_with_the_actual_engine() {
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/probe"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<p>local probe</p>", "text/html"))
        .mount(&site)
        .await;
    let mut config = CrawlConfig::builder().allow_private_networks(true).build();
    config.browser.mode = BrowserMode::Never;
    config.respect_robots_txt = false;
    config.proxy_provider = Some(std::sync::Arc::new(StaticProxyProvider::empty()));
    let engine = CrawlEngine::builder().config(config).build().unwrap();
    let page = tokio::time::timeout(Duration::from_secs(2), engine.scrape(&format!("{}/probe", site.uri())))
        .await
        .expect("ordinary direct request must complete")
        .unwrap();
    assert_eq!(page.status_code, 200);
    assert_eq!(site.received_requests().await.unwrap().len(), 1);
}

async fn assert_many_page_batch_completes(seed_count: usize, rate_limit_ms: Option<u64>) {
    const PAGES: usize = 20;
    const CONCURRENCY: usize = 4;
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/probe"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<p>local probe</p>", "text/html"))
        .mount(&site)
        .await;
    let body = (1..PAGES)
        .map(|page| format!("<a href=\"/{page}\">page {page}</a>"))
        .collect::<String>();
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/html"))
        .mount(&site)
        .await;
    for page in 1..PAGES {
        Mock::given(method("GET"))
            .and(path(format!("/{page}")))
            .respond_with(ResponseTemplate::new(200).set_body_raw(format!("<p>page {page}</p>"), "text/html"))
            .mount(&site)
            .await;
    }
    let mut config = CrawlConfig::builder().allow_private_networks(true).build();
    config.max_concurrent = Some(CONCURRENCY);
    config.max_depth = Some(1);
    config.max_pages = Some(PAGES);
    config.rate_limit_ms = rate_limit_ms;
    config.respect_robots_txt = false;
    config.browser.mode = BrowserMode::Never;
    config.ssrf.allowlist = (1..=seed_count)
        .map(|host| HostMatcher::exact(format!("host{host}.example.test")))
        .collect();
    config.proxy = Some(ProxyConfig {
        url: site.uri(),
        username: None,
        password: None,
    });
    let probe_engine = CrawlEngine::builder().config(config.clone()).build().unwrap();
    let engine = CrawlEngine::builder().config(config).build().unwrap();
    let seeds = (1..=seed_count)
        .map(|host| format!("http://host{host}.example.test/"))
        .collect::<Vec<_>>();
    let direct = tokio::time::timeout(
        Duration::from_secs(2),
        probe_engine.scrape(&format!("{}/probe", site.uri())),
    )
    .await
    .expect("ordinary mock URI must work through the exact HTTP client policy")
    .unwrap();
    assert_eq!(direct.status_code, 200);
    assert_eq!(site.received_requests().await.unwrap().len(), 1);
    for seed in &seeds {
        let probe = tokio::time::timeout(Duration::from_secs(2), probe_engine.scrape(&format!("{seed}probe")))
            .await
            .expect("each distinct host must work through the engine's local proxy policy")
            .unwrap();
        assert_eq!(probe.status_code, 200);
    }
    assert_eq!(site.received_requests().await.unwrap().len(), seed_count + 1);
    let urls = seeds.iter().map(String::as_str).collect::<Vec<_>>();
    let completed = tokio::time::timeout(Duration::from_secs(20), engine.batch_crawl(&urls)).await;
    assert!(
        completed.is_ok(),
        "batch with {seed_count} seeds and {CONCURRENCY} slots stalled after {} requests",
        site.received_requests().await.unwrap().len() - seed_count - 1
    );
    let results = completed.unwrap();
    assert_eq!(results.len(), seed_count);
    let mut all_pages = HashSet::new();
    for (seed, outcome) in results {
        assert!(seeds.contains(&seed));
        let crawl = outcome.unwrap();
        assert_eq!(crawl.error, None, "seed {seed}");
        assert_eq!(crawl.pages.len(), PAGES, "seed {seed}");
        for page in crawl.pages {
            assert!(all_pages.insert(page.url), "a page appeared more than once");
        }
    }
    assert_eq!(all_pages.len(), seed_count * PAGES);
    assert_eq!(
        site.received_requests().await.unwrap().len() - seed_count - 1,
        seed_count * PAGES
    );
}
