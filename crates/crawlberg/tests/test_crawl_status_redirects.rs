//! Crawl handling of error statuses and redirects on linked pages: the body of a 404 or 410 under
//! `soft_http_errors` (crawlberg#772), a redirect that ends in a 404 (crawlberg#777) and a
//! redirect to a page the crawl already has (crawlberg#776).

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
    engine_with_depth(soft_http_errors, 1)
}

fn engine_with_depth(soft_http_errors: bool, max_depth: usize) -> CrawlEngineHandle {
    let mut config = CrawlConfig::builder().allow_private_networks(true).build();
    config.soft_http_errors = soft_http_errors;
    config.max_depth = Some(max_depth);
    config.respect_robots_txt = false;
    config.browser.mode = BrowserMode::Never;
    create_engine(Some(config)).expect("engine build must not fail")
}

fn html(status: u16, body: &str) -> ResponseTemplate {
    ResponseTemplate::new(status)
        .insert_header("content-type", "text/html; charset=utf-8")
        .set_body_string(body.to_owned())
}

fn redirect(location: &str) -> ResponseTemplate {
    ResponseTemplate::new(301).insert_header("location", location)
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

#[tokio::test]
async fn a_link_that_redirects_to_a_404_is_an_error_event_not_a_page() {
    let mock = MockServer::start().await;
    serve_root(&mock, &["/moved"]).await;
    serve(&mock, "/moved", redirect("/missing")).await;
    serve(&mock, "/missing", html(404, ERROR_PAGE_BODY)).await;

    let crawled = crawl(&engine(false), &format!("{}/", mock.uri())).await;
    assert!(
        !crawled.pages.iter().any(|page| page.0.ends_with("/moved")),
        "a redirect to a 404 must not be a page, got {:?}",
        crawled.pages
    );
    let error = crawled
        .errors
        .iter()
        .find(|(url, _)| url.ends_with("/moved"))
        .unwrap_or_else(|| panic!("an error event for /moved, got {:?}", crawled.errors));
    assert!(error.1.contains("404"), "the error names the status, got {error:?}");
}

#[tokio::test]
async fn a_link_that_redirects_to_a_404_is_still_a_page_when_soft_errors_enabled() {
    let mock = MockServer::start().await;
    serve_root(&mock, &["/moved"]).await;
    serve(&mock, "/moved", redirect("/missing")).await;
    serve(&mock, "/missing", html(404, ERROR_PAGE_BODY)).await;

    let crawled = crawl(&engine(true), &format!("{}/", mock.uri())).await;
    let page = crawled
        .pages
        .iter()
        .find(|page| page.0.ends_with("/moved"))
        .unwrap_or_else(|| panic!("a soft page for /moved, got {:?}", crawled.pages));
    assert_eq!(page.3, 404);
    assert!(
        page.4.contains("real readable body text"),
        "the redirected 404 keeps its body, got {:?}",
        page.4
    );
}

/// A seed redirecting to a status the direct fetch accepts as a page (418, 451) is a page, as a
/// linked one is. Before this change any seed redirect to a status >= 400 was a crawl error.
#[tokio::test]
async fn a_seed_that_redirects_to_a_418_or_451_page_is_a_page() {
    for status in [418_u16, 451] {
        let mock = MockServer::start().await;
        serve(&mock, "/moved", redirect("/blocked")).await;
        serve(&mock, "/blocked", html(status, ERROR_PAGE_BODY)).await;

        let crawled = crawl(&engine(false), &format!("{}/moved", mock.uri())).await;
        assert!(
            crawled.errors.is_empty(),
            "{status}: no error events, got {:?}",
            crawled.errors
        );
        let page = crawled
            .pages
            .first()
            .unwrap_or_else(|| panic!("{status}: a page, got {:?}", crawled.pages));
        assert_eq!(page.3, status);
        assert!(page.4.contains("real readable body text"), "{status}: got {:?}", page.4);
    }
}

/// A redirect chain of several hops ending in a 404 is an error for a linked page and a seed alike.
#[tokio::test]
async fn a_multi_hop_redirect_chain_ending_in_a_404_is_an_error() {
    let mock = MockServer::start().await;
    serve_root(&mock, &["/first"]).await;
    serve(&mock, "/first", redirect("/second")).await;
    serve(&mock, "/second", redirect("/third")).await;
    serve(&mock, "/third", redirect("/missing")).await;
    serve(&mock, "/missing", html(404, ERROR_PAGE_BODY)).await;

    let linked = crawl(&engine(false), &format!("{}/", mock.uri())).await;
    assert!(
        !linked.pages.iter().any(|page| page.0.ends_with("/first")),
        "got {:?}",
        linked.pages
    );
    let error = linked
        .errors
        .iter()
        .find(|(url, _)| url.ends_with("/first"))
        .unwrap_or_else(|| panic!("an error event for /first, got {:?}", linked.errors));
    assert!(error.1.contains("404"), "got {error:?}");

    let seed = crawl(&engine(false), &format!("{}/first", mock.uri())).await;
    assert!(seed.pages.is_empty(), "got {:?}", seed.pages);
    assert!(
        seed.errors.iter().any(|(_, error)| error.contains("404")),
        "a seed error event naming 404, got {:?}",
        seed.errors
    );
}

/// The body of a soft 404 is a real page now, but its navigation links are not followed.
#[tokio::test]
async fn a_soft_404_page_does_not_enqueue_its_links() {
    let mock = MockServer::start().await;
    serve_root(&mock, &["/missing"]).await;
    let error_page =
        "<html><body><p>Not found, real readable body text.</p><a href=\"/deeper\">deeper</a></body></html>";
    serve(&mock, "/missing", html(404, error_page)).await;
    serve(
        &mock,
        "/deeper",
        html(200, "<html><body><p>Deeper text.</p></body></html>"),
    )
    .await;

    let crawled = crawl(&engine_with_depth(true, 3), &format!("{}/", mock.uri())).await;
    assert!(
        crawled
            .pages
            .iter()
            .any(|page| page.0.ends_with("/missing") && page.3 == 404),
        "the 404 is still a page, got {:?}",
        crawled.pages
    );
    assert!(
        !crawled.pages.iter().any(|page| page.0.ends_with("/deeper")),
        "a link on a soft 404 must not be followed, got {:?}",
        crawled.pages
    );
    let requested: Vec<String> = mock
        .received_requests()
        .await
        .expect("requests are recorded")
        .iter()
        .map(|request| request.url.path().to_owned())
        .collect();
    assert!(!requested.iter().any(|p| p == "/deeper"), "requested: {requested:?}");
}

/// A seed that redirects to a 404 is a page under `soft_http_errors`, as a linked one is.
#[tokio::test]
async fn a_seed_that_redirects_to_a_404_is_a_page_when_soft_errors_enabled() {
    let mock = MockServer::start().await;
    serve(&mock, "/moved", redirect("/missing")).await;
    serve(&mock, "/missing", html(404, ERROR_PAGE_BODY)).await;

    let crawled = crawl(&engine(true), &format!("{}/moved", mock.uri())).await;
    assert!(crawled.errors.is_empty(), "no error events, got {:?}", crawled.errors);
    let page = crawled
        .pages
        .first()
        .unwrap_or_else(|| panic!("a page, got {:?}", crawled.pages));
    assert_eq!(page.3, 404);
    assert!(page.4.contains("real readable body text"), "got {:?}", page.4);
}

/// A seed that redirects to a 404 is an error without the flag, as before.
#[tokio::test]
async fn a_seed_that_redirects_to_a_404_is_an_error_when_soft_errors_disabled() {
    let mock = MockServer::start().await;
    serve(&mock, "/moved", redirect("/missing")).await;
    serve(&mock, "/missing", html(404, ERROR_PAGE_BODY)).await;

    let crawled = crawl(&engine(false), &format!("{}/moved", mock.uri())).await;
    assert!(crawled.pages.is_empty(), "got {:?}", crawled.pages);
    assert!(
        crawled.errors.iter().any(|(_, error)| error.contains("404")),
        "an error event naming 404, got {:?}",
        crawled.errors
    );
}

/// A link whose redirect lands on a URL the crawl already holds is requested but gets no event of
/// its own: the destination page is the event, and it is reported once. Documented on
/// `CrawlEvent::Page`.
#[tokio::test]
async fn a_link_that_redirects_to_a_queued_page_gives_no_event_and_the_destination_once() {
    let mock = MockServer::start().await;
    serve_root(&mock, &["/a", "/b", "/c"]).await;
    serve(&mock, "/a", redirect("/c")).await;
    serve(&mock, "/b", redirect("/c")).await;
    serve(&mock, "/c", html(200, "<html><body><p>Page C text.</p></body></html>")).await;

    let crawled = crawl(&engine(false), &format!("{}/", mock.uri())).await;
    let requested: Vec<String> = mock
        .received_requests()
        .await
        .expect("requests are recorded")
        .iter()
        .map(|request| request.url.path().to_owned())
        .collect();
    for route in ["/a", "/b"] {
        assert!(
            requested.iter().any(|p| p == route),
            "{route} was requested: {requested:?}"
        );
    }
    assert!(
        crawled.errors.is_empty(),
        "a duplicate redirect is not an error: {:?}",
        crawled.errors
    );
    let urls: Vec<&str> = crawled.pages.iter().map(|page| page.0.as_str()).collect();
    assert_eq!(
        urls.iter().filter(|url| url.ends_with("/c")).count(),
        1,
        "the destination is reported once: {urls:?}"
    );
    assert!(
        !urls.iter().any(|url| url.ends_with("/a") || url.ends_with("/b")),
        "the redirecting links give no event of their own: {urls:?}"
    );
}
