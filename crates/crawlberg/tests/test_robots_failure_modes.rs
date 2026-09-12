//! RFC 9309 section 2.3.1: what a crawler does when it cannot read robots.txt.
//!
//! A 4xx answer is "Unavailable" (2.3.1.3) and allows every path. A 5xx answer is
//! "Unreachable" (2.3.1.4) and disallows every path, as does a network failure.

use std::time::Duration;

use crawlberg::{CrawlConfig, CrawlResult, crawl, create_engine};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const INDEX_HTML: &str = r#"<html><body><a href="/private/secret.html">x</a></body></html>"#;

/// Serve an index that links to `/private/secret.html`, plus that page, and answer
/// `/robots.txt` with `robots`.
async fn site_with_robots(robots: ResponseTemplate) -> MockServer {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(robots)
        .mount(&mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(INDEX_HTML)
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/private/secret.html"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>secret</body></html>")
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    mock
}

async fn crawl_site(mock: &MockServer, request_timeout: Duration) -> CrawlResult {
    let config = CrawlConfig::builder()
        .respect_robots_txt(true)
        .allow_private_networks(true)
        .request_timeout(request_timeout)
        .max_pages(10)
        .build();
    let engine = create_engine(Some(config)).expect("engine builds");
    crawl(&engine, &format!("{}/", mock.uri())).await.expect("crawl runs")
}

fn crawled_paths(result: &CrawlResult, base: &str) -> Vec<String> {
    result
        .pages
        .iter()
        .map(|page| page.url.strip_prefix(base).unwrap_or(&page.url).to_owned())
        .collect()
}

#[tokio::test]
async fn should_crawl_nothing_when_robots_txt_answers_5xx() {
    let mock = site_with_robots(ResponseTemplate::new(503)).await;

    let result = crawl_site(&mock, Duration::from_secs(10)).await;

    assert_eq!(
        crawled_paths(&result, &mock.uri()),
        Vec::<String>::new(),
        "RFC 9309 2.3.1.4: a 5xx robots.txt is unreachable and disallows every path"
    );
    let error = result
        .error
        .expect("a fail-closed crawl records why it fetched nothing");
    assert!(
        error.starts_with(&format!("robots.txt at {}/robots.txt could not be read", mock.uri())),
        "the reason must name the file that could not be read, got {error:?}"
    );
    assert!(
        error.contains("service unavailable"),
        "the reason must carry the 503 condition, got {error:?}"
    );
    assert!(
        error.ends_with("every path on this origin is disallowed"),
        "the reason must state the RFC 9309 2.3.1.4 consequence, got {error:?}"
    );
}

#[tokio::test]
async fn should_crawl_nothing_when_robots_txt_request_times_out() {
    let mock = site_with_robots(
        ResponseTemplate::new(200)
            .set_body_string("User-agent: *\nAllow: /\n")
            .set_delay(Duration::from_secs(30)),
    )
    .await;

    let result = crawl_site(&mock, Duration::from_millis(300)).await;

    assert_eq!(
        crawled_paths(&result, &mock.uri()),
        Vec::<String>::new(),
        "RFC 9309 2.3.1.4: a robots.txt that cannot be fetched disallows every path"
    );
    let error = result
        .error
        .expect("a fail-closed crawl records why it fetched nothing");
    assert!(
        error.starts_with(&format!("robots.txt at {}/robots.txt could not be read", mock.uri())),
        "the reason must name the file that could not be read, got {error:?}"
    );
    assert!(
        error.ends_with("every path on this origin is disallowed"),
        "the reason must state the RFC 9309 2.3.1.4 consequence, got {error:?}"
    );
}

#[tokio::test]
async fn should_crawl_every_page_when_robots_txt_answers_404() {
    let mock = site_with_robots(ResponseTemplate::new(404)).await;

    let result = crawl_site(&mock, Duration::from_secs(10)).await;

    let mut paths = crawled_paths(&result, &mock.uri());
    paths.sort();
    assert_eq!(
        paths,
        vec!["/".to_owned(), "/private/secret.html".to_owned()],
        "RFC 9309 2.3.1.3: a 4xx robots.txt is unavailable and allows every path"
    );
    assert_eq!(result.error, None, "a fail-open crawl records no robots.txt failure");
}

#[tokio::test]
async fn should_honour_the_rules_when_robots_txt_answers_200() {
    let mock =
        site_with_robots(ResponseTemplate::new(200).set_body_string("User-agent: *\nDisallow: /private/\n")).await;

    let result = crawl_site(&mock, Duration::from_secs(10)).await;

    assert_eq!(
        crawled_paths(&result, &mock.uri()),
        vec!["/".to_owned()],
        "RFC 9309 2.3.1.1: a 2xx robots.txt is parsed and its rules are followed"
    );
    assert_eq!(result.error, None, "a crawl that read robots.txt records no failure");
}
