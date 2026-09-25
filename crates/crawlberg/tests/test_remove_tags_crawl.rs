//! Regression test: `CrawlConfig::remove_tags` must be honoured by the native `crawl()` path,
//! not just by `scrape()`.
//!
//! ~keep Before the fix, `crawl()`'s page-result path fed `self.config.content` straight to the
//! markdown converter and never consulted `remove_tags` at all -- `scrape()` was the only path
//! that folded `remove_tags` into `exclude_selectors` via `merged_content_config`. `<h2>` is used
//! rather than `<aside>` because the default preprocessing preset strips nav-hinted asides on its
//! own; a marker element the preset leaves alone is required to prove `remove_tags` did the work.

use crawlberg::{CrawlConfig, crawl, create_engine, scrape};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PAGE: &str = r#"<!doctype html>
<html>
	<head><title>Remove Tags Page</title></head>
	<body>
		<main><article>
			<h1>Main Article Title</h1>
			<h2>SIDEBAR_MARKER</h2>
			<p>This is the main content that should be extracted.</p>
		</article></main>
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

fn config_with_remove_tags() -> CrawlConfig {
    CrawlConfig {
        remove_tags: vec!["h2".to_owned()],
        ..CrawlConfig::builder()
            .allow_private_networks(true)
            .respect_robots_txt(false)
            .max_pages(1)
            .max_depth(0)
            .build()
    }
}

#[tokio::test]
async fn should_honour_remove_tags_in_scrape() {
    let mock = serve_page().await;
    let engine = create_engine(Some(config_with_remove_tags())).expect("engine builds");
    let result = scrape(&engine, &mock.uri()).await.expect("scrape succeeds");
    let markdown = result
        .markdown
        .expect("markdown is generated for an HTML response")
        .content;

    assert!(
        markdown.contains("Main Article Title"),
        "the article body must survive remove_tags filtering; got:\n{markdown}"
    );
    assert!(
        !markdown.contains("SIDEBAR_MARKER"),
        "scrape() must honour remove_tags=[\"h2\"] and drop the marker; got:\n{markdown}"
    );
}

#[tokio::test]
async fn should_honour_remove_tags_in_crawl() {
    let mock = serve_page().await;
    let engine = create_engine(Some(config_with_remove_tags())).expect("engine builds");
    let result = crawl(&engine, &mock.uri()).await.expect("crawl succeeds");
    let page = result.pages.first().expect("crawl should return the seed page");
    let markdown = page
        .markdown
        .as_ref()
        .expect("markdown is generated for an HTML response")
        .content
        .clone();

    assert!(
        markdown.contains("Main Article Title"),
        "the article body must survive remove_tags filtering; got:\n{markdown}"
    );
    assert!(
        !markdown.contains("SIDEBAR_MARKER"),
        "crawl() must honour remove_tags=[\"h2\"] just like scrape() does; got:\n{markdown}"
    );
}
