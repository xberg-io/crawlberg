//! The seed receives one request, and the robots.txt decision comes before it.
//!
//! Redirect resolution used to fetch the seed and throw the body away, after which the
//! crawl loop fetched the same URL again. That first request also ran before robots.txt
//! was read, so a disallowed seed was requested anyway.

use crawlberg::{CrawlConfig, CrawlResult, crawl, create_engine};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const INDEX_HTML: &str = r#"<html><body><a href="/next.html">next</a></body></html>"#;

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

async fn mount_robots(mock: &MockServer, body: &str) {
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body.to_owned()))
        .mount(mock)
        .await;
}

async fn site(robots: &str) -> MockServer {
    let mock = MockServer::start().await;
    mount_robots(&mock, robots).await;
    mount_page(&mock, "/", INDEX_HTML).await;
    mount_page(&mock, "/next.html", "<html><body>next</body></html>").await;
    mock
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

async fn crawl_seed(seed: &str, max_pages: usize) -> CrawlResult {
    let config = CrawlConfig::builder()
        .respect_robots_txt(true)
        .allow_private_networks(true)
        .max_pages(max_pages)
        .build();
    let engine = create_engine(Some(config)).expect("engine builds");
    crawl(&engine, seed).await.expect("crawl runs")
}

#[tokio::test]
async fn should_request_the_seed_exactly_once() {
    let mock = site("User-agent: *\nAllow: /\n").await;

    let result = crawl_seed(&format!("{}/", mock.uri()), 1).await;

    let log = request_log(&mock).await;
    assert_eq!(
        log.iter().filter(|entry| entry.as_str() == "/").count(),
        1,
        "one crawled page must cost one request to the seed, got {log:?}"
    );
    assert_eq!(result.pages.len(), 1, "the seed is the one crawled page");
}

#[tokio::test]
async fn should_read_robots_txt_before_the_first_request_to_the_seed() {
    let mock = site("User-agent: *\nAllow: /\n").await;

    crawl_seed(&format!("{}/", mock.uri()), 1).await;

    let log = request_log(&mock).await;
    let robots_at = log
        .iter()
        .position(|entry| entry == "/robots.txt")
        .expect("robots.txt must be requested");
    let seed_at = log
        .iter()
        .position(|entry| entry == "/")
        .expect("the seed must be requested");
    assert!(
        robots_at < seed_at,
        "the robots.txt decision must precede the first request to the seed, got {log:?}"
    );
}

#[tokio::test]
async fn should_never_request_a_seed_that_robots_txt_disallows() {
    let mock = site("User-agent: *\nDisallow: /\n").await;

    let result = crawl_seed(&format!("{}/", mock.uri()), 10).await;

    let log = request_log(&mock).await;
    assert_eq!(
        log.iter().filter(|entry| entry.as_str() == "/").count(),
        0,
        "a disallowed seed must receive no request at all, got {log:?}"
    );
    assert_eq!(result.pages.len(), 0, "a disallowed seed yields no page");
}

/// A redirect off the seed's origin must be judged by the new origin's robots.txt.
///
/// At the base the redirect target is fetched twice and its robots.txt is never requested,
/// because the port defect makes that file unreachable on its origin too. Fixing the port
/// alone turns this green, since robots.txt was already read for the final URL. From that
/// point the test guards the reverse: that reading the seed's origin first did not drop the
/// read for the origin the redirect lands on.
#[tokio::test]
async fn should_read_robots_txt_again_when_a_redirect_leaves_the_seeds_origin() {
    let target = MockServer::start().await;
    mount_robots(&target, "User-agent: *\nDisallow: /landing.html\n").await;
    mount_page(&target, "/landing.html", "<html><body>landing</body></html>").await;

    let seed = MockServer::start().await;
    mount_robots(&seed, "User-agent: *\nAllow: /\n").await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(302).append_header("location", format!("{}/landing.html", target.uri())))
        .mount(&seed)
        .await;

    let result = crawl_seed(&format!("{}/", seed.uri()), 10).await;

    let target_log = request_log(&target).await;
    assert_eq!(
        target_log
            .iter()
            .filter(|entry| entry.as_str() == "/robots.txt")
            .count(),
        1,
        "the redirect target's own robots.txt must be read, got {target_log:?}"
    );
    assert_eq!(
        result.pages.len(),
        0,
        "the redirect target disallows the landing page, so nothing is crawled"
    );
}
