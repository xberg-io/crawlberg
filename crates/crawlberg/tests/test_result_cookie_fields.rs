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
    Mock::given(method("GET")).and(path(at)).respond_with(page).mount(site).await;
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
    assert_eq!(crawl_fields(&site, BrowserMode::Never).await, header_cookies, "HTTP mode");

    let mut browser_cookies = header_cookies;
    browser_cookies.push(field("js", "127.0.0.1", "/"));
    browser_cookies.sort();
    assert_eq!(crawl_fields(&site, BrowserMode::Always).await, browser_cookies, "browser mode");
}

/// ~keep The browser page sets another value: Chrome omits a cookie that a page writes again with
/// the value the crawl already gave it, so only a changed value reaches the result from both modes.
#[tokio::test]
async fn should_list_a_cookie_once_when_auto_mode_fetches_one_page_over_http_and_one_in_chrome() {
    let site = MockServer::start().await;
    route(
        &site,
        "/",
        html("<a href='/next'>next</a>").append_header("set-cookie", "a=1"),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/next"))
        .respond_with(|request: &Request| {
            if request.headers.contains_key("upgrade-insecure-requests") {
                html("next").append_header("set-cookie", "a=2")
            } else {
                ResponseTemplate::new(403)
            }
        })
        .mount(&site)
        .await;

    let fields = crawl_fields(&site, BrowserMode::Auto).await;

    let browser_fetches = site
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| r.url.path() == "/next" && r.headers.contains_key("upgrade-insecure-requests"))
        .count();
    assert_eq!(browser_fetches, 1, "Chrome must fetch the second page");
    assert_eq!(fields, vec![field("a", "127.0.0.1", "/")]);
}
