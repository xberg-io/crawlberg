//! Integration tests for markdown output: citations, fit_content, and structure.

use crawlberg::{CrawlConfig, create_engine, scrape};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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

    let handle = create_engine(Some(CrawlConfig::default())).unwrap();
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

    let handle = create_engine(Some(CrawlConfig::default())).unwrap();
    let result = scrape(&handle, &mock.uri()).await.unwrap();
    let md = result.markdown.expect("markdown should be present");

    assert!(
        md.content.contains("# Main Title") || md.content.contains("Main Title"),
        "should contain h1 content in markdown: {}",
        md.content
    );
    assert!(md.content.contains("Section One"), "should contain h2 content");
}
