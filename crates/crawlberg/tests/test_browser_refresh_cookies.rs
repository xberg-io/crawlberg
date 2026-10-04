//! Chromiumoxide refresh hops preserve the browser's cookie jar.

#![cfg(feature = "browser")]

use std::time::Duration;

use crawlberg::{BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig, CrawlError, create_engine, scrape};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::{announce_chrome_skip, is_missing_chrome_message};

fn config() -> CrawlConfig {
    CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            mode: BrowserMode::Always,
            timeout: Duration::from_secs(20),
            ..BrowserConfig::default()
        },
        max_redirects: 1,
        respect_robots_txt: false,
        ..CrawlConfig::builder().allow_private_networks(true).build()
    }
}

/// A delayed refresh is followed by the redirect chain after Chrome returns the source page,
/// so its target is fetched in a newly-created page. ~keep
#[tokio::test]
async fn chromiumoxide_keeps_source_page_cookies_across_a_delayed_meta_refresh() {
    let test_name = "chromiumoxide_keeps_source_page_cookies_across_a_delayed_meta_refresh";
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(
                    r#"<html><head><meta http-equiv="refresh" content="60; url=/target"></head><body>source</body></html>"#,
                    "text/html",
                )
                .append_header("set-cookie", "source=1; Path=/target; HttpOnly"),
        )
        .mount(&site)
        .await;
    Mock::given(method("GET"))
        .and(path("/target"))
        .and(header("cookie", "source=1"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<html><body>landed</body></html>", "text/html"))
        .mount(&site)
        .await;

    let engine = create_engine(Some(config())).expect("engine must build");
    let result = scrape(&engine, &format!("{}/", site.uri())).await;
    let page = match result {
        Ok(page) => page,
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
            return;
        }
        Err(error) => panic!("{test_name}: scrape must succeed: {error:?}"),
    };

    let requests = site.received_requests().await.expect("request recording is on");
    let paths: Vec<&str> = requests.iter().map(|request| request.url.path()).collect();
    let target = requests
        .iter()
        .find(|request| request.url.path() == "/target")
        .unwrap_or_else(|| {
            panic!(
                "the scrape must request the refresh target; final_url={}, body={:?}, paths={paths:?}",
                page.final_url, page.html
            )
        });
    let sent = target
        .headers
        .get("cookie")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    assert!(
        sent.split("; ").any(|cookie| cookie == "source=1"),
        "the refresh target must receive the source page's cookie; sent={sent:?}, final_url={}, body={:?}",
        page.final_url,
        page.html
    );
    assert_eq!(
        page.final_url,
        format!("{}/target", site.uri()),
        "the delayed refresh must land on its target; body was {:?}",
        page.html
    );
    assert!(
        page.html.contains("landed"),
        "the scrape must return the refresh target; final_url={}, body={:?}",
        page.final_url,
        page.html
    );
}
