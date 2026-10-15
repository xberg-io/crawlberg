#![cfg(feature = "browser")]
//! A result cookie has the same `domain` and `path` in an HTTP and a browser crawl (#753).

use std::time::Duration;

use crawlberg::{BrowserConfig, BrowserMode, CrawlConfig, CrawlResult, crawl, create_engine};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn config(mode: BrowserMode) -> CrawlConfig {
    CrawlConfig {
        browser: BrowserConfig {
            mode,
            timeout: Duration::from_secs(20),
            chrome_path: std::env::var_os("CRAWLBERG_TEST_CHROME").map(Into::into),
            ..BrowserConfig::default()
        },
        cookies_enabled: true,
        respect_robots_txt: false,
        max_concurrent: Some(1),
        ..CrawlConfig::builder().allow_private_networks(true).build()
    }
}

fn html(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(format!("<html><body>{body}</body></html>"), "text/html")
}

async fn route(site: &MockServer, at: &str, page: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(page)
        .mount(site)
        .await;
}

async fn crawl_fields(site: &MockServer, mode: BrowserMode) -> Vec<(String, Option<String>, Option<String>)> {
    let engine = create_engine(Some(config(mode))).expect("engine");
    let result: CrawlResult = crawl(&engine, &format!("{}/", site.uri())).await.expect("crawl");
    assert_eq!(result.error, None);
    let mut fields: Vec<_> = result
        .cookies
        .iter()
        .map(|c| (c.name.clone(), c.domain.clone(), c.path.clone()))
        .collect();
    fields.sort();
    fields
}

fn field(name: &str, domain: &str, path: &str) -> (String, Option<String>, Option<String>) {
    (name.to_owned(), Some(domain.to_owned()), Some(path.to_owned()))
}

#[tokio::test]
async fn should_give_one_cookie_the_same_domain_and_path_in_http_and_browser_mode() {
    let site = MockServer::start().await;
    route(
        &site,
        "/",
        html("<script>document.cookie='js=5'</script><a href='/a/b/c'>deep</a>")
            .append_header("set-cookie", "plain=1")
            .append_header("set-cookie", "ip=2; Domain=127.0.0.1")
            .append_header("set-cookie", "scoped=3; Path=/x/")
            .append_header("set-cookie", "dotted=4; Domain=.127.0.0.1"),
    )
    .await;
    route(&site, "/a/b/c", html("deep").append_header("set-cookie", "deep=6")).await;

    let header_cookies = vec![
        field("deep", "127.0.0.1", "/a/b"),
        field("dotted", "127.0.0.1", "/"),
        field("ip", "127.0.0.1", "/"),
        field("plain", "127.0.0.1", "/"),
        field("scoped", "127.0.0.1", "/x/"),
    ];
    assert_eq!(
        crawl_fields(&site, BrowserMode::Never).await,
        header_cookies,
        "HTTP mode"
    );

    let mut browser_cookies = header_cookies;
    browser_cookies.push(field("js", "127.0.0.1", "/"));
    browser_cookies.sort();
    assert_eq!(
        crawl_fields(&site, BrowserMode::Always).await,
        browser_cookies,
        "browser mode"
    );
}

/// ~keep RFC 6265 5.3: a `Max-Age` of zero or a past `Expires` deletes the cookie, an IP host
/// accepts only its own address as `Domain`, and the last `Domain` attribute decides. The browser
/// lists what its store holds; HTTP mode lists the same.
#[tokio::test]
async fn should_list_the_cookies_a_browser_stores_for_deleted_and_mis_scoped_set_cookie_headers() {
    let site = MockServer::start().await;
    route(
        &site,
        "/",
        html("plain")
            .append_header("set-cookie", "plain=1")
            .append_header("set-cookie", "gone=0; Max-Age=0")
            .append_header("set-cookie", "expired=0; Expires=Thu, 01 Jan 1970 00:00:00 GMT")
            .append_header("set-cookie", "ipsuffix=15; Domain=0.0.1")
            .append_header("set-cookie", "lastwins=16; Domain=other.test; Domain=127.0.0.1"),
    )
    .await;

    let stored = vec![field("lastwins", "127.0.0.1", "/"), field("plain", "127.0.0.1", "/")];
    assert_eq!(crawl_fields(&site, BrowserMode::Never).await, stored, "HTTP mode");
    assert_eq!(crawl_fields(&site, BrowserMode::Always).await, stored, "browser mode");
}

fn from_the_browser(request: &Request) -> bool {
    request.headers.contains_key("upgrade-insecure-requests")
}

/// ~keep The HTTP fetch of the start page gets a 403, so automatic mode fetches it in Chrome;
/// the linked page then comes over HTTP. Each page sets another value of `a`.
#[tokio::test]
async fn should_list_a_cookie_once_when_auto_mode_fetches_one_page_in_chrome_and_one_over_http() {
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(|request: &Request| {
            if from_the_browser(request) {
                html("<a href='/next'>next</a>").append_header("set-cookie", "a=2")
            } else {
                ResponseTemplate::new(403)
            }
        })
        .mount(&site)
        .await;
    route(&site, "/next", html("next").append_header("set-cookie", "a=1")).await;

    let fields = crawl_fields(&site, BrowserMode::Auto).await;

    let fetches: Vec<_> = site
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .map(|r| (r.url.path().to_owned(), from_the_browser(r)))
        .collect();
    assert!(
        fetches.contains(&("/".to_owned(), true)),
        "Chrome must fetch the start page: {fetches:?}"
    );
    assert_eq!(
        fetches.iter().filter(|(at, _)| at == "/next").collect::<Vec<_>>(),
        [&("/next".to_owned(), false)],
        "the linked page must come over HTTP only"
    );
    assert_eq!(fields, vec![field("a", "127.0.0.1", "/")]);
}
