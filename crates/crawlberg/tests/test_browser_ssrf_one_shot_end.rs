//! One-shot browser scrapes send nothing to an address the SSRF policy refuses as they end, even
//! when Chrome is slow to close the page (xberg-io/crawlberg#468).
//!
//! A test binary of its own: it loads the host on purpose, and the other browser tests would
//! share that load if they ran beside it.
//!
//! The seed is served on `localhost`, which the policy allowlists by name. The denied target is
//! the literal address `127.0.0.1` on a second server, which `deny_private` refuses. Requires a
//! real Chrome binary; skipped (not failed) when Chrome is unavailable.

#![cfg(feature = "browser")]

use std::time::Duration;

use crawlberg::{
    BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig, CrawlError, HostMatcher, create_engine, scrape,
};
use wiremock::matchers::{any, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::{announce_chrome_skip, is_missing_chrome_message};

fn html(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(format!("<html><body>{body}</body></html>"), "text/html")
}

/// One-shot scrapes of a page that keeps sending, a dozen at a time and repeated, send nothing
/// to the denied address as they end.
///
/// ~keep A leak needs the browser to be slow to destroy the page as the fetch ends, which is what
/// ~keep load does, so the test makes its own: each round launches a dozen browsers at once, and
/// ~keep each page keeps its renderer busy between requests.
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_one_shot_scrapes_send_nothing_refused_as_they_end() {
    let test_name = "concurrent_one_shot_scrapes_send_nothing_refused_as_they_end";
    let denied = MockServer::start().await;
    Mock::given(any())
        .respond_with(html("denied-marker"))
        .mount(&denied)
        .await;
    let target = format!("http://127.0.0.1:{}/secret", denied.address().port());
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(html(&format!(
            "<p>start</p><script>setInterval(() => {{ const until = Date.now() + 5; while (Date.now() < until) {{}} for (let k = 0; k < 20; k++) fetch({target:?} + '?' + Math.random(), {{ mode: 'no-cors' }}).catch(() => {{}}); }}, 1);</script>"
        )))
        .mount(&site)
        .await;
    let seed = format!("http://localhost:{}/", site.address().port());
    let config = CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            mode: BrowserMode::Always,
            timeout: Duration::from_secs(20),
            ..BrowserConfig::default()
        },
        respect_robots_txt: false,
        ..CrawlConfig::builder()
            .ssrf_allowlist_host(HostMatcher::exact("localhost"))
            .build()
    };
    let engine = create_engine(Some(config)).expect("engine must build");
    let mut ended = 0;
    for _ in 0..4 {
        let results = futures::future::join_all((0..12).map(|_| scrape(&engine, &seed))).await;
        for result in results {
            match result {
                // ~keep A navigation that times out under this load while a request is refused ends
                // ~keep as the policy error; the fetch still ended and tore its browser down.
                Ok(_) | Err(CrawlError::SsrfPolicyViolation { .. }) => ended += 1,
                Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
                    announce_chrome_skip(test_name, &message);
                    return;
                }
                Err(error) => panic!("{test_name}: scrape must succeed: {error:?}"),
            }
        }
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let received = denied.received_requests().await.expect("request recording is on");
        assert!(
            received.is_empty(),
            "{test_name}: the denied address must receive no request, got {}",
            received.len()
        );
    }
    assert_eq!(ended, 48, "{test_name}: every scrape must end");
}
