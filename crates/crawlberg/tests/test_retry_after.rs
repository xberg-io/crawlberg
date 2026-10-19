use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crawlberg::{BrowserMode, CrawlConfig, CrawlError, SsrfPolicy, crawl, create_engine, scrape};
use wiremock::matchers::path;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

struct Throttled {
    status: u16,
    retry_after: String,
    throttled_requests: usize,
    arrivals: Arc<Mutex<Vec<Instant>>>,
}

impl Respond for Throttled {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        let mut arrivals = self.arrivals.lock().expect("arrivals");
        arrivals.push(Instant::now());
        if arrivals.len() <= self.throttled_requests {
            ResponseTemplate::new(self.status).insert_header("Retry-After", self.retry_after.as_str())
        } else {
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/html")
                .set_body_string("<p>served after the wait</p>")
        }
    }
}

fn config(retry_count: usize) -> CrawlConfig {
    let mut config = CrawlConfig {
        ssrf: SsrfPolicy {
            deny_private: false,
            ..SsrfPolicy::default()
        },
        ..CrawlConfig::default()
    };
    config.browser.mode = BrowserMode::Never;
    config.respect_robots_txt = false;
    config.retry_count = retry_count;
    config.retry_initial_delay_ms = 10;
    config
}

async fn serve(status: u16, retry_after: &str, throttled_requests: usize) -> (MockServer, Arc<Mutex<Vec<Instant>>>) {
    let server = MockServer::start().await;
    let arrivals = Arc::new(Mutex::new(Vec::new()));
    Mock::given(path("/page"))
        .respond_with(Throttled {
            status,
            retry_after: retry_after.to_string(),
            throttled_requests,
            arrivals: Arc::clone(&arrivals),
        })
        .mount(&server)
        .await;
    (server, arrivals)
}

async fn assert_retry_waits_for_retry_after(status: u16, retry_after: &str) {
    let (server, arrivals) = serve(status, retry_after, 1).await;
    let engine = create_engine(Some(config(3))).expect("engine");
    scrape(&engine, &format!("{}/page", server.uri()))
        .await
        .expect("the retry after the wait must succeed");
    let arrivals = arrivals.lock().expect("arrivals");
    assert_eq!(arrivals.len(), 2, "one throttled request and one retry");
    let gap = arrivals[1] - arrivals[0];
    assert!(
        gap >= Duration::from_millis(990),
        "a {status} with Retry-After: {retry_after} must delay the retry by at least a second, retried after {gap:?}"
    );
}

#[tokio::test]
async fn should_wait_for_delta_seconds_retry_after_before_retrying_a_429() {
    assert_retry_waits_for_retry_after(429, "1").await;
}

#[tokio::test]
async fn should_wait_for_delta_seconds_retry_after_before_retrying_a_503() {
    assert_retry_waits_for_retry_after(503, "1").await;
}

#[tokio::test]
async fn should_wait_for_an_http_date_retry_after_before_retrying() {
    // ~keep HTTP-dates truncate to whole seconds, so asking for 4s ahead keeps the wait above 1s even when the request is slow to arrive.
    let date = chrono::Utc::now() + chrono::Duration::seconds(4);
    assert_retry_waits_for_retry_after(429, &date.format("%a, %d %b %Y %H:%M:%S GMT").to_string()).await;
}

#[tokio::test]
async fn should_give_up_instead_of_retrying_early_when_retry_after_exceeds_the_cap() {
    let (server, arrivals) = serve(429, "5", usize::MAX).await;
    let mut config = config(3);
    config.retry_max_delay_ms = 500;
    let engine = create_engine(Some(config)).expect("engine");
    let started = Instant::now();
    let error = scrape(&engine, &format!("{}/page", server.uri()))
        .await
        .expect_err("a Retry-After beyond the cap must not be retried");
    assert!(matches!(error, CrawlError::RateLimited { .. }), "{error}");
    assert_eq!(arrivals.lock().expect("arrivals").len(), 1, "no retry may be sent");
    assert!(started.elapsed() < Duration::from_secs(4), "giving up must not wait");
}

#[tokio::test]
async fn should_wait_for_retry_after_before_retrying_a_429_in_a_crawl() {
    let (server, arrivals) = serve(429, "1", 1).await;
    let mut config = config(3);
    config.max_depth = Some(0);
    let engine = create_engine(Some(config)).expect("engine");
    let result = crawl(&engine, &format!("{}/page", server.uri()))
        .await
        .expect("the crawl must succeed after the wait");
    assert_eq!(result.pages.len(), 1, "the retried page must be returned");
    let arrivals = arrivals.lock().expect("arrivals");
    assert_eq!(arrivals.len(), 2, "one throttled request and one retry");
    let gap = arrivals[1] - arrivals[0];
    assert!(gap >= Duration::from_millis(990), "the crawl retried after {gap:?}");
}
