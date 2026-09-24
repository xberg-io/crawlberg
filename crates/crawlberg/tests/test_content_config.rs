//! Integration tests for `CrawlConfig::content` reaching the markdown converter.
//!
//! ~keep Until these landed, `content` had no end-to-end coverage anywhere: of 267 fixtures
//! exactly two set it, one with an empty assertion list and the other asserting only batch
//! counts. A `content` that was ignored entirely passed green in all 16 generated language
//! suites, which is how xberg-io/crawlberg#56 reached a user. These two tests are a matched
//! pair over one HTML document -- the default preset keeps the footer, `aggressive` drops it --
//! so a regression that stops honouring `content` fails exactly one of them, and a regression
//! that breaks markdown generation outright fails both.

use crawlberg::{ContentConfig, CrawlConfig, create_engine, scrape};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PAGE: &str = r#"<!doctype html>
<html>
	<head><title>Main Content Page</title></head>
	<body>
		<nav><a href="/home">Home</a><a href="/about">About</a></nav>
		<aside class="sidebar"><p>Sidebar content</p></aside>
		<main><article>
			<h1>Main Article Title</h1>
			<p>This is the main content that should be extracted.</p>
		</article></main>
		<footer><p>Footer content &copy; 2024</p></footer>
	</body>
</html>"#;

async fn serve_page() -> MockServer {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(PAGE)
                .append_header("content-type", "text/html; charset=utf-8"),
        )
        .mount(&mock)
        .await;
    mock
}

fn config_with(content: ContentConfig) -> CrawlConfig {
    CrawlConfig {
        content,
        ..CrawlConfig::builder()
            .allow_private_networks(true)
            .respect_robots_txt(false)
            .build()
    }
}

async fn markdown_for(content: ContentConfig) -> String {
    let mock = serve_page().await;
    let engine = create_engine(Some(config_with(content))).expect("engine builds");
    let result = scrape(&engine, &mock.uri()).await.expect("scrape succeeds");
    result
        .markdown
        .expect("markdown is generated for an HTML response")
        .content
}

#[tokio::test]
async fn should_drop_the_footer_when_preprocessing_preset_is_aggressive() {
    let markdown = markdown_for(ContentConfig {
        preprocessing_preset: "aggressive".to_owned(),
        ..ContentConfig::default()
    })
    .await;

    assert!(
        markdown.contains("Main Article Title"),
        "the article body must survive preprocessing; got:\n{markdown}"
    );
    assert!(
        !markdown.contains("Footer content"),
        "`aggressive` drops footers unconditionally, so the footer must be gone; got:\n{markdown}"
    );
    assert!(
        !markdown.contains("Sidebar content"),
        "`aggressive` drops asides unconditionally, so the sidebar must be gone; got:\n{markdown}"
    );
}

#[tokio::test]
async fn should_keep_the_footer_when_preprocessing_preset_is_default() {
    let markdown = markdown_for(ContentConfig::default()).await;

    assert!(
        markdown.contains("Main Article Title"),
        "the article body must survive preprocessing; got:\n{markdown}"
    );
    // ~keep This is the negative control for the test above. The default preset is `standard`,
    // which removes nav and nav-hinted asides but NOT a plain `<footer>`. If `content` were
    // ignored, both tests would see `standard` output and only the `aggressive` one would fail --
    // this assertion is what makes that failure mean "config ignored" rather than "footer
    // handling changed".
    assert!(
        markdown.contains("Footer content"),
        "`standard` keeps a plain footer; got:\n{markdown}"
    );
    assert!(
        !markdown.contains("Sidebar content"),
        "`standard` removes nav-hinted asides; got:\n{markdown}"
    );
}
