//! Browser mode with the default wait: a page that draws shortly after its last response is
//! read after the draw (crawlberg#769).
//!
//! Requires a real Chrome binary; skipped (not failed) when Chrome is unavailable.

#![cfg(feature = "browser")]

use std::time::{Duration, Instant};

use crawlberg::{BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig, CrawlError, create_engine, scrape};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::{announce_chrome_skip, is_missing_chrome_message};

const NEEDLE: &str = "This package has no requirements.";

fn config(timeout: Duration) -> CrawlConfig {
    CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            mode: BrowserMode::Always,
            timeout,
            ..BrowserConfig::default()
        },
        respect_robots_txt: false,
        max_depth: Some(0),
        max_pages: Some(1),
        ..CrawlConfig::builder().allow_private_networks(true).build()
    }
}

/// A page that fetches `/api` and draws `NEEDLE` `draw_ms` after the response.
async fn spa(draw_ms: u64) -> MockServer {
    let site = MockServer::start().await;
    let html = format!(
        "<!doctype html><html><body><h1>Package</h1><div id=\"req\"></div><script>\
         fetch('/api').then(r => r.json()).then(d => setTimeout(() => {{ \
         document.getElementById('req').textContent = d.text; }}, {draw_ms}));</script></body></html>"
    );
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(html, "text/html"))
        .mount(&site)
        .await;
    Mock::given(method("GET"))
        .and(path("/api"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(format!("{{\"text\": \"{NEEDLE}\"}}"), "application/json"),
        )
        .mount(&site)
        .await;
    site
}

fn chrome_missing(test_name: &str, error: &CrawlError) -> bool {
    let message = error.to_string();
    if is_missing_chrome_message(&message) {
        announce_chrome_skip(test_name, &message);
        return true;
    }
    false
}

async fn scrape_default(
    test_name: &str,
    site: &MockServer,
    config: CrawlConfig,
) -> Option<(Result<crawlberg::ScrapeResult, CrawlError>, Duration)> {
    let engine = create_engine(Some(config)).expect("engine must build");
    let started = Instant::now();
    let result = scrape(&engine, &format!("{}/", site.uri())).await;
    let elapsed = started.elapsed();
    if let Err(error) = &result
        && chrome_missing(test_name, error)
    {
        return None;
    }
    Some((result, elapsed))
}

#[tokio::test]
async fn default_wait_captures_text_drawn_600ms_after_the_last_response() {
    let test_name = "default_wait_captures_text_drawn_600ms_after_the_last_response";
    let site = spa(600).await;
    let Some((result, _)) = scrape_default(test_name, &site, config(Duration::from_secs(20))).await else {
        return;
    };
    let page = result.unwrap_or_else(|error| panic!("{test_name}: {error:?}"));
    assert!(page.html.contains(NEEDLE), "{test_name}: {}", page.html);
}

#[tokio::test]
async fn default_wait_captures_text_drawn_300ms_after_the_last_response() {
    let test_name = "default_wait_captures_text_drawn_300ms_after_the_last_response";
    let site = spa(300).await;
    let Some((result, _)) = scrape_default(test_name, &site, config(Duration::from_secs(20))).await else {
        return;
    };
    let page = result.unwrap_or_else(|error| panic!("{test_name}: {error:?}"));
    assert!(page.html.contains(NEEDLE), "{test_name}: {}", page.html);
}

#[tokio::test]
async fn default_wait_stays_short_on_a_static_page() {
    let test_name = "default_wait_stays_short_on_a_static_page";
    let site = spa(0).await;
    let Some((result, elapsed)) = scrape_default(test_name, &site, config(Duration::from_secs(20))).await else {
        return;
    };
    result.unwrap_or_else(|error| panic!("{test_name}: {error:?}"));
    assert!(elapsed < Duration::from_secs(10), "{test_name}: took {elapsed:?}");
}
