//! A Chrome crawlberg launches with a throwaway profile for one `interact` session or one scrape
//! leaves no profile directory behind, even though it is killed rather than closed
//! (xberg-io/crawlberg#468).
//!
//! The profile directories are named after this process's id, so the test counts only its own.
//! A test binary of its own, so no other test's browsers share that id. Requires a real Chrome
//! binary; skipped (not failed) when Chrome is unavailable.

#![cfg(feature = "browser")]

use std::time::Duration;

use crawlberg::{
    BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig, CrawlError, HostMatcher, PageAction, create_engine,
    interact, scrape,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::{announce_chrome_skip, is_missing_chrome_message};

/// The profile directories of this process left in the temp directory.
fn own_profiles() -> Vec<String> {
    let id = std::process::id();
    let prefixes = [format!("crawlberg-interact-{id}-"), format!("crawlberg-browser-{id}-")];
    std::fs::read_dir(std::env::temp_dir())
        .expect("the temp directory must be readable")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| prefixes.iter().any(|prefix| name.starts_with(prefix)))
        .collect()
}

/// Sessions and scrapes, several at a time, of a page that keeps its renderer busy and writes to
/// storage and the cache until the end, leave no profile directory behind.
///
/// ~keep The kill ends only Chrome's main process; the renderers write into the profile until
/// ~keep they exit a moment later, so a removal that does not wait for them loses the race. The
/// ~keep scrape's teardown runs in the background after `scrape` returns, and under load its stop
/// ~keep first waits for the check to answer every request the page had paused, so the count is
/// ~keep polled for up to a minute.
/// ~keep A profile the removal missed stays for good, so the wait cannot hide one.
#[tokio::test(flavor = "multi_thread")]
async fn killed_browsers_leave_no_profile_behind() {
    let test_name = "killed_browsers_leave_no_profile_behind";
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "<html><body><p>start</p><script>setInterval(() => { const until = Date.now() + 5; \
             while (Date.now() < until) {} localStorage.setItem('k' + Math.random(), 'v'.repeat(1000)); \
             fetch('/asset?' + Math.random()).catch(() => {}); }, 1);</script></body></html>",
            "text/html",
        ))
        .mount(&site)
        .await;
    Mock::given(method("GET"))
        .and(path("/asset"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("cache-control", "max-age=3600")
                .set_body_raw("x".repeat(20_000), "text/plain"),
        )
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
    let actions = vec![PageAction::Wait {
        milliseconds: Some(300),
        selector: None,
    }];
    let mut ended = 0;
    let mut failed_launches = 0;
    let mut missing_chrome = None;
    for _ in 0..2 {
        let sessions = futures::future::join_all((0..6).map(|_| interact(&engine, &seed, actions.clone())));
        let scrapes = futures::future::join_all((0..6).map(|_| scrape(&engine, &seed)));
        let (sessions, scrapes) = tokio::join!(sessions, scrapes);
        let outcomes = sessions
            .into_iter()
            .map(|result| result.map(drop))
            .chain(scrapes.into_iter().map(|result| result.map(drop)));
        // ~keep A launch that times out under load has no browser to kill, and its profile is
        // ~keep removed by the launch path, not the kill, so it may be left and is allowed for. A
        // ~keep session that times out still ends and tears its browser down, so it counts.
        for outcome in outcomes {
            match outcome {
                Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
                    missing_chrome = Some(message);
                    failed_launches += 1;
                }
                _ => ended += 1,
            }
        }
    }
    if ended == 0 {
        announce_chrome_skip(test_name, &missing_chrome.unwrap_or_default());
        return;
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let mut left = own_profiles();
    while left.len() > failed_launches && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
        left = own_profiles();
    }
    assert!(
        left.len() <= failed_launches,
        "{test_name}: every profile of a killed browser must be removed, {} left after {ended} ended sessions \
         and scrapes and {failed_launches} failed launches: {left:?}",
        left.len()
    );
}
