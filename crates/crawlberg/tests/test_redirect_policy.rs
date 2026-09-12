//! Every URL the seed's redirect chain reaches is judged before it is requested.
//!
//! The seed is fetched once, by resolving its redirects. The path filters and robots.txt
//! were applied to the URL the chain lands on only after the whole chain had been fetched,
//! so a disallowed or excluded target still received its request, and its `Crawl-delay`
//! reached the rate limiter after the requests it governs.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use crawlberg::traits::RateLimiter;
use crawlberg::{CrawlConfig, CrawlEngine, CrawlError, CrawlResult, crawl, create_engine};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ALLOW_ALL: &str = "User-agent: *\nAllow: /\n";

async fn mount_robots(mock: &MockServer, body: &str) {
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body.to_owned()))
        .mount(mock)
        .await;
}

async fn mount_page(mock: &MockServer, at: &str, html: &str) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(html.to_owned())
                .append_header("content-type", "text/html"),
        )
        .mount(mock)
        .await;
}

async fn mount_redirect(mock: &MockServer, at: &str, to: &str) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(ResponseTemplate::new(302).append_header("location", to.to_owned()))
        .mount(mock)
        .await;
}

/// Every path the server was asked for, in arrival order.
async fn request_log(mock: &MockServer) -> Vec<String> {
    mock.received_requests()
        .await
        .expect("the mock server records its requests")
        .iter()
        .map(|request| request.url.path().to_owned())
        .collect()
}

async fn crawl_seed(config: CrawlConfig, seed: &str) -> CrawlResult {
    let engine = create_engine(Some(config)).expect("engine builds");
    crawl(&engine, seed).await.expect("crawl runs")
}

fn config() -> crawlberg::CrawlConfigBuilder {
    CrawlConfig::builder()
        .respect_robots_txt(true)
        .allow_private_networks(true)
        .max_pages(10)
}

#[tokio::test]
async fn should_never_request_a_same_origin_redirect_target_that_exclude_paths_names() {
    let mock = MockServer::start().await;
    mount_robots(&mock, ALLOW_ALL).await;
    mount_redirect(&mock, "/", "/private.html").await;
    mount_page(&mock, "/private.html", "<html><body>private</body></html>").await;

    let config = config().exclude_paths(vec!["/private".to_owned()]).build();
    let result = crawl_seed(config, &format!("{}/", mock.uri())).await;

    let log = request_log(&mock).await;
    assert!(
        !log.iter().any(|entry| entry == "/private.html"),
        "exclude_paths names the redirect target, so it must never be requested, got {log:?}"
    );
    assert_eq!(result.pages.len(), 0, "an excluded redirect target yields no page");
}

#[tokio::test]
async fn should_never_request_a_same_origin_redirect_target_that_robots_txt_disallows() {
    let mock = MockServer::start().await;
    mount_robots(&mock, "User-agent: *\nDisallow: /private.html\n").await;
    mount_redirect(&mock, "/", "/private.html").await;
    mount_page(&mock, "/private.html", "<html><body>private</body></html>").await;

    let result = crawl_seed(config().build(), &format!("{}/", mock.uri())).await;

    let log = request_log(&mock).await;
    assert!(
        !log.iter().any(|entry| entry == "/private.html"),
        "robots.txt disallows the redirect target, so it must never be requested, got {log:?}"
    );
    assert_eq!(result.pages.len(), 0, "a disallowed redirect target yields no page");
}

#[tokio::test]
async fn should_read_the_targets_robots_txt_before_requesting_a_cross_origin_redirect_target() {
    let target = MockServer::start().await;
    mount_robots(&target, "User-agent: *\nDisallow: /landing.html\n").await;
    mount_page(&target, "/landing.html", "<html><body>landing</body></html>").await;

    let seed = MockServer::start().await;
    mount_robots(&seed, ALLOW_ALL).await;
    mount_redirect(&seed, "/", &format!("{}/landing.html", target.uri())).await;

    let result = crawl_seed(config().build(), &format!("{}/", seed.uri())).await;

    assert_eq!(
        request_log(&target).await,
        vec!["/robots.txt".to_owned()],
        "the target's robots.txt is the only request its origin may receive"
    );
    assert_eq!(result.pages.len(), 0, "a disallowed redirect target yields no page");
}

#[tokio::test]
async fn should_crawl_a_cross_origin_redirect_target_its_own_robots_txt_allows() {
    let target = MockServer::start().await;
    mount_robots(&target, ALLOW_ALL).await;
    mount_page(&target, "/landing.html", "<html><body>landing</body></html>").await;

    let seed = MockServer::start().await;
    mount_robots(&seed, "User-agent: *\nDisallow: /landing.html\n").await;
    mount_redirect(&seed, "/", &format!("{}/landing.html", target.uri())).await;

    let result = crawl_seed(config().build(), &format!("{}/", seed.uri())).await;

    assert_eq!(
        request_log(&target).await,
        vec!["/robots.txt".to_owned(), "/landing.html".to_owned()],
        "the target's own file allows the page, so the seed's rules must not block it"
    );
    assert_eq!(result.pages.len(), 1, "the redirect target is the crawl's one page");
}

/// The rate-limiter calls a crawl made, in order.
///
/// ~keep `CrawlEngineBuilder::rate_limiter` takes the limiter by value and wraps it in an
/// ~keep `Arc<dyn RateLimiter>` the test cannot reach again, so the log is cloned out first.
#[derive(Clone, Default)]
struct LimiterCalls(Arc<Mutex<Vec<String>>>);

impl LimiterCalls {
    fn record(&self, event: String) {
        self.0.lock().expect("the call log must not be poisoned").push(event);
    }

    fn events(&self) -> Vec<String> {
        self.0.lock().expect("the call log must not be poisoned").clone()
    }
}

/// Records the order of `set_crawl_delay` and `acquire` without waiting.
///
/// ~keep Waiting is what the real limiter does with a published delay; the ordering is what
/// ~keep decides whether it has one to wait on. Recording it keeps the test off the clock.
struct RecordingRateLimiter(LimiterCalls);

#[async_trait]
impl RateLimiter for RecordingRateLimiter {
    async fn acquire(&self, _domain: &str) -> Result<(), CrawlError> {
        self.0.record("acquire".to_owned());
        Ok(())
    }

    async fn record_response(&self, _domain: &str, _status: u16) -> Result<(), CrawlError> {
        Ok(())
    }

    async fn set_crawl_delay(&self, _domain: &str, delay: Duration) -> Result<(), CrawlError> {
        self.0.record(format!("set_crawl_delay {}s", delay.as_secs()));
        Ok(())
    }
}

fn engine_recording_limiter(config: CrawlConfig) -> (CrawlEngine, LimiterCalls) {
    let calls = LimiterCalls::default();
    let engine = CrawlEngine::builder()
        .config(config)
        .rate_limiter(RecordingRateLimiter(calls.clone()))
        .build()
        .expect("engine builds");
    (engine, calls)
}

/// `Crawl-delay` must reach the rate limiter before the request it governs.
///
/// Every request passes `acquire`, and a delay published afterwards throttles nothing that
/// has already gone out.
#[tokio::test]
async fn should_publish_the_seeds_crawl_delay_before_the_seed_request() {
    let mock = MockServer::start().await;
    mount_robots(&mock, "User-agent: *\nCrawl-delay: 3\nAllow: /\n").await;
    mount_page(&mock, "/", "<html><body>seed</body></html>").await;

    let (engine, calls) = engine_recording_limiter(config().build());
    let result = engine.crawl(&format!("{}/", mock.uri())).await.expect("crawl runs");

    assert_eq!(result.pages.len(), 1, "the seed is the one crawled page");
    assert_eq!(
        calls.events(),
        vec!["set_crawl_delay 3s".to_owned(), "acquire".to_owned()],
        "the seed's Crawl-delay must be published before the seed is requested"
    );
}

/// A redirect can leave the seed's origin, and each origin sets its own `Crawl-delay`.
#[tokio::test]
async fn should_publish_each_origins_crawl_delay_before_its_own_request() {
    let target = MockServer::start().await;
    mount_robots(&target, "User-agent: *\nCrawl-delay: 2\nAllow: /\n").await;
    mount_page(&target, "/landing.html", "<html><body>landing</body></html>").await;

    let seed = MockServer::start().await;
    mount_robots(&seed, "User-agent: *\nCrawl-delay: 1\nAllow: /\n").await;
    mount_redirect(&seed, "/", &format!("{}/landing.html", target.uri())).await;

    let (engine, calls) = engine_recording_limiter(config().build());
    let result = engine.crawl(&format!("{}/", seed.uri())).await.expect("crawl runs");

    assert_eq!(result.pages.len(), 1, "the redirect target is the crawl's one page");
    assert_eq!(
        calls.events(),
        vec![
            "set_crawl_delay 1s".to_owned(),
            "acquire".to_owned(),
            "set_crawl_delay 2s".to_owned(),
            "acquire".to_owned(),
        ],
        "each origin's Crawl-delay must be published before that origin is requested"
    );
}
