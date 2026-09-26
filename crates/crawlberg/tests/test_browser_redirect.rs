//! A page reached through a redirect in browser mode resolves its links against the URL
//! Chrome landed on, and that URL passes the same crawl policy a followed 3xx target does.
//!
//! Requires a real Chrome binary (chromiumoxide auto-detects it) and is gated behind the
//! `browser` feature; skipped (not failed) when Chrome is unavailable, matching the other
//! browser tests.

#![cfg(feature = "browser")]

use std::time::Duration;

use crawlberg::{
    BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig, CrawlError, CrawlResult, crawl, create_engine, scrape,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::{announce_chrome_skip, is_missing_chrome_message};

fn browser_config(exclude_paths: Vec<String>) -> CrawlConfig {
    CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            mode: BrowserMode::Always,
            timeout: Duration::from_secs(20),
            ..BrowserConfig::default()
        },
        max_depth: Some(2),
        exclude_paths,
        ..CrawlConfig::builder().allow_private_networks(true).build()
    }
}

async fn mount_html(mock: &MockServer, route: &str, body: &str) {
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(
            // ~keep `set_body_raw`, not `set_body_string` plus a header: the latter also sends
            // ~keep `text/plain`, and Chrome renders that as text with no links in it.
            ResponseTemplate::new(200).set_body_raw(format!("<html><body>{body}</body></html>"), "text/html"),
        )
        .mount(mock)
        .await;
}

/// `/` links to `/go`, which answers 302 to `landing`; the landing page links `next.html`.
async fn redirecting_site(landing: &str) -> MockServer {
    let mock = MockServer::start().await;
    mount_html(&mock, "/", r#"<a href="/go">go</a>"#).await;
    Mock::given(method("GET"))
        .and(path("/go"))
        .respond_with(ResponseTemplate::new(302).append_header("location", landing))
        .mount(&mock)
        .await;
    mount_html(&mock, landing, r#"<p>landed</p><a href="next.html">next</a>"#).await;
    let next = format!(
        "{}next.html",
        &landing[..=landing.rfind('/').expect("landing is a path")]
    );
    mount_html(&mock, &next, "<p>next page</p>").await;
    mock
}

/// Crawl `seed`, or `None` when no usable Chrome exists on this host.
async fn crawl_in_browser(test_name: &str, config: CrawlConfig, seed: &str) -> Option<CrawlResult> {
    let engine = create_engine(Some(config)).expect("engine must build");
    match crawl(&engine, seed).await {
        Ok(result) => Some(result),
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
            None
        }
        Err(error) => panic!("crawl must succeed: {error:?}"),
    }
}

async fn requested_paths(mock: &MockServer) -> Vec<String> {
    mock.received_requests()
        .await
        .expect("request recording is on")
        .iter()
        .map(|request| request.url.path().to_owned())
        .collect()
}

#[tokio::test]
async fn a_redirected_page_resolves_its_links_against_the_landed_url() {
    let mock = redirecting_site("/page/index.html").await;
    let Some(result) = crawl_in_browser(
        "a_redirected_page_resolves_its_links_against_the_landed_url",
        browser_config(Vec::new()),
        &format!("{}/", mock.uri()),
    )
    .await
    else {
        return;
    };

    let landed = result
        .pages
        .iter()
        .find(|page| page.url.ends_with("/go"))
        .unwrap_or_else(|| {
            let reported: Vec<(&str, &str)> = result
                .pages
                .iter()
                .map(|p| (p.url.as_str(), p.final_url.as_str()))
                .collect();
            panic!("the /go page must be reported, got {reported:?}")
        });
    assert_eq!(
        landed.final_url,
        format!("{}/page/index.html", mock.uri()),
        "the page must report the URL Chrome landed on"
    );
    assert_eq!(landed.redirect_count, 1, "the landed URL is one redirect away");

    let requested = requested_paths(&mock).await;
    assert!(
        requested.iter().any(|p| p == "/page/next.html"),
        "the relative link must resolve against the landed URL, requested: {requested:?}"
    );
    assert!(
        !requested.iter().any(|p| p == "/next.html"),
        "the relative link must not resolve against the requested URL, requested: {requested:?}"
    );
}

#[tokio::test]
async fn a_landed_url_the_crawl_excludes_is_not_used() {
    let mock = redirecting_site("/private/index.html").await;
    let Some(result) = crawl_in_browser(
        "a_landed_url_the_crawl_excludes_is_not_used",
        browser_config(vec!["^/private/".to_owned()]),
        &format!("{}/", mock.uri()),
    )
    .await
    else {
        return;
    };

    let reported: Vec<&str> = result.pages.iter().map(|page| page.url.as_str()).collect();
    assert!(
        !result.pages.iter().any(|page| page.url.ends_with("/go")),
        "a page that landed on an excluded path must not be reported, got {reported:?}"
    );
    let requested = requested_paths(&mock).await;
    assert!(
        !requested.iter().any(|p| p.starts_with("/private/next")),
        "an excluded landing page's links must not be followed, requested: {requested:?}"
    );
}

/// robots.txt disallows `/private/`, and `/go` redirects there. The landing page links an
/// allowed page, so only a crawl that dropped the landing leaves that page unfetched.
#[tokio::test]
async fn a_landed_url_robots_txt_disallows_is_not_used() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("User-agent: *\nDisallow: /private/\n", "text/plain"))
        .mount(&mock)
        .await;
    mount_html(&mock, "/", r#"<a href="/go">go</a>"#).await;
    Mock::given(method("GET"))
        .and(path("/go"))
        .respond_with(ResponseTemplate::new(302).append_header("location", "/private/index.html"))
        .mount(&mock)
        .await;
    mount_html(
        &mock,
        "/private/index.html",
        r#"<p>landed</p><a href="/public/after.html">after</a>"#,
    )
    .await;
    mount_html(&mock, "/public/after.html", "<p>after</p>").await;

    let config = CrawlConfig {
        respect_robots_txt: true,
        ..browser_config(Vec::new())
    };
    let Some(result) = crawl_in_browser(
        "a_landed_url_robots_txt_disallows_is_not_used",
        config,
        &format!("{}/", mock.uri()),
    )
    .await
    else {
        return;
    };

    let reported: Vec<&str> = result.pages.iter().map(|page| page.url.as_str()).collect();
    assert!(
        !result.pages.iter().any(|page| page.url.ends_with("/go")),
        "a page that landed on a robots-disallowed path must not be reported, got {reported:?}"
    );
    let requested = requested_paths(&mock).await;
    assert!(
        !requested.iter().any(|p| p == "/public/after.html"),
        "a disallowed landing page's links must not be followed, requested: {requested:?}"
    );
}

/// `scrape()` takes a separate chromiumoxide path when a screenshot is requested, so both
/// paths are checked for the landed URL.
#[tokio::test]
async fn scrape_reports_the_landed_url_with_and_without_a_screenshot() {
    let mock = redirecting_site("/page/index.html").await;
    for capture_screenshot in [false, true] {
        let config = CrawlConfig {
            capture_screenshot,
            ..browser_config(Vec::new())
        };
        let engine = create_engine(Some(config)).expect("engine must build");
        let result = match scrape(&engine, &format!("{}/go", mock.uri())).await {
            Ok(result) => result,
            Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
                announce_chrome_skip("scrape_reports_the_landed_url_with_and_without_a_screenshot", &message);
                return;
            }
            Err(error) => panic!("scrape must succeed: {error:?}"),
        };
        assert_eq!(
            result.final_url,
            format!("{}/page/index.html", mock.uri()),
            "capture_screenshot={capture_screenshot}: the result must report the landed URL"
        );
        assert!(
            result.links.iter().any(|link| link.url.ends_with("/page/next.html")),
            "capture_screenshot={capture_screenshot}: links must resolve against the landed URL, got {:?}",
            result.links
        );
    }
}
