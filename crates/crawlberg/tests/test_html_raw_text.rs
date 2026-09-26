//! Integration tests for raw-text element handling (`script`, `style`, `textarea`, `title`).
//!
//! These drive the real `scrape()` pipeline so the assertions cover the production
//! parse path, not a unit-test-only helper.

use crawlberg::{BrowserMode, CrawlConfig, crawl, create_engine, scrape};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Builds a `CrawlConfig` whose SSRF policy permits private networks, so wiremock's
/// 127.0.0.1 servers are reachable.
///
// ~keep Uses the `allow_private_networks` config seam rather than the
// `CRAWLBERG_ALLOW_PRIVATE_NETWORK` env var: writing that variable is a process-global mutation
// that races every concurrent `std::env::var` read in this binary's other tests.
fn allow_private_config() -> CrawlConfig {
    CrawlConfig::builder().allow_private_networks(true).build()
}

/// Serve `body` as `text/html` from a fresh mock server and scrape it.
async fn scrape_html(body: &str) -> (String, crawlberg::ScrapeResult) {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body.to_owned())
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let handle = create_engine(Some(allow_private_config())).expect("engine should build");
    let base = mock.uri();
    let result = scrape(&handle, &base).await.expect("scrape should succeed");
    (base, result)
}

#[tokio::test]
async fn should_find_links_after_a_comment_opener_inside_script_text() {
    let html = r#"<html><body>
        <script>var marker = "/* <!-- */";</script>
        <a href="/real">real</a>
        <p>tail</p>
    </body></html>"#;
    let (base, result) = scrape_html(html).await;

    let urls: Vec<&str> = result.links.iter().map(|l| l.url.as_str()).collect();
    assert_eq!(
        urls,
        vec![format!("{base}/real")],
        "a `<!--` inside script text must not hide the anchor that follows it"
    );
}

#[tokio::test]
async fn should_find_links_after_a_comment_opener_inside_style_text() {
    let html = r#"<html><head><style>body:after { content: "<!--"; }</style></head>
        <body><a href="/after-style">after</a></body></html>"#;
    let (base, result) = scrape_html(html).await;

    let urls: Vec<&str> = result.links.iter().map(|l| l.url.as_str()).collect();
    assert_eq!(
        urls,
        vec![format!("{base}/after-style")],
        "a `<!--` inside style text must not hide the anchor that follows it"
    );
}

#[tokio::test]
async fn should_not_extract_links_that_only_appear_inside_raw_text() {
    let html = r#"<html><head><title>Real Title</title></head><body>
        <script>document.write('<a href="/from-script">x</a>');</script>
        <style>/* <a href="/from-style">y</a> */</style>
        <textarea><a href="/from-textarea">z</a></textarea>
        <a href="/real">real</a>
    </body></html>"#;
    let (base, result) = scrape_html(html).await;

    let urls: Vec<&str> = result.links.iter().map(|l| l.url.as_str()).collect();
    assert_eq!(
        urls,
        vec![format!("{base}/real")],
        "links inside script/style/textarea text must be ignored, and the real link kept"
    );
    assert_eq!(
        result.metadata.title.as_deref(),
        Some("Real Title"),
        "title text must still populate the title metadata"
    );
}

#[tokio::test]
async fn should_not_extract_images_that_only_appear_inside_raw_text() {
    let html = r#"<html><body>
        <script>var t = '<img src="/from-script.png">';</script>
        <textarea><img src="/from-textarea.png"></textarea>
        <img src="/real.png" alt="real">
    </body></html>"#;
    let (base, result) = scrape_html(html).await;

    let urls: Vec<&str> = result.images.iter().map(|i| i.url.as_str()).collect();
    assert_eq!(
        urls,
        vec![format!("{base}/real.png")],
        "images inside script/textarea text must be ignored, and the real image kept"
    );
}

#[tokio::test]
async fn should_ignore_a_base_href_inside_title_text() {
    let html = r#"<html><head><title>Home <base href="https://hijacked.example/"></title></head>
        <body><a href="/page">page</a></body></html>"#;
    let (base, result) = scrape_html(html).await;

    let urls: Vec<&str> = result.links.iter().map(|l| l.url.as_str()).collect();
    assert_eq!(
        urls,
        vec![format!("{base}/page")],
        "a `<base href>` inside title text must not change the document base"
    );
    assert!(
        result
            .metadata
            .title
            .as_deref()
            .is_some_and(|t| t.trim_start().starts_with("Home")),
        "the title metadata must still carry the title text, got {:?}",
        result.metadata.title
    );
}

#[tokio::test]
async fn should_still_extract_json_ld_from_script_contents() {
    let html = r#"<html><head>
        <script type="application/ld+json">{"@type":"Article","name":"Ada"}</script>
        </head><body><a href="/real">real</a></body></html>"#;
    let (base, result) = scrape_html(html).await;

    assert_eq!(
        result.json_ld.len(),
        1,
        "one JSON-LD entry expected, got {:?}",
        result.json_ld
    );
    assert_eq!(
        result.json_ld[0].schema_type, "Article",
        "JSON-LD @type should survive raw-text handling, got {:?}",
        result.json_ld[0]
    );
    let urls: Vec<&str> = result.links.iter().map(|l| l.url.as_str()).collect();
    assert_eq!(
        urls,
        vec![format!("{base}/real")],
        "the real link should still be found"
    );
}

#[tokio::test]
async fn should_keep_links_when_a_script_sits_inside_an_html_comment() {
    // ~keep The `>` before the `<script>` matters: it makes the fixture fail unless comments
    // ~keep are skipped to `-->`, instead of merely to the first `>`.
    let html = r#"<html><body>
        <!-- a > b <script> --><a href="/real">real</a>
    </body></html>"#;
    let (base, result) = scrape_html(html).await;

    let urls: Vec<&str> = result.links.iter().map(|l| l.url.as_str()).collect();
    assert_eq!(
        urls,
        vec![format!("{base}/real")],
        "a `<script>` inside a comment must not start a raw-text region"
    );
}

/// A crawl must not follow an address that only exists as raw text, because the crawl path
/// masks raw text with its own call before it parses the page. Scrape has a separate call, so
/// only a crawl covers this one.
#[tokio::test]
async fn should_not_follow_a_link_that_only_appears_inside_raw_text_when_crawling() {
    let mock = MockServer::start().await;
    for route in ["/real", "/from-script", "/from-title"] {
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(format!("<html><body>leaf {route}</body></html>"))
                    .append_header("content-type", "text/html"),
            )
            .mount(&mock)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(
                    r#"<html><head><title>Root <a href="/from-title">t</a></title></head><body>
                    <script>document.write('<a href="/from-script">s</a>');</script>
                    <a href="/real">real</a>
                    </body></html>"#
                        .to_owned(),
                )
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let mut config = CrawlConfig {
        max_depth: Some(2),
        max_pages: Some(10),
        // ~keep One fetch in flight so `pages` is filled in a deterministic order.
        max_concurrent: Some(1),
        ..allow_private_config()
    };
    config.browser.mode = BrowserMode::Never;
    let handle = create_engine(Some(config)).expect("engine should build");
    let base = mock.uri();
    let result = crawl(&handle, &base).await.expect("crawl should succeed");

    let paths: Vec<String> = result
        .pages
        .iter()
        .map(|page| match page.url.strip_prefix(&base).unwrap_or(&page.url) {
            "" => "/".to_owned(),
            rest => rest.to_owned(),
        })
        .collect();
    assert_eq!(
        paths,
        vec!["/".to_owned(), "/real".to_owned()],
        "the crawl frontier must take only the links a browser sees on the root page"
    );
}
