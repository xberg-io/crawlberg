//! A page a browser renders reports the status the server answered, and that status is
//! handled as HTTP mode handles it: a 404 or 500 page is the error the HTTP fetch returns, and
//! a crawl keeps the same pages.
//!
//! Requires a real Chrome binary for the Chromiumoxide backend; skipped (not failed) when
//! Chrome is unavailable, matching the other browser tests.

#![cfg(feature = "browser")]

use std::time::Duration;

use crawlberg::{BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig, CrawlError, crawl, create_engine, scrape};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::{announce_chrome_skip, is_missing_chrome_message};

fn config(backend: BrowserBackend, mode: BrowserMode) -> CrawlConfig {
    CrawlConfig {
        browser: BrowserConfig {
            backend,
            mode,
            timeout: Duration::from_secs(20),
            ..BrowserConfig::default()
        },
        respect_robots_txt: false,
        max_depth: Some(1),
        ..CrawlConfig::builder().allow_private_networks(true).build()
    }
}

fn page(status: u16, body: &str) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_raw(format!("<html><body>{body}</body></html>"), "text/html")
}

/// `/` links to `/ok`, `/missing` and `/broken`, which answer 200, 404 and 500 with a page.
/// `/moved` redirects to `/missing`.
async fn site() -> MockServer {
    let site = MockServer::start().await;
    let routes = [
        (
            "/",
            200,
            r#"<a href="/ok">ok</a><a href="/missing">missing</a><a href="/broken">broken</a>"#,
        ),
        ("/ok", 200, "<p>fine</p>"),
        ("/missing", 404, "<p>no such page</p>"),
        ("/broken", 500, "<p>server trouble</p>"),
    ];
    for (route, status, body) in routes {
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(page(status, body))
            .mount(&site)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/moved"))
        .respond_with(ResponseTemplate::new(302).append_header("location", "/missing"))
        .mount(&site)
        .await;
    site
}

/// What a scrape reports, in a form both modes can be compared by: the status of the page,
/// or the kind of error.
fn scrape_outcome(result: Result<crawlberg::ScrapeResult, CrawlError>) -> Result<u16, String> {
    match result {
        Ok(page) => Ok(page.status_code),
        Err(error) => Err(error_kind(&error)),
    }
}

fn error_kind(error: &CrawlError) -> String {
    format!("{error:?}")
        .split([' ', '{', '('])
        .next()
        .unwrap_or_default()
        .to_owned()
}

fn chrome_missing(test_name: &str, error: &CrawlError) -> bool {
    match error {
        CrawlError::BrowserError { message, .. } if is_missing_chrome_message(message) => {
            announce_chrome_skip(test_name, message);
            true
        }
        _ => false,
    }
}

/// Scrape each route in HTTP mode and in browser mode with `backend`, and require the same
/// outcome. Returns `false` when Chrome is missing.
async fn assert_scrape_matches_http_mode(test_name: &str, backend: BrowserBackend) -> bool {
    let site = site().await;
    let http = create_engine(Some(config(backend.clone(), BrowserMode::Never))).expect("engine must build");
    let browser = create_engine(Some(config(backend, BrowserMode::Always))).expect("engine must build");
    for (route, expected) in [
        ("/ok", Ok(200)),
        ("/missing", Err("NotFound".to_owned())),
        ("/broken", Err("ServerError".to_owned())),
        ("/moved", Ok(404)),
    ] {
        let url = format!("{}{route}", site.uri());
        let http_outcome = scrape_outcome(scrape(&http, &url).await);
        assert_eq!(http_outcome, expected, "{test_name}: HTTP mode for {route}");
        let browser_result = scrape(&browser, &url).await;
        if let Err(error) = &browser_result
            && chrome_missing(test_name, error)
        {
            return false;
        }
        assert_eq!(
            scrape_outcome(browser_result),
            http_outcome,
            "{test_name}: browser mode must report {route} as HTTP mode does"
        );
    }
    true
}

#[tokio::test]
async fn chromiumoxide_scrape_reports_the_document_status_as_http_mode_does() {
    assert_scrape_matches_http_mode(
        "chromiumoxide_scrape_reports_the_document_status_as_http_mode_does",
        BrowserBackend::Chromiumoxide,
    )
    .await;
}

#[cfg(feature = "browser-native")]
#[tokio::test]
async fn native_scrape_reports_the_document_status_as_http_mode_does() {
    assert_scrape_matches_http_mode(
        "native_scrape_reports_the_document_status_as_http_mode_does",
        BrowserBackend::Native,
    )
    .await;
}

/// With `soft_http_errors`, a 404 page is a page with status 404 and no body in both modes.
#[tokio::test]
async fn chromiumoxide_soft_http_errors_report_a_404_page_as_http_mode_does() {
    let test_name = "chromiumoxide_soft_http_errors_report_a_404_page_as_http_mode_does";
    let site = site().await;
    let url = format!("{}/missing", site.uri());
    let soft = |mode| CrawlConfig {
        soft_http_errors: true,
        ..config(BrowserBackend::Chromiumoxide, mode)
    };
    let http = create_engine(Some(soft(BrowserMode::Never))).expect("engine must build");
    let expected = scrape(&http, &url).await.expect("a soft 404 is a page");
    let browser = create_engine(Some(soft(BrowserMode::Always))).expect("engine must build");
    let result = match scrape(&browser, &url).await {
        Ok(result) => result,
        Err(error) if chrome_missing(test_name, &error) => return,
        Err(error) => panic!("{test_name}: a soft 404 is a page: {error:?}"),
    };
    assert_eq!(
        (result.status_code, result.html.as_str()),
        (expected.status_code, expected.html.as_str())
    );
}

/// A crawl in browser mode keeps the pages, with the statuses, that HTTP mode keeps.
#[tokio::test]
async fn chromiumoxide_crawl_keeps_the_pages_http_mode_keeps() {
    let test_name = "chromiumoxide_crawl_keeps_the_pages_http_mode_keeps";
    let site = site().await;
    let seed = format!("{}/", site.uri());
    let pages = |result: crawlberg::CrawlResult| {
        let mut pages: Vec<(String, u16)> = result
            .pages
            .iter()
            .map(|page| (page.url.trim_start_matches(&site.uri()).to_owned(), page.status_code))
            .collect();
        pages.sort();
        pages
    };
    let http = create_engine(Some(config(BrowserBackend::Chromiumoxide, BrowserMode::Never))).expect("engine");
    let expected = pages(crawl(&http, &seed).await.expect("HTTP mode needs no Chrome"));
    assert!(
        expected.contains(&("/ok".to_owned(), 200)),
        "HTTP mode keeps the 200 page: {expected:?}"
    );
    let browser = create_engine(Some(config(BrowserBackend::Chromiumoxide, BrowserMode::Always))).expect("engine");
    let result = match crawl(&browser, &seed).await {
        Ok(result) => result,
        Err(error) if chrome_missing(test_name, &error) => return,
        Err(error) => panic!("{test_name}: crawl must succeed: {error:?}"),
    };
    if let Some(message) = result.error.as_deref()
        && is_missing_chrome_message(message)
    {
        announce_chrome_skip(test_name, message);
        return;
    }
    assert_eq!(pages(result), expected, "{test_name}");
}

/// Scrape `route` of a site whose routes answer `(route, status, body)` in browser mode, with
/// `extra_wait`, or `None` when Chrome is missing.
async fn browser_scrape(
    test_name: &str,
    routes: &[(&str, u16, &str)],
    route: &str,
    extra_wait: Option<Duration>,
) -> Option<Result<crawlberg::ScrapeResult, CrawlError>> {
    let site = MockServer::start().await;
    for (path_, status, body) in routes {
        Mock::given(method("GET"))
            .and(path(*path_))
            .respond_with(page(*status, body))
            .mount(&site)
            .await;
    }
    let mut config = config(BrowserBackend::Chromiumoxide, BrowserMode::Always);
    config.browser.extra_wait = extra_wait;
    let engine = create_engine(Some(config)).expect("engine must build");
    let result = scrape(&engine, &format!("{}{route}", site.uri())).await;
    if let Err(error) = &result
        && chrome_missing(test_name, error)
    {
        return None;
    }
    Some(result)
}

/// A 403 challenge page that moves to a 200 page during the extra wait is the 200 page: the
/// status is the one of the document whose HTML is returned.
#[tokio::test]
async fn chromiumoxide_reports_the_status_of_the_document_it_returns() {
    let test_name = "chromiumoxide_reports_the_status_of_the_document_it_returns";
    let routes = [
        (
            "/challenge",
            403,
            "<p>checking</p><script>setTimeout(() => location.replace('/solved'), 500)</script>",
        ),
        ("/solved", 200, "<p>solved-marker</p>"),
    ];
    let Some(result) = browser_scrape(test_name, &routes, "/challenge", Some(Duration::from_secs(3))).await else {
        return;
    };
    let page = result.unwrap_or_else(|error| panic!("{test_name}: the solved page is a page: {error:?}"));
    assert_eq!(page.status_code, 200, "{test_name}");
    assert!(page.html.contains("solved-marker"), "{test_name}: {}", page.html);
}

/// The status is the main document's: a failing iframe or image inside a 200 page does not
/// change it.
#[tokio::test]
async fn chromiumoxide_reports_the_main_document_status_not_a_frame_or_image_status() {
    let test_name = "chromiumoxide_reports_the_main_document_status_not_a_frame_or_image_status";
    let routes = [
        (
            "/",
            200,
            r#"<p>main-marker</p><iframe src="/frame"></iframe><img src="/missing.png">"#,
        ),
        ("/frame", 500, "<p>frame trouble</p>"),
        ("/missing.png", 404, ""),
    ];
    let Some(result) = browser_scrape(test_name, &routes, "/", None).await else {
        return;
    };
    let page = result.unwrap_or_else(|error| panic!("{test_name}: the 200 page is a page: {error:?}"));
    assert_eq!(page.status_code, 200, "{test_name}");
    assert!(page.html.contains("main-marker"), "{test_name}: {}", page.html);
}
