//! Integration tests for markdown output: citations, fit_content, and structure.

use crawlberg::{CrawlConfig, create_engine, scrape};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Builds a `CrawlConfig` whose SSRF policy permits private networks, so wiremock's
/// 127.0.0.1 servers are reachable.
///
// ~keep Uses the `allow_private_networks` config seam rather than the
// `CRAWLBERG_ALLOW_PRIVATE_NETWORK` env var: writing that variable is a process-global mutation
// that races every concurrent `std::env::var` read (`SsrfPolicy::from_env`, reached from
// `CrawlConfig::default()`) in this binary's other tests, aborting the process on glibc
// with no failing test name.
fn allow_private_config() -> CrawlConfig {
    CrawlConfig::builder().allow_private_networks(true).build()
}

#[tokio::test]
async fn test_markdown_output_is_populated() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(
                    r#"<html><body>
                <nav><a href="/">Home</a> | <a href="/about">About</a></nav>
                <article>
                    <h1>Title</h1>
                    <p>Visit <a href="https://example.com">Example</a> for more info.</p>
                    <p>Some additional content here to fill the page.</p>
                </article>
                <footer>Copyright 2024. All rights reserved.</footer>
            </body></html>"#,
                )
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let handle = create_engine(Some(allow_private_config())).unwrap();
    let result = scrape(&handle, &mock.uri()).await.unwrap();
    let md = result.markdown.expect("markdown should be present");

    assert!(
        md.content.contains("Title"),
        "markdown content should contain the heading"
    );
    assert!(
        md.content.contains("Example"),
        "markdown content should contain link text"
    );

    assert!(md.citations, "citations flag should be true for pages with links");

    assert!(md.fit_content.is_some(), "fit content should be populated");
}

#[tokio::test]
async fn test_markdown_heading_extraction() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(
                    r#"<html><body>
                    <h1>Main Title</h1>
                    <h2>Section One</h2>
                    <p>Content for section one.</p>
                    <h2>Section Two</h2>
                    <p>Content for section two.</p>
                </body></html>"#,
                )
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let handle = create_engine(Some(allow_private_config())).unwrap();
    let result = scrape(&handle, &mock.uri()).await.unwrap();
    let md = result.markdown.expect("markdown should be present");

    assert!(
        md.content.contains("# Main Title") || md.content.contains("Main Title"),
        "should contain h1 content in markdown: {}",
        md.content
    );
    assert!(md.content.contains("Section One"), "should contain h2 content");
}

/// The markdown of a scraped page resolves relative links against the page's `<base href>`,
/// which is itself relative to the page URL (issue #63).
#[tokio::test]
async fn test_markdown_links_resolve_against_a_relative_base_href() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(
                    r#"<html><head><base href="/other/"></head><body><a href="leaf.html">leaf</a></body></html>"#,
                )
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let handle = create_engine(Some(allow_private_config())).unwrap();
    let result = scrape(&handle, &mock.uri()).await.unwrap();
    let md = result.markdown.expect("markdown should be present");
    let expected = format!("{}/other/leaf.html", mock.uri());

    assert!(
        md.content.contains(&format!("[leaf]({expected})")),
        "the markdown link must resolve against the base href, got {:?}",
        md.content
    );
    assert!(
        result.links.iter().any(|link| link.url == expected),
        "the links list must agree with the markdown, got {:?}",
        result.links
    );
}

/// A scrape that follows a redirect resolves the markdown's relative links against the URL
/// that served the content, not the one requested (issue #63).
#[tokio::test]
async fn test_markdown_links_resolve_against_the_scrape_redirect_target() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/go"))
        .respond_with(ResponseTemplate::new(302).append_header("location", "/page/index.html"))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/page/index.html"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"<html><body><p><a href="next.html">next</a></p></body></html>"#)
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let handle = create_engine(Some(allow_private_config())).unwrap();
    let result = scrape(&handle, &format!("{}/go", mock.uri())).await.unwrap();
    let md = result.markdown.expect("markdown should be present");

    assert!(
        md.content.contains(&format!("[next]({}/page/next.html)", mock.uri())),
        "the markdown link must resolve against the redirect target, got {:?}",
        md.content
    );
}

/// A scraped page's inline `data:` image keeps its alt text in the markdown, and its encoded
/// payload stays out of both the content and `fit_content` (issue #97).
#[tokio::test]
async fn test_markdown_leaves_out_the_payload_of_an_inline_image() {
    let payload = "PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciLz4=".repeat(40);
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(format!(
                    r#"<html><body><p>Real text before the icon.</p><img src="data:image/svg+xml;base64,{payload}" alt="icon"><p>Real text after the icon.</p></body></html>"#
                ))
                .append_header("content-type", "text/html"),
        )
        .mount(&mock)
        .await;

    let handle = create_engine(Some(allow_private_config())).unwrap();
    let result = scrape(&handle, &mock.uri()).await.unwrap();
    let md = result.markdown.expect("markdown should be present");
    let fit = md.fit_content.clone().unwrap_or_default();

    assert!(
        md.content.contains("![icon]"),
        "the alt text must stay, got {:?}",
        md.content
    );
    assert!(md.content.contains("Real text after the icon."), "got {:?}", md.content);
    assert!(
        fit.contains("![icon]"),
        "fit_content must keep the image line, got {fit:?}"
    );
    for text in [&md.content, &fit] {
        assert!(
            !text.contains("PHN2Zy"),
            "the encoded payload must not reach the markdown, got {text:?}"
        );
        assert!(
            text.len() < 200,
            "{} bytes of markdown for two sentences and an icon",
            text.len()
        );
    }
}
