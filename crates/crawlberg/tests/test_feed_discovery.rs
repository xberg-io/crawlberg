//! Integration tests for feed discovery on a page that declares its own `<meta charset>`.
//!
//! A `<meta charset>` page goes through charset detection and a re-decode of the body before
//! extraction runs, so feed discovery on such a page is a distinct path from feed discovery on
//! a page whose encoding comes from the `Content-Type` header.

use crawlberg::{BrowserMode, CrawlConfig, FeedType, create_engine, scrape};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Builds a `CrawlConfig` whose SSRF policy permits private networks, so wiremock's
/// 127.0.0.1 servers are reachable, and which never escalates to a browser.
///
// ~keep Uses the `allow_private_networks` config seam rather than the
// `CRAWLBERG_ALLOW_PRIVATE_NETWORK` env var: writing that variable is a process-global mutation
// that races every concurrent `std::env::var` read in this binary's other tests.
fn local_config() -> CrawlConfig {
    let mut config = CrawlConfig::builder().allow_private_networks(true).build();
    config.browser.mode = BrowserMode::Never;
    config
}

/// Serve `body_bytes` with `content_type` from a fresh mock server and scrape it.
async fn scrape_bytes(body_bytes: Vec<u8>, content_type: &str) -> (String, crawlberg::ScrapeResult) {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body_bytes, content_type))
        .mount(&mock)
        .await;

    let handle = create_engine(Some(local_config())).expect("engine should build");
    let base = mock.uri();
    let result = scrape(&handle, &base).await.expect("scrape should succeed");
    (base, result)
}

/// Each discovered feed as (url, title, feed type), for an exact-value assertion.
///
// ~keep `FeedType` has no `PartialEq`, so the variant is compared through its `Debug` form.
fn feed_tuples(result: &crawlberg::ScrapeResult) -> Vec<(String, Option<String>, String)> {
    result
        .feeds
        .iter()
        .map(|feed| (feed.url.clone(), feed.title.clone(), format!("{:?}", feed.feed_type)))
        .collect()
}

#[tokio::test]
async fn should_discover_rss_and_atom_feeds_on_a_page_with_a_meta_charset() {
    let html = r#"<!DOCTYPE html><html><head>
        <meta charset="utf-8">
        <title>Blog</title>
        <link rel="alternate" type="application/rss+xml" href="/feed.xml" title="RSS">
        <link rel="alternate" type="application/atom+xml" href="/atom.xml" title="Atom">
        </head><body><p>posts</p></body></html>"#;
    let (base, result) = scrape_bytes(html.as_bytes().to_vec(), "text/html").await;

    assert_eq!(
        result.detected_charset.as_deref(),
        Some("utf-8"),
        "the meta charset must be the detected charset when the header declares none, got {:?}",
        result.detected_charset
    );
    assert_eq!(
        feed_tuples(&result),
        vec![
            (
                format!("{base}/feed.xml"),
                Some("RSS".to_owned()),
                format!("{:?}", FeedType::Rss)
            ),
            (
                format!("{base}/atom.xml"),
                Some("Atom".to_owned()),
                format!("{:?}", FeedType::Atom)
            ),
        ],
        "both feeds must be discovered on a page that declares its own charset"
    );
}

#[tokio::test]
async fn should_decode_a_feed_title_with_the_charset_the_page_declares() {
    let title = "Café";
    let (encoded_title, _, had_errors) = encoding_rs::WINDOWS_1252.encode(title);
    assert!(!had_errors, "the fixture title must be representable in Windows-1252");

    let mut body = Vec::new();
    body.extend_from_slice(
        br#"<!DOCTYPE html><html><head><meta charset="windows-1252">
        <link rel="alternate" type="application/rss+xml" href="/feed.xml" title=""#,
    );
    body.extend_from_slice(&encoded_title);
    body.extend_from_slice(br#""></head><body><p>posts</p></body></html>"#);

    let (base, result) = scrape_bytes(body, "text/html").await;

    assert_eq!(
        feed_tuples(&result),
        vec![(
            format!("{base}/feed.xml"),
            Some(title.to_owned()),
            format!("{:?}", FeedType::Rss)
        )],
        "the feed title must be decoded with the charset the page declares"
    );
}
