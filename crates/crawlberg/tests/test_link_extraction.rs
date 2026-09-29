//! Integration tests for how the crawl reads link addresses: character references in an
//! address (crawlberg#86) and markup written in uppercase (crawlberg#87).

use crawlberg::{CrawlConfig, CrawlResult, crawl, create_engine};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn mount_html(mock: &MockServer, at: &str, body: &str) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body.to_owned())
                .append_header("content-type", "text/html"),
        )
        .mount(mock)
        .await;
}

/// Every path and query the server was asked for, in arrival order.
async fn request_log(mock: &MockServer) -> Vec<String> {
    mock.received_requests()
        .await
        .expect("the mock server records its requests")
        .iter()
        .map(|request| match request.url.query() {
            Some(query) => format!("{}?{query}", request.url.path()),
            None => request.url.path().to_owned(),
        })
        .collect()
}

async fn crawl_seed(seed: &str) -> CrawlResult {
    let config = CrawlConfig::builder()
        .allow_private_networks(true)
        .max_pages(10)
        .max_depth(1)
        .build();
    let engine = create_engine(Some(config)).expect("engine builds");
    crawl(&engine, seed).await.expect("crawl runs")
}

/// `&amp;` in an `href` is an HTML-encoded `&`, so the crawl requests `?a=1&b=2`.
#[tokio::test]
async fn an_encoded_ampersand_in_a_link_is_crawled_as_a_plain_ampersand() {
    let mock = MockServer::start().await;
    mount_html(
        &mock,
        "/",
        r#"<html><body><a href="/list?a=1&amp;b=2">list</a> <a href="&#47;root.html">root</a></body></html>"#,
    )
    .await;
    mount_html(&mock, "/list", "<html><body>list</body></html>").await;
    mount_html(&mock, "/root.html", "<html><body>root</body></html>").await;

    let result = crawl_seed(&format!("{}/", mock.uri())).await;

    let requests = request_log(&mock).await;
    assert!(
        requests.iter().any(|r| r == "/list?a=1&b=2"),
        "the decoded address must be requested, got {requests:?}"
    );
    assert!(
        requests.iter().any(|r| r == "/root.html"),
        "a numeric character reference must be decoded, got {requests:?}"
    );
    let seed = &result.pages[0];
    assert!(
        seed.links
            .iter()
            .all(|link| !link.url.contains("&amp;") && !link.url.contains("&#")),
        "the links list must hold decoded addresses, got {:?}",
        seed.links
    );
}

/// Tag names are case-insensitive in HTML, so `<A HREF>` is a link the crawl follows.
#[tokio::test]
async fn a_link_in_uppercase_markup_is_followed() {
    let mock = MockServer::start().await;
    mount_html(&mock, "/", r#"<HTML><BODY><A HREF="/up.html">up</A></BODY></HTML>"#).await;
    mount_html(&mock, "/up.html", "<html><body>up</body></html>").await;

    crawl_seed(&format!("{}/", mock.uri())).await;

    let requests = request_log(&mock).await;
    assert!(
        requests.iter().any(|r| r == "/up.html"),
        "the uppercase link must be crawled, got {requests:?}"
    );
}
