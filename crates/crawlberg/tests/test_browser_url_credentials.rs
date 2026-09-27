//! The chromiumoxide backend sends a caller's URL credentials to the seed host only, and
//! refuses a URL with userinfo that a page asks the browser to load.
//!
//! Requires a real Chrome binary (chromiumoxide auto-detects it) and is gated behind the
//! `browser` feature; skipped (not failed) when Chrome is unavailable, matching the other
//! browser tests.

#![cfg(feature = "browser")]

use std::time::Duration;

use base64::Engine as _;
use crawlberg::{
    AuthConfig, BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig, CrawlError, ScrapeResult, create_engine,
    scrape,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::{announce_chrome_skip, is_missing_chrome_message};

const USER: &str = "canary";
const PASSWORD: &str = "CANARY-PW-7f3a";
const PAGE_PASSWORD: &str = "PAGE-PW-91c2";

fn browser_config() -> CrawlConfig {
    CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            mode: BrowserMode::Always,
            timeout: Duration::from_secs(20),
            extra_wait: Some(Duration::from_millis(1500)),
            ..BrowserConfig::default()
        },
        ..CrawlConfig::builder().allow_private_networks(true).build()
    }
}

fn basic_header() -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(format!("{USER}:{PASSWORD}"));
    format!("Basic {encoded}")
}

async fn mount(mock: &MockServer, route: &str, response: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(response)
        .mount(mock)
        .await;
}

fn png() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(b"\x89PNG\r\n".to_vec(), "image/png")
}

/// Scrape `url` in Chrome, or `None` when no usable Chrome exists on this host.
async fn scrape_in_browser(test_name: &str, url: &str) -> Option<Result<ScrapeResult, CrawlError>> {
    scrape_in_browser_with(test_name, browser_config(), url).await
}

/// Scrape `url` in Chrome with `config`, or `None` when no usable Chrome exists on this host.
async fn scrape_in_browser_with(
    test_name: &str,
    config: CrawlConfig,
    url: &str,
) -> Option<Result<ScrapeResult, CrawlError>> {
    let engine = create_engine(Some(config)).expect("engine must build");
    match scrape(&engine, url).await {
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
            None
        }
        outcome => Some(outcome),
    }
}

/// The request headers `mock` recorded for `at`, as `(name, value)` text.
async fn requests_for(mock: &MockServer, at: &str) -> Vec<Vec<(String, String)>> {
    mock.received_requests()
        .await
        .expect("request recording is on")
        .into_iter()
        .filter(|request| request.url.path() == at)
        .map(|request| {
            request
                .headers
                .iter()
                .map(|(name, value)| (name.as_str().to_owned(), value.to_str().unwrap_or_default().to_owned()))
                .collect()
        })
        .collect()
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(existing, _)| existing.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

#[tokio::test]
async fn the_credential_goes_to_seed_host_requests_only_and_cookies_still_go_out() {
    let seed = MockServer::start().await;
    let other = MockServer::start().await;
    let authority = seed.uri().trim_start_matches("http://").to_owned();
    let other_port = other.address().port();
    let page = format!(
        r#"<html><body><p>seed</p><img src="/seed.png"><img src="http://localhost:{other_port}/third.png"></body></html>"#
    );
    mount(
        &seed,
        "/",
        ResponseTemplate::new(200)
            .set_body_raw(page, "text/html")
            .append_header("set-cookie", "session=cookie-value; Path=/"),
    )
    .await;
    mount(&seed, "/seed.png", png()).await;
    mount(&other, "/third.png", png()).await;

    let Some(outcome) = scrape_in_browser(
        "the_credential_goes_to_seed_host_requests_only_and_cookies_still_go_out",
        &format!("http://{USER}:{PASSWORD}@{authority}/"),
    )
    .await
    else {
        return;
    };
    let result = outcome.expect("scrape must succeed");

    assert_eq!(result.final_url, format!("http://{authority}/"));
    let output = serde_json::to_string(&result).expect("result serializes");
    assert!(
        !output.contains(PASSWORD),
        "the password leaked into the result: {output}"
    );

    for at in ["/", "/seed.png"] {
        let requests = requests_for(&seed, at).await;
        assert!(!requests.is_empty(), "{at} on the seed host must have been requested");
        for headers in &requests {
            assert_eq!(
                header(headers, "authorization"),
                Some(basic_header().as_str()),
                "{at} on the seed host carries the caller's Basic header: {headers:?}"
            );
        }
    }
    let image = requests_for(&seed, "/seed.png").await;
    assert!(
        image
            .iter()
            .all(|headers| header(headers, "cookie").is_some_and(|cookie| cookie.contains("session=cookie-value"))),
        "the page's cookie still goes out next to the injected header: {image:?}"
    );

    let third_party = requests_for(&other, "/third.png").await;
    assert!(
        !third_party.is_empty(),
        "the third-party image must have been requested"
    );
    for headers in &third_party {
        assert_eq!(
            header(headers, "authorization"),
            None,
            "a third-party request never gets the credential: {headers:?}"
        );
    }
}

#[tokio::test]
async fn a_navigation_a_page_starts_to_a_url_with_userinfo_is_refused() {
    let seed = MockServer::start().await;
    let authority = seed.uri().trim_start_matches("http://").to_owned();
    // ~keep The beacon proves the script ran, so the absence of `/landed` below is a refusal
    // ~keep and not a script that never got the chance to navigate.
    let page = format!(
        r#"<html><body><p>seed</p><script>
        fetch("/beacon").finally(() => {{ location.href = "http://page:{PAGE_PASSWORD}@{authority}/landed"; }});
        </script></body></html>"#
    );
    mount(&seed, "/", ResponseTemplate::new(200).set_body_raw(page, "text/html")).await;
    mount(&seed, "/beacon", ResponseTemplate::new(204)).await;
    mount(
        &seed,
        "/landed",
        ResponseTemplate::new(200).set_body_raw("<html><body>landed</body></html>", "text/html"),
    )
    .await;

    let Some(outcome) = scrape_in_browser(
        "a_navigation_a_page_starts_to_a_url_with_userinfo_is_refused",
        &format!("http://{authority}/"),
    )
    .await
    else {
        return;
    };

    // ~keep A successful result carries the page's own HTML, which holds the password as the
    // ~keep page wrote it. Only an error is the engine's text.
    if let Err(error) = &outcome {
        let text = format!("{error} {error:?}");
        assert!(
            !text.contains(PAGE_PASSWORD),
            "the page's password leaked into the error: {text}"
        );
    }
    assert!(
        !requests_for(&seed, "/beacon").await.is_empty(),
        "the page's script must have run, or the absence below proves nothing"
    );
    assert!(
        requests_for(&seed, "/landed").await.is_empty(),
        "a navigation to a URL with userinfo must never reach the network"
    );
}

/// A page on the seed host that loads one image from the seed host and one from `other`.
async fn mount_seed_and_third_party(seed: &MockServer, other: &MockServer) {
    let other_port = other.address().port();
    let page = format!(
        r#"<html><body><p>seed</p><img src="/seed.png"><img src="http://localhost:{other_port}/third.png"></body></html>"#
    );
    mount(seed, "/", ResponseTemplate::new(200).set_body_raw(page, "text/html")).await;
    mount(seed, "/seed.png", png()).await;
    mount(other, "/third.png", png()).await;
}

/// The seed host got `expected` as its `Authorization` on the page and its image, and the
/// third-party image was requested without one.
async fn assert_scoped_authorization(seed: &MockServer, other: &MockServer, expected: &str) {
    for at in ["/", "/seed.png"] {
        let requests = requests_for(seed, at).await;
        assert!(!requests.is_empty(), "{at} on the seed host must have been requested");
        for headers in &requests {
            assert_eq!(
                header(headers, "authorization"),
                Some(expected),
                "{at} on the seed host carries the configured credential: {headers:?}"
            );
        }
    }
    let third_party = requests_for(other, "/third.png").await;
    assert!(
        !third_party.is_empty(),
        "the third-party image must have been requested"
    );
    for headers in &third_party {
        assert_eq!(
            header(headers, "authorization"),
            None,
            "a third-party request never gets the credential: {headers:?}"
        );
    }
}

fn bearer_config() -> CrawlConfig {
    CrawlConfig {
        auth: Some(AuthConfig::Bearer {
            token: BEARER.to_owned(),
        }),
        ..browser_config()
    }
}

const BEARER: &str = "bearer-canary";

#[tokio::test]
async fn configured_bearer_auth_goes_to_seed_host_requests_only() {
    let seed = MockServer::start().await;
    let other = MockServer::start().await;
    mount_seed_and_third_party(&seed, &other).await;

    let Some(outcome) = scrape_in_browser_with(
        "configured_bearer_auth_goes_to_seed_host_requests_only",
        bearer_config(),
        &format!("{}/", seed.uri()),
    )
    .await
    else {
        return;
    };
    outcome.expect("scrape must succeed");

    assert_scoped_authorization(&seed, &other, &format!("Bearer {BEARER}")).await;
}

#[cfg(feature = "interact")]
#[tokio::test]
async fn configured_bearer_auth_in_an_interaction_goes_to_seed_host_requests_only() {
    let seed = MockServer::start().await;
    let other = MockServer::start().await;
    mount_seed_and_third_party(&seed, &other).await;

    let engine = create_engine(Some(bearer_config())).expect("engine must build");
    match crawlberg::interact(&engine, &format!("{}/", seed.uri()), Vec::new()).await {
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(
                "configured_bearer_auth_in_an_interaction_goes_to_seed_host_requests_only",
                &message,
            );
            return;
        }
        outcome => {
            outcome.expect("the interaction must succeed");
        }
    }

    assert_scoped_authorization(&seed, &other, &format!("Bearer {BEARER}")).await;
}
