//! Every URL the seed's redirect chain reaches is judged before it is requested.
//!
//! The seed is fetched once, by resolving its redirects. The path filters and robots.txt
//! were applied to the URL the chain lands on only after the whole chain had been fetched,
//! so a disallowed or excluded target still received its request.

use crawlberg::{CrawlConfig, CrawlResult, crawl, create_engine};
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

// ---------------------------------------------------------------------------------------
// Coverage added alongside the review corrections to the change above.
// ---------------------------------------------------------------------------------------

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crawlberg::traits::RateLimiter;
use crawlberg::{CrawlEngine, CrawlError};

const DISALLOW_PRIVATE: &str = "User-agent: *\nDisallow: /private\n";

/// Records the `Crawl-delay` values published to the limiter, per domain.
///
/// ~keep The log is shared through an `Arc` because `rate_limiter()` takes the limiter by
/// ~keep value and wraps it itself, so the test cannot hold the limiter to read it back.
#[derive(Debug, Clone, Default)]
struct RecordingRateLimiter {
    delays: Arc<Mutex<Vec<(String, Duration)>>>,
}

#[async_trait::async_trait]
impl RateLimiter for RecordingRateLimiter {
    async fn acquire(&self, _domain: &str) -> Result<(), CrawlError> {
        Ok(())
    }

    async fn record_response(&self, _domain: &str, _status: u16) -> Result<(), CrawlError> {
        Ok(())
    }

    async fn set_crawl_delay(&self, domain: &str, delay: Duration) -> Result<(), CrawlError> {
        self.delays
            .lock()
            .expect("delay log mutex must not be poisoned")
            .push((domain.to_owned(), delay));
        Ok(())
    }
}

async fn mount_robots_with_delay(mock: &MockServer, seconds: u64) {
    mount_robots(mock, &format!("User-agent: *\nAllow: /\nCrawl-delay: {seconds}\n")).await;
}

#[tokio::test]
async fn should_publish_each_origins_crawl_delay_even_when_the_chain_leaves_that_origin() {
    // ~keep The seed origin declares a Crawl-delay and then redirects away. Reading robots
    // ~keep only for the origin the chain LANDS on loses the seed's delay entirely, which is
    // ~keep the origin whose rules governed the request that was actually made to it.
    let seed_host = MockServer::start().await;
    let target = MockServer::start().await;

    mount_robots_with_delay(&seed_host, 2).await;
    mount_redirect(&seed_host, "/", &format!("{}/landing.html", target.uri())).await;
    mount_robots(&target, ALLOW_ALL).await;
    mount_page(&target, "/landing.html", "<html><body>landing</body></html>").await;

    let limiter = RecordingRateLimiter::default();
    let engine = CrawlEngine::builder()
        .config(config().build())
        .rate_limiter(limiter.clone())
        .build()
        .expect("engine builds");
    let _ = engine.crawl(&seed_host.uri()).await.expect("crawl runs");

    let seed_domain = url::Url::parse(&seed_host.uri())
        .expect("seed uri parses")
        .host_str()
        .expect("seed uri has a host")
        .to_owned();
    let recorded = limiter
        .delays
        .lock()
        .expect("delay log mutex must not be poisoned")
        .clone();
    assert!(
        recorded
            .iter()
            .any(|(domain, delay)| *domain == seed_domain && *delay == Duration::from_secs(2)),
        "the seed origin's Crawl-delay must reach the limiter before its request goes out, \
         but only these delays were published: {recorded:?}"
    );
}

#[tokio::test]
async fn should_never_request_a_target_reached_through_an_excluded_intermediate_hop() {
    // ~keep Deliberate scope expansion: exclude_paths now judges every hop, not only the URL
    // ~keep the chain lands on. Passing THROUGH an excluded path is still reaching it.
    let mock = MockServer::start().await;
    mount_robots(&mock, ALLOW_ALL).await;
    mount_redirect(&mock, "/", "/private/shim").await;
    mount_redirect(&mock, "/private/shim", "/ok.html").await;
    mount_page(&mock, "/ok.html", "<html><body>ok</body></html>").await;

    let config = config().exclude_paths(vec!["/private".to_owned()]).build();
    let result = crawl_seed(config, &mock.uri()).await;

    let log = request_log(&mock).await;
    assert!(
        !log.iter().any(|p| p == "/private/shim"),
        "the excluded intermediate hop must never be requested, got {log:?}"
    );
    assert!(
        !log.iter().any(|p| p == "/ok.html"),
        "a chain stopped at an excluded hop must not reach its target, got {log:?}"
    );
    assert_eq!(result.pages.len(), 0, "an excluded chain yields no pages");
}

#[tokio::test]
async fn should_judge_every_hop_of_a_three_hop_chain_before_requesting_it() {
    let mock = MockServer::start().await;
    mount_robots(&mock, DISALLOW_PRIVATE).await;
    mount_redirect(&mock, "/", "/one").await;
    mount_redirect(&mock, "/one", "/two").await;
    mount_redirect(&mock, "/two", "/private/deep.html").await;
    mount_page(&mock, "/private/deep.html", "<html><body>deep</body></html>").await;

    let result = crawl_seed(config().build(), &mock.uri()).await;

    let log = request_log(&mock).await;
    assert!(
        !log.iter().any(|p| p == "/private/deep.html"),
        "the disallowed end of a three-hop chain must never be requested, got {log:?}"
    );
    assert_eq!(
        log.iter().filter(|p| *p == "/robots.txt").count(),
        1,
        "one origin, one robots read"
    );
    assert_eq!(result.pages.len(), 0);
}

#[tokio::test]
async fn should_report_the_hops_already_followed_when_a_later_hop_is_refused() {
    // ~keep A refusal at hop 3 still followed two real redirects; reporting redirect_count 0
    // ~keep understates what the crawl did.
    let mock = MockServer::start().await;
    mount_robots(&mock, DISALLOW_PRIVATE).await;
    mount_redirect(&mock, "/", "/one").await;
    mount_redirect(&mock, "/one", "/two").await;
    mount_redirect(&mock, "/two", "/private/end.html").await;

    let result = crawl_seed(config().build(), &mock.uri()).await;

    assert!(
        result.redirect_count >= 2,
        "the hops already followed must be reported, got redirect_count {}",
        result.redirect_count
    );
}

#[tokio::test]
async fn should_fail_closed_when_a_redirect_targets_own_robots_txt_is_unreachable() {
    // ~keep The 1.6.1 fail-closed contract has to survive the move into RedirectPolicy: a
    // ~keep 5xx robots.txt is "unreachable" (RFC 9309 2.3.1.4), not "crawl freely".
    let seed_host = MockServer::start().await;
    let target = MockServer::start().await;

    mount_robots(&seed_host, ALLOW_ALL).await;
    mount_redirect(&seed_host, "/", &format!("{}/landing.html", target.uri())).await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&target)
        .await;
    mount_page(&target, "/landing.html", "<html><body>landing</body></html>").await;

    let result = crawl_seed(config().build(), &seed_host.uri()).await;

    let log = request_log(&target).await;
    assert!(
        !log.iter().any(|p| p == "/landing.html"),
        "a target whose robots.txt is unreachable must not be requested, got {log:?}"
    );
    assert_eq!(result.pages.len(), 0);
    let error = result.error.unwrap_or_default();
    assert!(
        error.contains("robots_unreachable"),
        "a fail-closed refusal must say so, got {error:?}"
    );
}

#[tokio::test]
async fn should_not_read_robots_txt_at_all_when_respect_robots_txt_is_false() {
    let mock = MockServer::start().await;
    mount_robots(&mock, DISALLOW_PRIVATE).await;
    mount_redirect(&mock, "/", "/private/page.html").await;
    mount_page(&mock, "/private/page.html", "<html><body>private</body></html>").await;

    let config = CrawlConfig::builder()
        .respect_robots_txt(false)
        .allow_private_networks(true)
        .max_pages(10)
        .build();
    let result = crawl_seed(config, &mock.uri()).await;

    let log = request_log(&mock).await;
    assert!(
        !log.iter().any(|p| p == "/robots.txt"),
        "robots.txt must not be fetched when the caller opted out, got {log:?}"
    );
    assert_eq!(
        result.pages.len(),
        1,
        "the disallowed page is crawled when robots is off"
    );
}
