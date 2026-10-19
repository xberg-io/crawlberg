//! Crawl handling of error statuses and redirects on linked pages: the body of a 404 or 410 under
//! `soft_http_errors` (crawlberg#772).

#![allow(clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro)]

use crawlberg::{
    BrowserMode, CrawlConfig, CrawlEngineHandle, CrawlError, CrawlEvent, crawl_stream, create_engine, scrape,
};
use futures::StreamExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ERROR_PAGE_BODY: &str = "<html><head><title>T</title></head><body><h1>Heading</h1>\
<p>This error page has real readable body text.</p></body></html>";

fn engine(soft_http_errors: bool) -> CrawlEngineHandle {
    let mut config = CrawlConfig::builder().allow_private_networks(true).build();
    config.soft_http_errors = soft_http_errors;
    config.max_depth = Some(1);
    config.respect_robots_txt = false;
    config.browser.mode = BrowserMode::Never;
    create_engine(Some(config)).expect("engine build must not fail")
}

fn html(status: u16, body: &str) -> ResponseTemplate {
    ResponseTemplate::new(status)
        .insert_header("content-type", "text/html; charset=utf-8")
        .set_body_string(body.to_owned())
}

struct Crawled {
    /// (url, original_url, final_url, status, markdown)
    pages: Vec<(String, String, String, u16, String)>,
    errors: Vec<(String, String)>,
}

async fn crawl(handle: &CrawlEngineHandle, root: &str) -> Crawled {
    let mut crawled = Crawled {
        pages: Vec::new(),
        errors: Vec::new(),
    };
    let mut stream = crawl_stream(handle, root).await.expect("crawl must start");
    while let Some(event) = stream.next().await {
        match event.expect("a crawl event must not be a transport error") {
            CrawlEvent::Page { result } => {
                let markdown = result.markdown.as_ref().map(|m| m.content.clone()).unwrap_or_default();
                crawled.pages.push((
                    result.url.clone(),
                    result.original_url.clone(),
                    result.final_url.clone(),
                    result.status_code,
                    markdown,
                ));
            }
            CrawlEvent::Error { url, error } => crawled.errors.push((url, error)),
            CrawlEvent::Complete { .. } => {}
        }
    }
    crawled
}

async fn serve_root(mock: &MockServer, links: &[&str]) {
    let anchors: String = links.iter().map(|l| format!(r#"<a href="{l}">{l}</a> "#)).collect();
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(html(200, &format!("<html><body><h1>Start</h1>{anchors}</body></html>")))
        .mount(mock)
        .await;
}

async fn serve(mock: &MockServer, route: &str, response: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(response)
        .mount(mock)
        .await;
}

#[tokio::test]
async fn crawl_converts_the_body_of_a_404_and_a_410_when_soft_errors_enabled() {
    let mock = MockServer::start().await;
    serve_root(&mock, &["/ok", "/gone", "/missing"]).await;
    serve(&mock, "/ok", html(200, ERROR_PAGE_BODY)).await;
    serve(&mock, "/gone", html(410, ERROR_PAGE_BODY)).await;
    serve(&mock, "/missing", html(404, ERROR_PAGE_BODY)).await;

    let crawled = crawl(&engine(true), &format!("{}/", mock.uri())).await;
    assert!(crawled.errors.is_empty(), "no error events, got {:?}", crawled.errors);
    for (route, status) in [("/ok", 200_u16), ("/gone", 410), ("/missing", 404)] {
        let page = crawled
            .pages
            .iter()
            .find(|page| page.0.ends_with(route))
            .unwrap_or_else(|| panic!("{route} must be a page, got {:?}", crawled.pages));
        assert_eq!(page.3, status, "{route}: status");
        assert!(
            page.4.contains("real readable body text"),
            "{route}: markdown must hold the body, got {:?}",
            page.4
        );
    }
}

#[tokio::test]
async fn scrape_returns_a_410_page_with_its_body_when_soft_errors_enabled() {
    let mock = MockServer::start().await;
    serve(&mock, "/gone", html(410, ERROR_PAGE_BODY)).await;
    let page = scrape(&engine(true), &format!("{}/gone", mock.uri()))
        .await
        .expect("a 410 must be a page under soft_http_errors");
    assert_eq!(page.status_code, 410);
    let markdown = page.markdown.expect("the body must be converted").content;
    assert!(markdown.contains("real readable body text"), "got {markdown:?}");
}

#[tokio::test]
async fn a_410_stays_an_error_when_soft_errors_disabled() {
    let mock = MockServer::start().await;
    serve(&mock, "/gone", html(410, ERROR_PAGE_BODY)).await;
    let result = scrape(&engine(false), &format!("{}/gone", mock.uri())).await;
    assert!(matches!(result, Err(CrawlError::Gone { .. })), "got {result:?}");
}
