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
