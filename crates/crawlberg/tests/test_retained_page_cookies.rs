#![cfg(feature = "browser")]

use std::sync::Arc;
use std::time::Duration;

use crawlberg::{
    BrowserBackend, BrowserConfig, BrowserMode, BrowserPool, BrowserPoolConfig, BrowserSessionPool, CrawlConfig, crawl,
    create_engine,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn from_the_browser(request: &Request) -> bool {
    request.headers.contains_key("upgrade-insecure-requests")
}

async fn browser_only(site: &MockServer, at: &'static str, page: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(move |request: &Request| {
            if from_the_browser(request) {
                page.clone()
            } else {
                ResponseTemplate::new(403)
            }
        })
        .mount(site)
        .await;
}

async fn retained_cookie_case(last_path: &'static str, extra_cookie: &str, deletion: &str, expected: &str) {
    retained_named_cookie_case(last_path, "c=1; Path=/", extra_cookie, deletion, expected).await;
}

async fn retained_named_cookie_case(
    last_path: &'static str,
    first_cookie: &str,
    extra_cookie: &str,
    deletion: &str,
    expected: &str,
) {
    let site = MockServer::start().await;
    let first = r#"<html><head><meta http-equiv="refresh" content="60; url=/hop"></head><body>first</body></html>"#;
    browser_only(
        &site,
        "/first",
        ResponseTemplate::new(200)
            .set_body_raw(first, "text/html")
            .append_header("set-cookie", first_cookie)
            .append_header("set-cookie", "keep=1; Path=/")
            .append_header("set-cookie", extra_cookie),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/hop"))
        .respond_with(
            ResponseTemplate::new(302)
                .append_header("location", last_path)
                .append_header("set-cookie", deletion),
        )
        .mount(&site)
        .await;
    browser_only(
        &site,
        last_path,
        ResponseTemplate::new(200).set_body_raw("<html><body>last</body></html>", "text/html"),
    )
    .await;

    let pool = BrowserPool::new(BrowserPoolConfig::default());
    let mut config = CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            mode: BrowserMode::Auto,
            timeout: Duration::from_secs(20),
            ..BrowserConfig::default()
        },
        cookies_enabled: true,
        respect_robots_txt: false,
        stay_on_domain: false,
        max_depth: Some(3),
        ..CrawlConfig::builder().allow_private_networks(true).build()
    };
    config.browser.session_affinity = true;
    config.browser_pool = Some(Arc::clone(&pool));
    config.browser_session_pool = Some(Arc::new(BrowserSessionPool::new()));
    let engine = create_engine(Some(config)).expect("engine must build");

    crawl(&engine, &format!("{}/first", site.uri()))
        .await
        .expect("the crawl must run");

    let last: Vec<String> = site
        .received_requests()
        .await
        .expect("request recording is on")
        .iter()
        .filter(|request| request.url.path() == last_path && from_the_browser(request))
        .map(|request| {
            let cookie = request.headers.get("cookie").and_then(|value| value.to_str().ok());
            cookie.unwrap_or_default().to_owned()
        })
        .collect();
    pool.shutdown().await;
    assert_eq!(last, [expected], "the deleted cookie `c` must not come back");
}

#[tokio::test]
async fn a_cookie_deleted_over_http_does_not_come_back_from_the_kept_page() {
    retained_cookie_case("/last", "outside=1; Path=/private", "c=; Path=/; Max-Age=0", "keep=1").await;
}

#[tokio::test]
async fn deleting_one_path_keeps_the_same_cookie_name_on_another_path() {
    retained_cookie_case(
        "/private/last",
        "c=private; Path=/private",
        "c=; Path=/; Max-Age=0",
        "c=private; keep=1",
    )
    .await;
}

#[tokio::test]
async fn an_expires_deletion_reaches_the_retained_context() {
    retained_cookie_case(
        "/last",
        "outside=1; Path=/private",
        "c=; Path=/; Expires=Thu, 01 Jan 1970 00:00:00 GMT",
        "keep=1",
    )
    .await;
}

#[tokio::test]
async fn a_partitioned_http_change_keeps_the_unpartitioned_retained_cookie() {
    retained_cookie_case(
        "/last",
        "outside=1; Path=/private",
        "c=; Path=/; Secure; Partitioned; Max-Age=0",
        "c=1; keep=1",
    )
    .await;
}

#[tokio::test]
async fn a_malformed_prefixed_http_deletion_keeps_the_retained_cookie() {
    retained_named_cookie_case(
        "/last",
        "__Host-c=1; Path=/; Secure",
        "outside=1; Path=/private",
        "__Host-c=; Path=/; Max-Age=0",
        "__Host-c=1; keep=1",
    )
    .await;
}
