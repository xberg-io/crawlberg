//! Browser mode with the default wait: a page that draws shortly after its last response is
//! read after the draw (crawlberg#769), and a page that hits `browser.timeout` while its text is
//! already rendered returns that text, marked partial, in place of a `browser_timeout` error
//! (crawlberg#779).
//!
//! Requires a real Chrome binary; skipped (not failed) when Chrome is unavailable.

#![cfg(feature = "browser")]

use std::time::{Duration, Instant};

use crawlberg::{BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig, CrawlError, crawl, create_engine, scrape};
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

/// An article whose only image answers after `image_delay`.
async fn article_with_slow_image(image_delay: Duration) -> MockServer {
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "<html><body><h1>Article</h1><p>The article text is on the page.</p><img src=\"/slow.gif\"></body></html>",
            "text/html",
        ))
        .mount(&site)
        .await;
    Mock::given(method("GET"))
        .and(path("/slow.gif"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(b"GIF89a".to_vec(), "image/gif")
                .set_delay(image_delay),
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
async fn configured_quiet_window_captures_text_drawn_600ms_after_the_last_response() {
    let test_name = "configured_quiet_window_captures_text_drawn_600ms_after_the_last_response";
    let site = spa(600).await;
    let mut options = config(Duration::from_secs(20));
    options.browser.dom_quiet_window = Duration::from_secs(1);
    let Some((result, _)) = scrape_default(test_name, &site, options).await else {
        return;
    };
    let page = result.unwrap_or_else(|error| panic!("{test_name}: {error:?}"));
    assert!(page.html.contains(NEEDLE), "{test_name}: {}", page.html);
}

/// A page that changes its DOM every 100 ms, forever.
async fn ticker() -> MockServer {
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "<html><body><p>ticker</p><div id=\"t\"></div><script>\
             setInterval(() => { document.getElementById('t').textContent = Date.now(); }, 100);</script></body></html>",
            "text/html",
        ))
        .mount(&site)
        .await;
    site
}

/// A quiet page returns on the quiet window and a page that never settles on the cap, which is
/// longer and far below `browser.timeout`. Compared with each other so Chrome's launch time, which
/// both include, cancels out.
#[tokio::test]
async fn the_wait_ends_at_the_quiet_window_on_a_static_page_and_at_the_cap_on_a_ticker() {
    let test_name = "the_wait_ends_at_the_quiet_window_on_a_static_page_and_at_the_cap_on_a_ticker";
    let timeout = Duration::from_secs(30);
    let quiet_site = spa(0).await;
    let Some((quiet, quiet_elapsed)) = scrape_default(test_name, &quiet_site, config(timeout)).await else {
        return;
    };
    let quiet = quiet.unwrap_or_else(|error| panic!("{test_name}: {error:?}"));
    assert!(quiet.html.contains(NEEDLE), "{test_name}");
    let busy_site = ticker().await;
    let Some((busy, busy_elapsed)) = scrape_default(test_name, &busy_site, config(timeout)).await else {
        return;
    };
    let busy = busy.unwrap_or_else(|error| panic!("{test_name}: {error:?}"));
    assert!(
        busy.markdown.is_some_and(|markdown| markdown.warnings.is_empty()),
        "{test_name}: not partial"
    );
    assert!(
        busy_elapsed > quiet_elapsed + Duration::from_millis(500) && busy_elapsed >= Duration::from_secs(4),
        "{test_name}: the ticker ({busy_elapsed:?}) must wait for the cap, longer than the static page ({quiet_elapsed:?})"
    );
    assert!(
        busy_elapsed < Duration::from_secs(20),
        "{test_name}: the cap, not browser.timeout, ends the wait: {busy_elapsed:?}"
    );
}

#[tokio::test]
async fn a_renderer_stall_is_not_counted_as_dom_quiet_on_a_ticker() {
    let test_name = "a_renderer_stall_is_not_counted_as_dom_quiet_on_a_ticker";
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"<html><body><p>ticker</p><div id="t"></div><script>
            const NativeObserver = MutationObserver;
            let stalled = false;
            window.MutationObserver = class extends NativeObserver {
              constructor(callback) {
                super((records, observer) => {
                  callback(records, observer);
                  if (!stalled) {
                    stalled = true;
                    setTimeout(() => {
                      const until = performance.now() + 250;
                      while (performance.now() < until) {}
                    }, 10);
                  }
                });
              }
            };
            setInterval(() => document.getElementById('t').textContent = Date.now(), 100);
            </script></body></html>"#,
            "text/html",
        ))
        .mount(&site)
        .await;
    let Some((result, elapsed)) = scrape_default(test_name, &site, config(Duration::from_secs(30))).await else {
        return;
    };
    result.unwrap_or_else(|error| panic!("{test_name}: {error:?}"));
    assert!(
        elapsed >= Duration::from_secs(4),
        "a busy renderer is not quiet: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(20),
        "the existing cap must still end the wait: {elapsed:?}"
    );
}

/// The settle wait must not turn a page that finished loading into a timeout: with 5 s of budget
/// and 4.3 s spent loading, less than the 1 s quiet window is left, and the wait gets only that.
#[tokio::test]
async fn a_page_that_loads_late_in_its_timeout_is_not_partial() {
    let test_name = "a_page_that_loads_late_in_its_timeout_is_not_partial";
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw("<html><body><p>loaded in time</p></body></html>", "text/html")
                .set_delay(Duration::from_millis(4300)),
        )
        .mount(&site)
        .await;
    let mut options = config(Duration::from_secs(5));
    options.browser.dom_quiet_window = Duration::from_secs(1);
    let Some((result, _)) = scrape_default(test_name, &site, options).await else {
        return;
    };
    let page = result.unwrap_or_else(|error| panic!("{test_name}: {error:?}"));
    assert!(page.html.contains("loaded in time"), "{test_name}");
    let markdown = page.markdown.expect("markdown");
    assert!(markdown.warnings.is_empty(), "{test_name}: {:?}", markdown.warnings);
}

#[tokio::test]
async fn a_timeout_with_rendered_text_returns_the_text_marked_partial() {
    let test_name = "a_timeout_with_rendered_text_returns_the_text_marked_partial";
    let site = article_with_slow_image(Duration::from_secs(30)).await;
    let Some((result, elapsed)) = scrape_default(test_name, &site, config(Duration::from_secs(3))).await else {
        return;
    };
    let page = result.unwrap_or_else(|error| panic!("{test_name}: a rendered page is not an error: {error:?}"));
    assert!(
        page.html.contains("The article text is on the page."),
        "{test_name}: {}",
        page.html
    );
    let markdown = page.markdown.expect("markdown");
    assert!(
        markdown.content.contains("The article text is on the page."),
        "{test_name}"
    );
    assert!(
        markdown.warnings.iter().any(|warning| warning.contains("partial")),
        "{test_name}: warnings must say the page is partial: {:?}",
        markdown.warnings
    );
    assert!(elapsed < Duration::from_secs(12), "{test_name}: took {elapsed:?}");
}

#[tokio::test]
async fn a_crawl_timeout_with_rendered_text_keeps_the_page_marked_partial() {
    let test_name = "a_crawl_timeout_with_rendered_text_keeps_the_page_marked_partial";
    let site = article_with_slow_image(Duration::from_secs(30)).await;
    let engine = create_engine(Some(config(Duration::from_secs(3)))).expect("engine must build");
    let result = match crawl(&engine, &format!("{}/", site.uri())).await {
        Ok(result) => result,
        Err(error) if chrome_missing(test_name, &error) => return,
        Err(error) => panic!("{test_name}: {error:?}"),
    };
    assert_eq!(result.error, None, "{test_name}");
    let page = result.pages.first().unwrap_or_else(|| panic!("{test_name}: no page"));
    let markdown = page.markdown.as_ref().expect("markdown");
    assert!(
        markdown.content.contains("The article text is on the page."),
        "{test_name}"
    );
    assert!(
        markdown.warnings.iter().any(|warning| warning.contains("partial")),
        "{test_name}: {:?}",
        markdown.warnings
    );
}

#[tokio::test]
async fn a_timeout_with_nothing_rendered_is_still_a_timeout_error() {
    let test_name = "a_timeout_with_nothing_rendered_is_still_a_timeout_error";
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw("<html><body><p>late</p></body></html>", "text/html")
                .set_delay(Duration::from_secs(30)),
        )
        .mount(&site)
        .await;
    let Some((result, _)) = scrape_default(test_name, &site, config(Duration::from_secs(2))).await else {
        return;
    };
    assert!(
        matches!(result, Err(CrawlError::BrowserTimeout { .. })),
        "{test_name}: {result:?}"
    );
}

#[test]
fn quiet_window_defaults_to_150ms_and_accepts_milliseconds_in_json() {
    let default = serde_json::to_value(BrowserConfig::default()).expect("serialize config");
    assert_eq!(default["dom_quiet_window"], serde_json::json!(150));
    let config: BrowserConfig =
        serde_json::from_value(serde_json::json!({"dom_quiet_window": 725})).expect("deserialize config");
    let serialized = serde_json::to_value(config).expect("serialize configured window");
    assert_eq!(serialized["dom_quiet_window"], serde_json::json!(725));
}

#[tokio::test]
async fn each_dom_mutation_restarts_the_default_quiet_window() {
    let test_name = "each_dom_mutation_restarts_the_default_quiet_window";
    let site = MockServer::start().await;
    let html = format!(
        "<html><body><h1>Loading</h1><p id=progress></p><script>\
         let ticks = 0; const timer = setInterval(() => {{\
         const progress = document.getElementById('progress'); ticks++;\
         progress.textContent = ticks < 4 ? String(ticks) : '{NEEDLE}';\
         if (ticks === 4) clearInterval(timer); }}, 75);</script></body></html>"
    );
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(html, "text/html"))
        .mount(&site)
        .await;
    let Some((result, _)) = scrape_default(test_name, &site, config(Duration::from_secs(20))).await else {
        return;
    };
    let page = result.unwrap_or_else(|error| panic!("{test_name}: {error:?}"));
    assert!(page.html.contains(NEEDLE), "{test_name}: {}", page.html);
}
