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

/// `/` links to `/ok`, `/missing` and `/broken`, which answer 200, 404 and 500 with a page, and
/// to `/moved`, which redirects to `/missing`. `/waf` is a 403 page that only its headers fingerprint as
/// a WAF block.
async fn site() -> MockServer {
    let site = MockServer::start().await;
    let routes = [
        (
            "/",
            200,
            r#"<a href="/ok">ok</a><a href="/missing">missing</a><a href="/broken">broken</a><a href="/moved">moved</a>"#,
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
        .and(path("/waf"))
        .respond_with(page(403, "<p>forbidden-marker</p>").append_header("x-datadome", "protected"))
        .mount(&site)
        .await;
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
    match error {
        CrawlError::NotFound { .. } => "NotFound",
        CrawlError::Forbidden { .. } => "Forbidden",
        CrawlError::WafBlocked { .. } => "WafBlocked",
        CrawlError::ServerError { .. } => "ServerError",
        other => return format!("unexpected: {other:?}"),
    }
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
        ("/waf", Err("WafBlocked".to_owned())),
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

/// With `soft_http_errors`, a 404 or 403 page is a page with its status and no body in both
/// modes.
#[tokio::test]
async fn chromiumoxide_soft_http_errors_report_a_404_or_403_page_as_http_mode_does() {
    let test_name = "chromiumoxide_soft_http_errors_report_a_404_or_403_page_as_http_mode_does";
    let site = site().await;
    let soft = |mode| CrawlConfig {
        soft_http_errors: true,
        ..config(BrowserBackend::Chromiumoxide, mode)
    };
    let http = create_engine(Some(soft(BrowserMode::Never))).expect("engine must build");
    let browser = create_engine(Some(soft(BrowserMode::Always))).expect("engine must build");
    for (route, status) in [("/missing", 404), ("/waf", 403)] {
        let url = format!("{}{route}", site.uri());
        let expected = scrape(&http, &url)
            .await
            .unwrap_or_else(|error| panic!("{test_name}: HTTP mode reports a soft {route} as a page: {error:?}"));
        assert_eq!(expected.status_code, status, "{test_name}: HTTP mode for {route}");
        let result = match scrape(&browser, &url).await {
            Ok(result) => result,
            Err(error) if chrome_missing(test_name, &error) => return,
            Err(error) => panic!("{test_name}: a soft {route} is a page: {error:?}"),
        };
        assert_eq!(
            (result.status_code, result.html.as_str()),
            (expected.status_code, expected.html.as_str()),
            "{test_name}: browser mode for {route}"
        );
    }
}

/// A crawl in browser mode with `backend` keeps the pages, with the statuses, that HTTP mode
/// keeps. The crawl follows the redirect from `/moved` itself before the browser fetch, so this
/// does not test how a backend reports a redirect; the unit tests in `browser.rs` and
/// `browser/navigation.rs` do.
async fn assert_crawl_matches_http_mode(test_name: &str, backend: BrowserBackend) {
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
    let http = create_engine(Some(config(backend.clone(), BrowserMode::Never))).expect("engine");
    let expected = pages(crawl(&http, &seed).await.expect("HTTP mode needs no Chrome"));
    assert!(
        expected.contains(&("/ok".to_owned(), 200)),
        "HTTP mode keeps the 200 page: {expected:?}"
    );
    let browser = create_engine(Some(config(backend, BrowserMode::Always))).expect("engine");
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

#[tokio::test]
async fn chromiumoxide_crawl_keeps_the_pages_http_mode_keeps() {
    assert_crawl_matches_http_mode(
        "chromiumoxide_crawl_keeps_the_pages_http_mode_keeps",
        BrowserBackend::Chromiumoxide,
    )
    .await;
}

#[cfg(feature = "browser-native")]
#[tokio::test]
async fn native_crawl_keeps_the_pages_http_mode_keeps() {
    assert_crawl_matches_http_mode("native_crawl_keeps_the_pages_http_mode_keeps", BrowserBackend::Native).await;
}

/// Scrape `route` of a site whose routes answer `(route, status, body)` in browser mode, with
/// `extra_wait`. Returns the result and the site, or `None` when Chrome is missing.
async fn browser_scrape(
    test_name: &str,
    routes: &[(&str, u16, &str)],
    route: &str,
    extra_wait: Option<Duration>,
) -> Option<(Result<crawlberg::ScrapeResult, CrawlError>, MockServer)> {
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
    browser_scrape_with(test_name, config, &site, route)
        .await
        .map(|result| (result, site))
}

/// Scrape `route` of `site` with `config`, or `None` when Chrome is missing.
async fn browser_scrape_with(
    test_name: &str,
    config: CrawlConfig,
    site: &MockServer,
    route: &str,
) -> Option<Result<crawlberg::ScrapeResult, CrawlError>> {
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
    let Some((result, _site)) = browser_scrape(test_name, &routes, "/challenge", Some(Duration::from_secs(3))).await
    else {
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
    let Some((result, site)) = browser_scrape(test_name, &routes, "/", None).await else {
        return;
    };
    let page = result.unwrap_or_else(|error| panic!("{test_name}: the 200 page is a page: {error:?}"));
    let requested: Vec<String> = site
        .received_requests()
        .await
        .expect("request recording is on")
        .iter()
        .map(|request| request.url.path().to_owned())
        .collect();
    for subresource in ["/frame", "/missing.png"] {
        assert!(
            requested.iter().any(|path| path == subresource),
            "{test_name}: the page must have requested {subresource}, or the test proves nothing: {requested:?}"
        );
    }
    assert_eq!(page.status_code, 200, "{test_name}");
    assert!(page.html.contains("main-marker"), "{test_name}: {}", page.html);
}

/// A page that navigates itself to an address the SSRF policy refuses, after its load, fails
/// the fetch as a refused navigation does: the page Chrome then shows is its own error page.
/// This holds during `extra_wait` and, with no extra wait, for a navigation 50 ms after the load.
#[tokio::test]
async fn chromiumoxide_refuses_a_page_that_navigates_to_a_denied_address_after_the_load() {
    let test_name = "chromiumoxide_refuses_a_page_that_navigates_to_a_denied_address_after_the_load";
    for (delay_ms, extra_wait) in [(300, Some(Duration::from_secs(2))), (50, None)] {
        let Some((result, _site)) = scrape_start_page(
            test_name,
            loopback_only(extra_wait),
            &format!(
                "<script>setTimeout(() => location.assign('http://169.254.169.254/latest/'), {delay_ms})</script>"
            ),
            vec![],
        )
        .await
        else {
            return;
        };
        match result {
            Err(CrawlError::SsrfPolicyViolation { url, .. }) => {
                assert!(url.contains("169.254.169.254"), "{test_name}: {delay_ms} ms: {url}");
            }
            other => panic!("{test_name}: {delay_ms} ms: the refused navigation must fail the fetch: {other:?}"),
        }
    }
}

/// A challenge status (429, 503) whose headers name a WAF is a WAF block in both modes, so it
/// escalates instead of being retried: from Chrome's error page for an empty body, and from a
/// page Chrome renders.
#[tokio::test]
async fn chromiumoxide_reports_a_waf_challenge_status_as_http_mode_does() {
    let test_name = "chromiumoxide_reports_a_waf_challenge_status_as_http_mode_does";
    let site = MockServer::start().await;
    for status in [429_u16, 503] {
        let empty = ResponseTemplate::new(status)
            .append_header("content-length", "0")
            .append_header("x-datadome", "blocked");
        let rendered = page(status, "<p>challenge-marker</p>").append_header("x-datadome", "blocked");
        for (route, response) in [
            (format!("/empty-{status}"), empty),
            (format!("/page-{status}"), rendered),
        ] {
            Mock::given(method("GET"))
                .and(path(route))
                .respond_with(response)
                .mount(&site)
                .await;
        }
    }
    let http =
        create_engine(Some(config(BrowserBackend::Chromiumoxide, BrowserMode::Never))).expect("engine must build");
    let browser =
        create_engine(Some(config(BrowserBackend::Chromiumoxide, BrowserMode::Always))).expect("engine must build");
    for status in [429_u16, 503] {
        for kind in ["empty", "page"] {
            let url = format!("{}/{kind}-{status}", site.uri());
            let expected = scrape_outcome(scrape(&http, &url).await);
            assert_eq!(
                expected,
                Err("WafBlocked".to_owned()),
                "{test_name}: HTTP mode for the {kind} {status}"
            );
            let result = scrape(&browser, &url).await;
            if let Err(error) = &result
                && chrome_missing(test_name, error)
            {
                return;
            }
            assert_eq!(
                scrape_outcome(result),
                expected,
                "{test_name}: browser mode for the {kind} {status}"
            );
        }
    }
}

/// A challenge status (403, 429, 503) whose page only its body names as a WAF is a WAF block in
/// both modes: browser mode checks the body Chrome rendered, as HTTP mode checks the body it read.
/// `server: cloudflare` alone does not name a WAF, so only the Cloudflare challenge body decides.
#[tokio::test]
async fn chromiumoxide_reports_a_challenge_page_only_its_body_names_as_http_mode_does() {
    let test_name = "chromiumoxide_reports_a_challenge_page_only_its_body_names_as_http_mode_does";
    let body = "<html><head><title>Just a moment...</title></head><body>\
                <script src=\"/cdn-cgi/challenge-platform/h/g/orchestrate/chl_page/v1\"></script></body></html>";
    let site = MockServer::start().await;
    for status in [403_u16, 429, 503] {
        Mock::given(method("GET"))
            .and(path(format!("/{status}")))
            .respond_with(
                ResponseTemplate::new(status)
                    .append_header("server", "cloudflare")
                    .set_body_raw(body, "text/html"),
            )
            .mount(&site)
            .await;
    }
    let http =
        create_engine(Some(config(BrowserBackend::Chromiumoxide, BrowserMode::Never))).expect("engine must build");
    let browser =
        create_engine(Some(config(BrowserBackend::Chromiumoxide, BrowserMode::Always))).expect("engine must build");
    for status in [403_u16, 429, 503] {
        let url = format!("{}/{status}", site.uri());
        let expected = scrape_outcome(scrape(&http, &url).await);
        assert_eq!(
            expected,
            Err("WafBlocked".to_owned()),
            "{test_name}: HTTP mode for the {status}"
        );
        let result = scrape(&browser, &url).await;
        if let Err(error) = &result
            && chrome_missing(test_name, error)
        {
            return;
        }
        assert_eq!(
            scrape_outcome(result),
            expected,
            "{test_name}: browser mode for the {status}"
        );
    }
}

/// Browser mode with the default SSRF policy, except that loopback is allowed so the mock site
/// loads. The metadata address stays denied.
fn loopback_only(extra_wait: Option<Duration>) -> CrawlConfig {
    let mut config = CrawlConfig {
        ssrf: crawlberg::SsrfPolicy {
            allowlist: vec![crawlberg::HostMatcher::cidr("127.0.0.0/8").expect("literal CIDR is valid")],
            ..crawlberg::SsrfPolicy::default()
        },
        ..config(BrowserBackend::Chromiumoxide, BrowserMode::Always)
    };
    config.browser.extra_wait = extra_wait;
    config
}

/// Scrape `/` of a site whose start page runs `script` and where each of `routes` answers its
/// response. Returns the result and the site, or `None` when Chrome is missing.
async fn scrape_start_page(
    test_name: &str,
    config: CrawlConfig,
    script: &str,
    routes: Vec<(&str, ResponseTemplate)>,
) -> Option<(Result<crawlberg::ScrapeResult, CrawlError>, MockServer)> {
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(page(START_STATUS, &format!("<p>start-marker</p>{script}")))
        .mount(&site)
        .await;
    for (route, response) in routes {
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(response)
            .mount(&site)
            .await;
    }
    browser_scrape_with(test_name, config, &site, "/")
        .await
        .map(|result| (result, site))
}

/// The status of the start page. It is not 200, so the status reported when no response was
/// recorded cannot pass for it.
const START_STATUS: u16 = 203;

/// Require that the start page came back as a page with its own status.
fn assert_start_page(test_name: &str, result: Result<crawlberg::ScrapeResult, CrawlError>) {
    let page = result.unwrap_or_else(|error| panic!("{test_name}: the start page is a page: {error:?}"));
    assert_eq!(page.status_code, START_STATUS, "{test_name}");
    assert!(page.html.contains("start-marker"), "{test_name}: {}", page.html);
}

/// Require that `site` received a request for `route`, or the test observed nothing.
async fn assert_requested(test_name: &str, site: &MockServer, route: &str) {
    let requested = site
        .received_requests()
        .await
        .expect("request recording is on")
        .iter()
        .any(|request| request.url.path() == route);
    assert!(
        requested,
        "{test_name}: the page must have requested {route}, or the test proves nothing"
    );
}

/// A navigation Chrome does not commit leaves the start page in place, so the start page's
/// status stays: a 204 during the extra wait is not the page's status.
#[tokio::test]
async fn chromiumoxide_keeps_the_status_when_a_late_navigation_commits_no_document() {
    let test_name = "chromiumoxide_keeps_the_status_when_a_late_navigation_commits_no_document";
    let mut config = config(BrowserBackend::Chromiumoxide, BrowserMode::Always);
    config.browser.extra_wait = Some(Duration::from_secs(2));
    let Some((result, site)) = scrape_start_page(
        test_name,
        config,
        "<script>setTimeout(() => location.assign('/nocontent'), 300)</script>",
        vec![("/nocontent", ResponseTemplate::new(204))],
    )
    .await
    else {
        return;
    };
    assert_requested(test_name, &site, "/nocontent").await;
    assert_start_page(test_name, result);
}

/// ~keep A response without a document that arrives before the start page's delayed load event
/// ~keep cannot leave chromiumoxide waiting for that event. The start page remains committed.
#[tokio::test]
async fn chromiumoxide_returns_the_start_page_when_a_pre_load_navigation_commits_no_document() {
    let test_name = "chromiumoxide_returns_the_start_page_when_a_pre_load_navigation_commits_no_document";
    let Some((result, site)) = scrape_start_page(
        test_name,
        config(BrowserBackend::Chromiumoxide, BrowserMode::Always),
        "<img src='/slow.png'><script>setTimeout(() => location.assign('/nocontent'), 300)</script>",
        vec![
            (
                "/slow.png",
                ResponseTemplate::new(200)
                    .set_body_raw("pixel", "image/png")
                    .set_delay(Duration::from_secs(3)),
            ),
            ("/nocontent", ResponseTemplate::new(204)),
        ],
    )
    .await
    else {
        return;
    };
    assert_requested(test_name, &site, "/slow.png").await;
    assert_requested(test_name, &site, "/nocontent").await;
    assert_start_page(test_name, result);
}

#[tokio::test]
async fn chromiumoxide_returns_a_redirect_response_whose_location_is_not_a_web_address() {
    let test_name = "chromiumoxide_returns_a_redirect_response_whose_location_is_not_a_web_address";
    let site = MockServer::start().await;
    for (route, target) in [
        ("/mail", "mailto:someone@example.com"),
        ("/data", "data:text/html,hi"),
        ("/file", "file:///etc/hostname"),
        ("/app", "myapp://open"),
    ] {
        let body = format!(
            "<script>fetch('/script-fired')</script>\
             <meta http-equiv='refresh' content='0;url=/meta-fired'>\
             <img src='/image-fired'><p>{route}-redirect-body</p>"
        );
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(
                ResponseTemplate::new(302)
                    .append_header("location", target)
                    .append_header("x-redirect-marker", route)
                    .append_header("refresh", "0;url=/header-fired")
                    .append_header("content-disposition", "attachment")
                    .set_body_raw(body, "text/html"),
            )
            .mount(&site)
            .await;
    }
    let mut browser_config = config(BrowserBackend::Chromiumoxide, BrowserMode::Always);
    browser_config.browser.timeout = Duration::from_secs(3);
    for (route, target) in [
        ("/mail", "mailto:someone@example.com"),
        ("/data", "data:text/html,hi"),
        ("/file", "file:///etc/hostname"),
        ("/app", "myapp://open"),
    ] {
        let Some(result) = browser_scrape_with(test_name, browser_config.clone(), &site, route).await else {
            return;
        };
        let page = result.unwrap_or_else(|error| panic!("{test_name}: {target}: {error:?}"));
        let expected_body = format!(
            "<script>fetch('/script-fired')</script>\
             <meta http-equiv='refresh' content='0;url=/meta-fired'>\
             <img src='/image-fired'><p>{route}-redirect-body</p>"
        );
        assert_eq!(
            (page.status_code, page.html.as_str()),
            (302, expected_body.as_str()),
            "{test_name}: {target}"
        );
        assert_eq!(page.content_type, "text/html", "{test_name}: {target}");
        assert_eq!(page.redirect_count, 0, "{test_name}: {target}");
        assert!(
            page.final_url.ends_with(route),
            "{test_name}: {target}: {}",
            page.final_url
        );
    }
    let active_requests: Vec<_> = site
        .received_requests()
        .await
        .expect("request recording is enabled")
        .into_iter()
        .filter(|request| {
            matches!(
                request.url.path(),
                "/script-fired" | "/meta-fired" | "/image-fired" | "/header-fired"
            )
        })
        .collect();
    assert!(
        active_requests.is_empty(),
        "terminal redirect content and navigation headers must be inert: {active_requests:?}"
    );
}

#[tokio::test]
async fn chromiumoxide_returns_a_late_non_web_redirect_response() {
    let test_name = "chromiumoxide_returns_a_late_non_web_redirect_response";
    let body = "<p>late-redirect-body</p>";
    let Some((result, site)) = scrape_start_page(
        test_name,
        late_navigation_config(),
        "<script>setTimeout(() => location.assign('/late'), 300)</script>",
        vec![(
            "/late",
            ResponseTemplate::new(302)
                .append_header("location", "mailto:someone@example.com")
                .append_header("x-redirect-marker", "late")
                .set_body_raw(body, "text/html"),
        )],
    )
    .await
    else {
        return;
    };
    assert_requested(test_name, &site, "/late").await;
    let page = result.unwrap_or_else(|error| panic!("{test_name}: {error:?}"));
    assert_eq!((page.status_code, page.html.as_str()), (302, body), "{test_name}");
    assert_eq!(page.content_type, "text/html", "{test_name}");
    assert_eq!(page.redirect_count, 1, "{test_name}");
    assert!(page.final_url.ends_with("/late"), "{test_name}: {}", page.final_url);
}

/// A document an iframe commits is not the page's: after an iframe loads during the extra wait,
/// a main-frame navigation that commits nothing leaves the start page and its status in place.
#[tokio::test]
async fn chromiumoxide_keeps_the_status_when_an_iframe_commits_before_a_late_navigation() {
    let test_name = "chromiumoxide_keeps_the_status_when_an_iframe_commits_before_a_late_navigation";
    let mut config = config(BrowserBackend::Chromiumoxide, BrowserMode::Always);
    config.browser.extra_wait = Some(Duration::from_secs(2));
    let Some((result, site)) = scrape_start_page(
        test_name,
        config,
        "<script>setTimeout(() => { const frame = document.createElement('iframe'); \
         frame.onload = () => location.assign('/nocontent'); frame.src = '/frame'; \
         document.body.appendChild(frame); }, 300)</script>",
        vec![
            ("/frame", page(200, "<p>frame-marker</p>")),
            ("/nocontent", ResponseTemplate::new(204)),
        ],
    )
    .await
    else {
        return;
    };
    assert_requested(test_name, &site, "/frame").await;
    assert_requested(test_name, &site, "/nocontent").await;
    assert_start_page(test_name, result);
}

/// A download is not a document either: a 202 download during the extra wait leaves the start
/// page and its status in place.
#[tokio::test]
async fn chromiumoxide_keeps_the_page_when_a_late_navigation_is_a_download() {
    let test_name = "chromiumoxide_keeps_the_page_when_a_late_navigation_is_a_download";
    let mut config = config(BrowserBackend::Chromiumoxide, BrowserMode::Always);
    config.browser.extra_wait = Some(Duration::from_secs(2));
    let download = ResponseTemplate::new(202)
        .set_body_raw("bin", "application/octet-stream")
        .append_header("content-disposition", "attachment; filename=x.bin");
    let Some((result, site)) = scrape_start_page(
        test_name,
        config,
        "<script>setTimeout(() => location.assign('/download'), 300)</script>",
        vec![("/download", download)],
    )
    .await
    else {
        return;
    };
    assert_requested(test_name, &site, "/download").await;
    assert_start_page(test_name, result);
}

/// An image to a denied address during the load is failed, and the page stays a page.
#[tokio::test]
async fn chromiumoxide_keeps_the_page_when_an_image_is_refused_during_the_load() {
    let test_name = "chromiumoxide_keeps_the_page_when_an_image_is_refused_during_the_load";
    let Some((result, _site)) = scrape_start_page(
        test_name,
        loopback_only(None),
        r#"<img src="http://169.254.169.254/x.png">"#,
        vec![],
    )
    .await
    else {
        return;
    };
    assert_start_page(test_name, result);
}

/// An image to a denied address after the load is failed, and the page stays a page.
#[tokio::test]
async fn chromiumoxide_keeps_the_page_when_an_image_is_refused_after_the_load() {
    let test_name = "chromiumoxide_keeps_the_page_when_an_image_is_refused_after_the_load";
    let Some((result, _site)) = scrape_start_page(
        test_name,
        loopback_only(Some(Duration::from_secs(2))),
        "<script>setTimeout(() => { const image = new Image(); image.src = 'http://169.254.169.254/x.png'; \
         document.body.appendChild(image); }, 300)</script>",
        vec![],
    )
    .await
    else {
        return;
    };
    assert_start_page(test_name, result);
}

/// An iframe navigated to a denied address after the load is failed, and the page stays a page:
/// only a main-frame navigation replaces the page.
#[tokio::test]
async fn chromiumoxide_keeps_the_page_when_an_iframe_is_refused_after_the_load() {
    let test_name = "chromiumoxide_keeps_the_page_when_an_iframe_is_refused_after_the_load";
    let Some((result, _site)) = scrape_start_page(
        test_name,
        loopback_only(Some(Duration::from_secs(2))),
        "<script>setTimeout(() => { const frame = document.createElement('iframe'); \
         frame.src = 'http://169.254.169.254/'; document.body.appendChild(frame); }, 300)</script>",
        vec![],
    )
    .await
    else {
        return;
    };
    assert_start_page(test_name, result);
}

/// Browser mode with a two-second extra wait, so a navigation the start page starts late lands
/// before the HTML is read.
fn late_navigation_config() -> CrawlConfig {
    late_navigation_config_with(BrowserMode::Always)
}

fn late_navigation_config_with(mode: BrowserMode) -> CrawlConfig {
    let mut config = config(BrowserBackend::Chromiumoxide, mode);
    config.browser.extra_wait = Some(Duration::from_secs(2));
    config
}

/// A download at `/dl` that answers `status`.
fn download(status: u16) -> ResponseTemplate {
    ResponseTemplate::new(status)
        .set_body_raw("bin", "application/octet-stream")
        .append_header("content-disposition", "attachment; filename=x.bin")
}

const LATE_DOWNLOAD: &str = "<script>setTimeout(() => location.assign('/dl'), 300)</script>";

/// A download whose status is not an error in HTTP mode makes Chrome show its own error page.
/// That page is not the server's content, so the fetch is a page with the status, the headers and
/// the URL of the download and no body.
#[tokio::test]
async fn chromiumoxide_reports_a_late_download_chrome_does_not_render_as_a_page_without_a_body() {
    let test_name = "chromiumoxide_reports_a_late_download_chrome_does_not_render_as_a_page_without_a_body";
    for status in [501, 505, 599] {
        let Some((result, site)) = scrape_start_page(
            test_name,
            late_navigation_config(),
            LATE_DOWNLOAD,
            vec![("/dl", download(status))],
        )
        .await
        else {
            return;
        };
        assert_requested(test_name, &site, "/dl").await;
        let page = result.unwrap_or_else(|error| panic!("{test_name}: {status}: the download is a page: {error:?}"));
        assert_eq!((page.status_code, page.html.as_str()), (status, ""), "{test_name}");
        assert!(
            page.final_url.ends_with("/dl"),
            "{test_name}: {status}: {}",
            page.final_url
        );
    }
}

/// A seed that answers an error status with an empty body makes Chrome show its own error page.
/// The scrape still reports what HTTP mode reports: a page with that status and no body.
#[tokio::test]
async fn chromiumoxide_reports_an_empty_error_response_as_http_mode_does() {
    let test_name = "chromiumoxide_reports_an_empty_error_response_as_http_mode_does";
    let site = MockServer::start().await;
    let statuses = [400_u16, 405, 409, 422, 451, 501];
    for status in statuses {
        Mock::given(method("GET"))
            .and(path(format!("/{status}")))
            .respond_with(ResponseTemplate::new(status).append_header("content-length", "0"))
            .mount(&site)
            .await;
    }
    let http = create_engine(Some(late_navigation_config_with(BrowserMode::Never))).expect("engine must build");
    let browser = create_engine(Some(late_navigation_config_with(BrowserMode::Always))).expect("engine must build");
    for status in statuses {
        let url = format!("{}/{status}", site.uri());
        let expected = scrape(&http, &url)
            .await
            .unwrap_or_else(|error| panic!("{test_name}: HTTP mode reports {status} as a page: {error:?}"));
        assert_eq!(
            (expected.status_code, expected.html.as_str()),
            (status, ""),
            "{test_name}: HTTP mode"
        );
        let page = match scrape(&browser, &url).await {
            Ok(page) => page,
            Err(error) if chrome_missing(test_name, &error) => return,
            Err(error) => panic!("{test_name}: {status} is a page in HTTP mode: {error:?}"),
        };
        assert_eq!(
            (page.status_code, page.html.as_str()),
            (expected.status_code, expected.html.as_str()),
            "{test_name}: browser mode"
        );
    }
}

/// A download whose status HTTP mode raises as an error fails with that error, a WAF found from
/// its headers included. Under `soft_http_errors` a 404 download is a page with its status, its
/// own URL and no body, as in HTTP mode.
#[tokio::test]
async fn chromiumoxide_reports_a_late_error_download_as_http_mode_does() {
    let test_name = "chromiumoxide_reports_a_late_error_download_as_http_mode_does";
    let waf = download(403).append_header("x-datadome", "protected");
    for (status, response, expected) in [
        (500, download(500), "ServerError"),
        (404, download(404), "NotFound"),
        (403, download(403), "Forbidden"),
        (403, waf, "WafBlocked"),
    ] {
        let Some((result, site)) = scrape_start_page(
            test_name,
            late_navigation_config(),
            LATE_DOWNLOAD,
            vec![("/dl", response)],
        )
        .await
        else {
            return;
        };
        assert_requested(test_name, &site, "/dl").await;
        assert_eq!(
            scrape_outcome(result),
            Err(expected.to_owned()),
            "{test_name}: {status} {expected}"
        );
    }
    let mut soft = late_navigation_config();
    soft.soft_http_errors = true;
    let Some((result, site)) = scrape_start_page(test_name, soft, LATE_DOWNLOAD, vec![("/dl", download(404))]).await
    else {
        return;
    };
    assert_requested(test_name, &site, "/dl").await;
    let page = result.unwrap_or_else(|error| panic!("{test_name}: a soft 404 is a page: {error:?}"));
    assert_eq!((page.status_code, page.html.as_str()), (404, ""), "{test_name}");
    assert!(page.final_url.ends_with("/dl"), "{test_name}: {}", page.final_url);
}

/// A late navigation that fails at the network leaves Chrome's error page with no response, so the
/// fetch fails instead of reporting status 200 with that page as content.
#[tokio::test]
async fn chromiumoxide_fails_when_a_late_navigation_fails_at_the_network() {
    let test_name = "chromiumoxide_fails_when_a_late_navigation_fails_at_the_network";
    let closed_port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .expect("a free local port")
        .port();
    let unreachable = format!("http://127.0.0.1:{closed_port}/gone");
    let Some((result, _site)) = scrape_start_page(
        test_name,
        late_navigation_config(),
        &format!("<script>setTimeout(() => location.assign('{unreachable}'), 300)</script>"),
        vec![],
    )
    .await
    else {
        return;
    };
    match result {
        Err(CrawlError::BrowserError { message, .. }) => {
            assert!(
                message.contains(&unreachable) && !message.contains("HTTP "),
                "{test_name}: no response arrived, so no status: {message}"
            );
        }
        other => panic!("{test_name}: the failed navigation must fail the fetch, not report status 200: {other:?}"),
    }
}

/// A site whose `/` starts a late navigation to `target`. `/seed` redirects to `/`, `/dl` is a
/// 404 download, `/gone` a 404 page, and `/to-dl` and `/to-gone` redirect to them.
async fn late_404_site(target: &str) -> MockServer {
    let site = MockServer::start().await;
    let late = format!("<script>setTimeout(() => location.assign('{target}'), 300)</script>");
    let routes = [
        ("/seed", ResponseTemplate::new(302).append_header("location", "/")),
        ("/", page(START_STATUS, &format!("<p>start-marker</p>{late}"))),
        ("/dl", download(404)),
        ("/gone", page(404, "<p>gone-marker</p>")),
        ("/to-dl", ResponseTemplate::new(302).append_header("location", "/dl")),
        (
            "/to-gone",
            ResponseTemplate::new(302).append_header("location", "/gone"),
        ),
    ];
    for (route, response) in routes {
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(response)
            .mount(&site)
            .await;
    }
    site
}

/// A 404 at the end of a redirect is a page in HTTP mode, and only the redirects of the late
/// navigation count: the seed's redirect does not make a late 404 a page, and a late navigation
/// that redirects to a 404 is one.
#[tokio::test]
async fn chromiumoxide_counts_only_the_late_navigation_s_redirects_for_a_404() {
    let test_name = "chromiumoxide_counts_only_the_late_navigation_s_redirects_for_a_404";
    for (seed, target, reached) in [("/seed", "/dl", "/dl"), ("/seed", "/gone", "/gone")] {
        let site = late_404_site(target).await;
        let Some(result) = browser_scrape_with(test_name, late_navigation_config(), &site, seed).await else {
            return;
        };
        assert_requested(test_name, &site, "/").await;
        assert_requested(test_name, &site, reached).await;
        assert_eq!(
            scrape_outcome(result),
            Err("NotFound".to_owned()),
            "{test_name}: the seed redirected, then a late navigation to {target} answered 404"
        );
    }
    for (target, reached) in [("/to-dl", "/dl"), ("/to-gone", "/gone")] {
        let site = late_404_site(target).await;
        let Some(result) = browser_scrape_with(test_name, late_navigation_config(), &site, "/").await else {
            return;
        };
        assert_requested(test_name, &site, reached).await;
        let page = result.unwrap_or_else(|error| {
            panic!("{test_name}: a late navigation that redirected to a 404 is a page: {error:?}")
        });
        assert_eq!(
            (page.status_code, page.html.as_str()),
            (404, ""),
            "{test_name}: {target}"
        );
        assert!(page.final_url.ends_with(reached), "{test_name}: {}", page.final_url);
    }
}

/// The error of a late navigation names the URL it failed on: when its connection fails, when a
/// download Chrome refuses answers 404, and when the SSRF policy refuses a late navigation or a
/// seed's redirect.
#[tokio::test]
async fn chromiumoxide_names_the_url_of_a_late_navigation_in_its_error() {
    let test_name = "chromiumoxide_names_the_url_of_a_late_navigation_in_its_error";
    let closed_port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .expect("a free local port")
        .port();
    let late_assign = |target: &str| format!("<script>setTimeout(() => location.assign('{target}'), 300)</script>");
    let mut outcomes = Vec::new();
    let unreachable = format!("http://127.0.0.1:{closed_port}/gone");
    let Some((result, _site)) =
        scrape_start_page(test_name, late_navigation_config(), &late_assign(&unreachable), vec![]).await
    else {
        return;
    };
    outcomes.push(("closed port", unreachable, result));
    let Some((result, site)) = scrape_start_page(
        test_name,
        late_navigation_config(),
        LATE_DOWNLOAD,
        vec![("/dl", download(404))],
    )
    .await
    else {
        return;
    };
    assert_requested(test_name, &site, "/dl").await;
    outcomes.push(("404 download", format!("{}/dl", site.uri()), result));
    let Some((result, _site)) = scrape_start_page(
        test_name,
        loopback_only(Some(Duration::from_secs(2))),
        &late_assign("http://169.254.169.254/latest/"),
        vec![],
    )
    .await
    else {
        return;
    };
    outcomes.push((
        "refused navigation",
        "http://169.254.169.254/latest/".to_owned(),
        result,
    ));
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/hop"))
        .respond_with(ResponseTemplate::new(302).append_header("location", "http://169.254.169.254/latest/"))
        .mount(&site)
        .await;
    let Some(result) = browser_scrape_with(test_name, loopback_only(None), &site, "/hop").await else {
        return;
    };
    assert_requested(test_name, &site, "/hop").await;
    outcomes.push((
        "refused seed redirect",
        "http://169.254.169.254/latest/".to_owned(),
        result,
    ));
    for (case, shown, result) in outcomes {
        let error = match result {
            Err(error) => error,
            Ok(page) => panic!(
                "{test_name}: {case}: the navigation must fail the fetch: {}",
                page.status_code
            ),
        };
        let text = format!("{error} {error:?}");
        assert!(
            text.contains(&shown),
            "{test_name}: {case}: the error must name {shown}: {text}"
        );
    }
}

/// A page that navigates on load is reported as the document it navigated to, with that
/// document's status and HTML. The fetch reads which document is committed before it reads the
/// HTML, and reads the HTML again when the document changed, so the HTML read does not go to the
/// document the navigation replaced.
#[tokio::test]
async fn chromiumoxide_reports_the_document_a_page_navigates_to_on_load() {
    let test_name = "chromiumoxide_reports_the_document_a_page_navigates_to_on_load";
    let Some((result, site)) = scrape_start_page(
        test_name,
        config(BrowserBackend::Chromiumoxide, BrowserMode::Always),
        "<script>addEventListener('load', () => location.assign('/next'))</script>",
        vec![(
            "/next",
            page(202, "<p>next-marker</p>").set_delay(Duration::from_millis(300)),
        )],
    )
    .await
    else {
        return;
    };
    assert_requested(test_name, &site, "/next").await;
    let page = result.unwrap_or_else(|error| panic!("{test_name}: the next page is a page: {error:?}"));
    assert_eq!(
        (page.status_code, page.html.contains("next-marker")),
        (202, true),
        "{test_name}: the status and the HTML must be the next page's: {}",
        page.html
    );
}

/// A site whose two pages replace each other every 15 ms: `/one` answers 201 and `/two` 203.
async fn ping_pong_site() -> MockServer {
    let site = MockServer::start().await;
    for (route, status, next) in [("/one", 201, "/two"), ("/two", 203, "/one")] {
        let body = format!("<p>doc{route}</p><script>setTimeout(() => location.replace('{next}'), 15)</script>");
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(page(status, &body))
            .mount(&site)
            .await;
    }
    site
}

/// A page that keeps navigating is reported as one document: its status and its HTML belong to
/// `/one`, or both to `/two`, and so does the final URL read after them. A page that navigates
/// during each read may fail the fetch instead, but never pairs one document's HTML with
/// another's status.
#[tokio::test]
async fn chromiumoxide_reports_the_status_html_and_url_of_one_document() {
    let test_name = "chromiumoxide_reports_the_status_html_and_url_of_one_document";
    let site = ping_pong_site().await;
    let browser =
        create_engine(Some(config(BrowserBackend::Chromiumoxide, BrowserMode::Always))).expect("engine must build");
    let mut pages = 0;
    for attempt in 0..20 {
        let result = scrape(&browser, &format!("{}/one", site.uri())).await;
        let page = match result {
            Ok(page) => page,
            Err(error) if chrome_missing(test_name, &error) => return,
            Err(CrawlError::BrowserError { message, .. }) if message.contains("navigated to a new document") => {
                continue;
            }
            Err(error) => panic!("{test_name}: attempt {attempt}: {error:?}"),
        };
        pages += 1;
        let route = if page.html.contains("doc/one") { "/one" } else { "/two" };
        let expected_status = if route == "/one" { 201 } else { 203 };
        assert_eq!(
            (page.status_code, page.final_url.ends_with(route)),
            (expected_status, true),
            "{test_name}: attempt {attempt}: the HTML is {route}'s, so the status and URL must be too: {} {}",
            page.final_url,
            page.html
        );
    }
    assert!(pages > 0, "{test_name}: no attempt returned a page");
}

/// A site whose `/blank` shows nothing and whose `/noise` fills the viewport with text. The two
/// replace each other after the number of milliseconds the `ms` query parameter names.
async fn screenshot_site() -> MockServer {
    let mut state: u32 = 12345;
    let noise: String = (0..12_000)
        .map(|_| {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let alphabet = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
            char::from(alphabet[usize::try_from(state >> 16).expect("16 bits") % alphabet.len()])
        })
        .collect();
    let site = MockServer::start().await;
    for (route, next, content) in [
        ("/blank", "/noise", String::new()),
        (
            "/noise",
            "/blank",
            format!("<p style=\"font:8px monospace;word-break:break-all\">{noise}</p>"),
        ),
    ] {
        let body = format!(
            "<p hidden>doc{route}</p>{content}<script>setTimeout(() => location.replace('{next}' + location.search), \
             Number(new URLSearchParams(location.search).get('ms')))</script>"
        );
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(page(200, &body))
            .mount(&site)
            .await;
    }
    site
}

/// A page that replaces its document every 40 ms is reported without a screenshot when Chrome
/// does not answer the screenshot, instead of holding the fetch until its deadline.
#[tokio::test]
async fn chromiumoxide_reports_a_page_that_keeps_navigating_without_waiting_for_its_screenshot() {
    let test_name = "chromiumoxide_reports_a_page_that_keeps_navigating_without_waiting_for_its_screenshot";
    let site = screenshot_site().await;
    let browser = create_engine(Some(CrawlConfig {
        capture_screenshot: true,
        ..config(BrowserBackend::Chromiumoxide, BrowserMode::Always)
    }))
    .expect("engine must build");
    for attempt in 0..8 {
        let started = std::time::Instant::now();
        match scrape(&browser, &format!("{}/blank?ms=40", site.uri())).await {
            Ok(_) => {}
            Err(error) if chrome_missing(test_name, &error) => return,
            Err(CrawlError::BrowserError { message, .. }) if message.contains("navigated to a new document") => {}
            Err(error) => panic!("{test_name}: attempt {attempt}: {error:?}"),
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "{test_name}: attempt {attempt} took {:?}",
            started.elapsed()
        );
    }
}
