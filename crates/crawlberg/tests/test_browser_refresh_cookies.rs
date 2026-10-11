//! Chromiumoxide and a meta refresh: a refresh with no delay is a redirect that keeps the
//! cookies of the source page, and a refresh with a delay leaves the source page as the result.

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

/// Scrape `/` of a site whose `/` carries the meta refresh `refresh` to `/target` and sets a
/// cookie for `/target`. `None` when no usable Chrome exists on this host.
async fn scrape_refresh(test_name: &str, refresh: &str) -> Option<(crawlberg::ScrapeResult, MockServer)> {
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(
                    format!("<html><head>{refresh}</head><body>source</body></html>"),
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
    match scrape(&engine, &format!("{}/", site.uri())).await {
        Ok(page) => Some((page, site)),
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
            None
        }
        Err(error) => panic!("{test_name}: scrape must succeed: {error:?}"),
    }
}

async fn requested_paths(site: &MockServer) -> Vec<String> {
    site.received_requests()
        .await
        .expect("request recording is on")
        .iter()
        .map(|request| request.url.path().to_owned())
        .collect()
}

/// A refresh that Chrome does not act on and the crawl follows: the redirect chain fetches its
/// target in a newly-created page, and the target gets the cookie that the source page set.
///
/// ~keep The `<noscript>` keeps Chrome off the refresh, as Chrome runs scripts. The crawl reads
/// ~keep the page with scripting off and follows it. A delay cannot do this: the crawl does not
/// ~keep follow a delayed refresh (#605).
#[tokio::test]
async fn chromiumoxide_keeps_source_page_cookies_across_a_meta_refresh() {
    let test_name = "chromiumoxide_keeps_source_page_cookies_across_a_meta_refresh";
    let Some((page, site)) = scrape_refresh(
        test_name,
        r#"<noscript><meta http-equiv="refresh" content="0; url=/target"></noscript>"#,
    )
    .await
    else {
        return;
    };
    let paths = requested_paths(&site).await;
    let requests = site.received_requests().await.expect("request recording is on");
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
        "the refresh must land on its target; body was {:?}, paths={paths:?}",
        page.html
    );
    assert!(
        page.html.contains("landed"),
        "the target answers only a request with the cookie of the source page; body={:?}, paths={paths:?}",
        page.html
    );
}

/// A refresh with a delay leaves the page that carries it as the result, as a reader sees that
/// page for the delay. Its target is a link of the page and is not requested (#605).
#[tokio::test]
async fn chromiumoxide_keeps_the_source_page_of_a_delayed_meta_refresh() {
    let test_name = "chromiumoxide_keeps_the_source_page_of_a_delayed_meta_refresh";
    let Some((page, site)) =
        scrape_refresh(test_name, r#"<meta http-equiv="refresh" content="60; url=/target">"#).await
    else {
        return;
    };
    let paths = requested_paths(&site).await;
    assert_eq!(
        page.final_url,
        format!("{}/", site.uri()),
        "the page with the delayed refresh is the result; body was {:?}",
        page.html
    );
    assert!(page.html.contains("source"), "body={:?}", page.html);
    assert!(
        !paths.iter().any(|path| path == "/target"),
        "the target of a delayed refresh is not requested: {paths:?}"
    );
    assert_eq!(
        page.links.iter().map(|link| link.url.as_str()).collect::<Vec<_>>(),
        [format!("{}/target", site.uri())],
        "the target of the refresh is a link of the page"
    );
}
