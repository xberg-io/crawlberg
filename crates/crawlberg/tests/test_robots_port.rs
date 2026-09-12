//! A crawl must read the robots.txt of the seed's own origin, port included.
//!
//! RFC 9309 section 2.3 scopes a robots.txt file to a scheme, a host and a port.
//! Every server here binds an ephemeral port, so the seed origin is never the
//! scheme's default port.

use crawlberg::{CrawlConfig, CrawlResult, crawl, create_engine};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const INDEX_HTML: &str = r#"<html><body><a href="/private/secret.html">x</a></body></html>"#;

async fn site_with_robots_body(body: &str) -> MockServer {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body.to_owned()))
        .mount(&mock)
        .await;

    for (at, html) in [
        ("/", INDEX_HTML),
        ("/private/secret.html", "<html><body>secret</body></html>"),
    ] {
        Mock::given(method("GET"))
            .and(path(at))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(html.to_owned())
                    .append_header("content-type", "text/html"),
            )
            .mount(&mock)
            .await;
    }

    mock
}

async fn crawl_site(mock: &MockServer) -> CrawlResult {
    let config = CrawlConfig::builder()
        .respect_robots_txt(true)
        .allow_private_networks(true)
        .max_pages(10)
        .build();
    let engine = create_engine(Some(config)).expect("engine builds");
    crawl(&engine, &format!("{}/", mock.uri())).await.expect("crawl runs")
}

fn crawled_paths(result: &CrawlResult, base: &str) -> Vec<String> {
    let mut paths: Vec<String> = result
        .pages
        .iter()
        .map(|page| page.url.strip_prefix(base).unwrap_or(&page.url).to_owned())
        .collect();
    paths.sort();
    paths
}

async fn robots_request_count(mock: &MockServer) -> usize {
    mock.received_requests()
        .await
        .expect("the mock server records its requests")
        .iter()
        .filter(|request| request.url.path() == "/robots.txt")
        .count()
}

#[tokio::test]
async fn should_request_robots_txt_on_the_seeds_own_port() {
    let mock = site_with_robots_body("User-agent: *\nDisallow: /private/\n").await;

    let result = crawl_site(&mock).await;

    assert_eq!(
        robots_request_count(&mock).await,
        1,
        "the seed's own origin must receive the robots.txt request"
    );
    assert_eq!(
        crawled_paths(&result, &mock.uri()),
        vec!["/".to_owned()],
        "a rule read from the seed's port must be applied"
    );
}

#[tokio::test]
async fn should_crawl_the_disallowed_path_when_that_ports_robots_txt_allows_it() {
    let mock = site_with_robots_body("User-agent: *\nDisallow: /nothing-here/\n").await;

    let result = crawl_site(&mock).await;

    assert_eq!(
        robots_request_count(&mock).await,
        1,
        "the seed's own origin must receive the robots.txt request"
    );
    assert_eq!(
        crawled_paths(&result, &mock.uri()),
        vec!["/".to_owned(), "/private/secret.html".to_owned()],
        "reading robots.txt from the right port must not disallow paths the file allows"
    );
}
