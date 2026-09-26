//! `max_redirects` bounds the redirects Chrome follows in browser mode the same way it bounds
//! the HTTP mode's own chain: the same redirect count, the same stopping response, and no
//! request past the limit.
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

fn config(mode: BrowserMode, max_redirects: usize) -> CrawlConfig {
    CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            mode,
            timeout: Duration::from_secs(20),
            ..BrowserConfig::default()
        },
        max_depth: Some(0),
        max_redirects,
        respect_robots_txt: false,
        ..CrawlConfig::builder().allow_private_networks(true).build()
    }
}

async fn mount_html(mock: &MockServer, route: &str, body: &str) {
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(ResponseTemplate::new(200).set_body_raw(format!("<html><body>{body}</body></html>"), "text/html"))
        .mount(mock)
        .await;
}

/// `/` answers 301 to `/r1`, `/r1` to `/r2`, and so on for `hops` redirects; the last
/// hop lands on a 200 page.
async fn redirect_chain(hops: usize) -> MockServer {
    let mock = MockServer::start().await;
    for hop in 0..hops {
        let from = if hop == 0 { "/".to_owned() } else { format!("/r{hop}") };
        Mock::given(method("GET"))
            .and(path(from))
            .respond_with(ResponseTemplate::new(301).append_header("location", format!("/r{}", hop + 1)))
            .mount(&mock)
            .await;
    }
    mount_html(&mock, &format!("/r{hops}"), r#"<p id="landed">landed</p>"#).await;
    mock
}

/// Crawl `seed`, or `None` when no usable Chrome exists on this host.
async fn crawl_with(test_name: &str, config: CrawlConfig, seed: &str) -> Option<CrawlResult> {
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

/// What a crawl reports about its seed's redirect chain.
fn chain_outcome(result: &CrawlResult, base: &str) -> (usize, String, u16) {
    let page = result.pages.first().expect("the seed page must be reported");
    (
        result.redirect_count,
        result.final_url.trim_start_matches(base).to_owned(),
        page.status_code,
    )
}

#[tokio::test]
async fn browser_mode_stops_a_chain_longer_than_max_redirects_where_http_mode_does() {
    let test_name = "browser_mode_stops_a_chain_longer_than_max_redirects_where_http_mode_does";
    let http_site = redirect_chain(5).await;
    let http = crawl_with(
        test_name,
        config(BrowserMode::Never, 2),
        &format!("{}/", http_site.uri()),
    )
    .await
    .expect("HTTP mode needs no Chrome");
    let http_outcome = chain_outcome(&http, &http_site.uri());
    assert_eq!(
        http_outcome,
        (2, "/r2".to_owned(), 301),
        "HTTP mode stops on the 3xx at the limit"
    );

    let browser_site = redirect_chain(5).await;
    let Some(browser) = crawl_with(
        test_name,
        config(BrowserMode::Always, 2),
        &format!("{}/", browser_site.uri()),
    )
    .await
    else {
        return;
    };
    assert_eq!(
        chain_outcome(&browser, &browser_site.uri()),
        http_outcome,
        "browser mode must report the chain the way HTTP mode does"
    );

    let requested = requested_paths(&browser_site).await;
    assert!(
        !requested.iter().any(|p| ["/r3", "/r4", "/r5"].contains(&p.as_str())),
        "Chrome must not request past the limit, requested: {requested:?}"
    );
}

#[tokio::test]
async fn browser_mode_follows_a_chain_of_exactly_max_redirects() {
    let site = redirect_chain(2).await;
    let Some(result) = crawl_with(
        "browser_mode_follows_a_chain_of_exactly_max_redirects",
        config(BrowserMode::Always, 2),
        &format!("{}/", site.uri()),
    )
    .await
    else {
        return;
    };

    assert_eq!(chain_outcome(&result, &site.uri()), (2, "/r2".to_owned(), 200));
    let page = result.pages.first().expect("the seed page must be reported");
    assert!(
        page.html.contains("landed"),
        "the landing page must be rendered: {}",
        page.html
    );
}

/// A navigation the page's own script starts after load is not an HTTP redirect: it is not
/// counted against `max_redirects`, and the crawl reports the page it landed on.
#[tokio::test]
async fn a_javascript_navigation_after_load_is_not_counted_as_a_redirect() {
    let site = MockServer::start().await;
    mount_html(
        &site,
        "/",
        "<p>start</p><script>addEventListener('load', () => location.replace('/after'))</script>",
    )
    .await;
    mount_html(&site, "/after", r#"<p id="after">after</p>"#).await;

    let Some(result) = crawl_with(
        "a_javascript_navigation_after_load_is_not_counted_as_a_redirect",
        config(BrowserMode::Always, 0),
        &format!("{}/", site.uri()),
    )
    .await
    else {
        return;
    };

    assert_eq!(chain_outcome(&result, &site.uri()), (0, "/after".to_owned(), 200));
    let page = result.pages.first().expect("the seed page must be reported");
    assert!(
        page.html.contains("id=\"after\""),
        "the page must be the one the script navigated to: {}",
        page.html
    );
}

/// The redirects of a navigation the page's script starts belong to that navigation, not to
/// the seed's chain, so they do not count against the seed's `max_redirects`.
#[tokio::test]
async fn a_redirect_after_a_script_navigation_is_not_counted() {
    let site = MockServer::start().await;
    mount_html(
        &site,
        "/",
        "<p>start</p><script>addEventListener('load', () => location.replace('/go'))</script>",
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/go"))
        .respond_with(ResponseTemplate::new(302).append_header("location", "/after"))
        .mount(&site)
        .await;
    mount_html(&site, "/after", r#"<p id="after">after</p>"#).await;

    let Some(result) = crawl_with(
        "a_redirect_after_a_script_navigation_is_not_counted",
        config(BrowserMode::Always, 0),
        &format!("{}/", site.uri()),
    )
    .await
    else {
        return;
    };

    assert_eq!(chain_outcome(&result, &site.uri()), (0, "/after".to_owned(), 200));
    let page = result.pages.first().expect("the seed page must be reported");
    assert!(
        page.html.contains("id=\"after\""),
        "the page must be the one the script's navigation landed on: {}",
        page.html
    );
}

/// `/` redirects twice to `/m`, whose meta refresh (too slow for Chrome to act on before the
/// page is read) points at `/n`, which starts a second chain of two redirects.
async fn chain_with_a_meta_refresh() -> MockServer {
    let mock = MockServer::start().await;
    for (from, to) in [("/", "/r1"), ("/r1", "/m"), ("/n", "/n1"), ("/n1", "/n2")] {
        Mock::given(method("GET"))
            .and(path(from))
            .respond_with(ResponseTemplate::new(301).append_header("location", to))
            .mount(&mock)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/m"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"<html><head><meta http-equiv="refresh" content="30; url=/n"></head><body>m</body></html>"#,
            "text/html",
        ))
        .mount(&mock)
        .await;
    mount_html(&mock, "/n2", "<p>n2</p>").await;
    mock
}

/// A browser fetch later in a chain may follow only the redirects the chain has left.
#[tokio::test]
async fn a_later_browser_hop_gets_only_the_redirects_the_chain_has_left() {
    let test_name = "a_later_browser_hop_gets_only_the_redirects_the_chain_has_left";
    let http_site = chain_with_a_meta_refresh().await;
    let http = crawl_with(
        test_name,
        config(BrowserMode::Never, 3),
        &format!("{}/", http_site.uri()),
    )
    .await
    .expect("HTTP mode needs no Chrome");
    let http_outcome = chain_outcome(&http, &http_site.uri());
    assert_eq!(
        http_outcome,
        (3, "/n".to_owned(), 301),
        "HTTP mode stops on the 3xx at the limit"
    );

    let browser_site = chain_with_a_meta_refresh().await;
    let Some(browser) = crawl_with(
        test_name,
        config(BrowserMode::Always, 3),
        &format!("{}/", browser_site.uri()),
    )
    .await
    else {
        return;
    };
    assert_eq!(chain_outcome(&browser, &browser_site.uri()), http_outcome);
    let requested = requested_paths(&browser_site).await;
    assert!(
        !requested.iter().any(|p| p == "/n1"),
        "Chrome must not follow the second chain past the limit, requested: {requested:?}"
    );
}

/// `scrape()` reaches Chrome through the redirect chain, or directly when a screenshot is
/// requested; both stop at the limit.
#[tokio::test]
async fn scrape_stops_at_max_redirects_with_and_without_a_screenshot() {
    for capture_screenshot in [false, true] {
        let site = redirect_chain(5).await;
        let config = CrawlConfig {
            capture_screenshot,
            ..config(BrowserMode::Always, 2)
        };
        let engine = create_engine(Some(config)).expect("engine must build");
        let result = match scrape(&engine, &format!("{}/", site.uri())).await {
            Ok(result) => result,
            Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
                announce_chrome_skip("scrape_stops_at_max_redirects_with_and_without_a_screenshot", &message);
                return;
            }
            Err(error) => panic!("scrape must succeed: {error:?}"),
        };
        assert_eq!(
            (result.status_code, result.final_url.trim_start_matches(&site.uri())),
            (301, "/r2"),
            "capture_screenshot={capture_screenshot}: the scrape must stop on the 3xx at the limit"
        );
        let requested = requested_paths(&site).await;
        assert!(
            !requested.iter().any(|p| p == "/r3"),
            "capture_screenshot={capture_screenshot}: Chrome must not request past the limit, requested: {requested:?}"
        );
    }
}

/// Only the page's own navigation is limited: a redirect inside an iframe does not count.
#[tokio::test]
async fn a_redirect_inside_an_iframe_does_not_count() {
    let site = MockServer::start().await;
    mount_html(&site, "/", r#"<p id="top">top</p><iframe src="/f"></iframe>"#).await;
    Mock::given(method("GET"))
        .and(path("/f"))
        .respond_with(ResponseTemplate::new(301).append_header("location", "/f1"))
        .mount(&site)
        .await;
    mount_html(&site, "/f1", "<p>frame</p>").await;

    let Some(result) = crawl_with(
        "a_redirect_inside_an_iframe_does_not_count",
        config(BrowserMode::Always, 0),
        &format!("{}/", site.uri()),
    )
    .await
    else {
        return;
    };

    assert_eq!(chain_outcome(&result, &site.uri()), (0, "/".to_owned(), 200));
}
