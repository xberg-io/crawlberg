//! The SSRF policy covers every request an `interact` session makes, not only the first
//! navigation: a click, a form submission, a script `fetch()`, an iframe and a popup the
//! actions start are refused when their address is denied.
//!
//! The seed is served on `localhost`, which the policy allowlists by name. The denied target
//! is the literal address `127.0.0.1` on a second server, which `deny_private` refuses. Both
//! servers listen on the loopback interface, so the denied server counts every request Chrome
//! sends it.
//!
//! Requires a real Chrome binary; skipped (not failed) when Chrome is unavailable, matching the
//! other browser tests.

#![cfg(feature = "browser")]

use std::time::Duration;

use crawlberg::{
    BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig, CrawlError, HostMatcher, InteractionResult, PageAction,
    create_engine, interact,
};
use wiremock::matchers::{any, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::{announce_chrome_skip, is_missing_chrome_message};

const SECRET_MARKER: &str = "denied-marker";
const ALLOWED_MARKER: &str = "allowed-marker";

fn config() -> CrawlConfig {
    CrawlConfig {
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
    }
}

fn html(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(format!("<html><body>{body}</body></html>"), "text/html")
}

/// The server the policy denies. It answers every request with the secret marker.
async fn denied_server() -> MockServer {
    let denied = MockServer::start().await;
    Mock::given(any())
        .respond_with(html(SECRET_MARKER))
        .mount(&denied)
        .await;
    denied
}

/// The seed site, reached as `localhost`. `/` carries `body`; `/allowed` is a page the policy
/// permits.
async fn seed_site(body: &str) -> (MockServer, String) {
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(html(body))
        .mount(&site)
        .await;
    Mock::given(method("GET"))
        .and(path("/allowed"))
        .respond_with(html(ALLOWED_MARKER))
        .mount(&site)
        .await;
    let seed = format!("http://localhost:{}/", site.address().port());
    (site, seed)
}

/// The denied URL, addressed by the literal loopback IP the policy refuses.
fn denied_url(denied: &MockServer) -> String {
    format!("http://127.0.0.1:{}/secret", denied.address().port())
}

/// Run `actions` then a wait long enough for any request they start, or `None` without Chrome.
async fn run(test_name: &str, seed: &str, mut actions: Vec<PageAction>) -> Option<InteractionResult> {
    actions.push(PageAction::Wait {
        milliseconds: Some(1500),
        selector: None,
    });
    let engine = create_engine(Some(config())).expect("engine must build");
    match interact(&engine, seed, actions).await {
        Ok(result) => Some(result),
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
            None
        }
        Err(error) => panic!("{test_name}: interact must succeed: {error:?}"),
    }
}

fn execute_js(script: &str) -> PageAction {
    PageAction::ExecuteJs {
        script: script.to_owned(),
    }
}

fn click(selector: &str) -> PageAction {
    PageAction::Click {
        selector: selector.to_owned(),
    }
}

/// Assert the denied server received nothing and its content reached no result.
async fn assert_refused(test_name: &str, denied: &MockServer, result: &InteractionResult) {
    let received = denied.received_requests().await.expect("request recording is on");
    assert!(
        received.is_empty(),
        "{test_name}: the denied address must receive no request, got {:?}",
        received
            .iter()
            .map(|request| format!("{} {}", request.method, request.url))
            .collect::<Vec<_>>()
    );
    assert!(
        !result.final_html.contains(SECRET_MARKER),
        "{test_name}: the denied page must not be returned: {}",
        result.final_html
    );
}

#[tokio::test]
async fn interact_refuses_a_click_to_a_denied_address() {
    let test_name = "interact_refuses_a_click_to_a_denied_address";
    let denied = denied_server().await;
    let (_site, seed) = seed_site(&format!(r#"<a id="go" href="{}">go</a>"#, denied_url(&denied))).await;
    let Some(result) = run(test_name, &seed, vec![click("#go")]).await else {
        return;
    };
    assert_refused(test_name, &denied, &result).await;
}

#[tokio::test]
async fn interact_refuses_a_form_post_to_a_denied_address() {
    let test_name = "interact_refuses_a_form_post_to_a_denied_address";
    let denied = denied_server().await;
    let (_site, seed) = seed_site(&format!(
        r#"<form method="post" action="{}"><input name="field" value="value"><button id="send">send</button></form>"#,
        denied_url(&denied)
    ))
    .await;
    let Some(result) = run(test_name, &seed, vec![click("#send")]).await else {
        return;
    };
    assert_refused(test_name, &denied, &result).await;
}

#[tokio::test]
async fn interact_refuses_a_script_fetch_to_a_denied_address() {
    let test_name = "interact_refuses_a_script_fetch_to_a_denied_address";
    let denied = denied_server().await;
    let (_site, seed) = seed_site("<p>start</p>").await;
    let script = format!(
        "fetch({:?}, {{ mode: 'no-cors' }}).catch(() => {{}}); return true",
        denied_url(&denied)
    );
    let Some(result) = run(test_name, &seed, vec![execute_js(&script)]).await else {
        return;
    };
    assert_refused(test_name, &denied, &result).await;
}

#[tokio::test]
async fn interact_refuses_an_iframe_to_a_denied_address() {
    let test_name = "interact_refuses_an_iframe_to_a_denied_address";
    let denied = denied_server().await;
    let (_site, seed) = seed_site("<p>start</p>").await;
    let script = format!(
        "const frame = document.createElement('iframe'); frame.src = {:?}; document.body.appendChild(frame); return true",
        denied_url(&denied)
    );
    let Some(result) = run(test_name, &seed, vec![execute_js(&script)]).await else {
        return;
    };
    assert_refused(test_name, &denied, &result).await;
}

#[tokio::test]
async fn interact_refuses_a_popup_to_a_denied_address() {
    let test_name = "interact_refuses_a_popup_to_a_denied_address";
    let denied = denied_server().await;
    let (_site, seed) = seed_site("<p>start</p>").await;
    let script = format!("window.open({:?}); return true", denied_url(&denied));
    let Some(result) = run(test_name, &seed, vec![execute_js(&script)]).await else {
        return;
    };
    assert_refused(test_name, &denied, &result).await;
}

/// Control: a click to an address the policy permits still navigates.
#[tokio::test]
async fn interact_still_follows_a_click_to_an_allowed_address() {
    let test_name = "interact_still_follows_a_click_to_an_allowed_address";
    let (site, seed) = seed_site(r#"<a id="go" href="/allowed">go</a>"#).await;
    let Some(result) = run(test_name, &seed, vec![click("#go")]).await else {
        return;
    };
    assert!(
        result.final_url.ends_with("/allowed"),
        "{test_name}: the click must navigate: {}",
        result.final_url
    );
    assert!(
        result.final_html.contains(ALLOWED_MARKER),
        "{test_name}: the allowed page must load: {}",
        result.final_html
    );
    assert!(
        result.action_results.iter().all(|action| action.success),
        "{test_name}: {:?}",
        result.action_results
    );
    let requested: Vec<String> = site
        .received_requests()
        .await
        .expect("request recording is on")
        .iter()
        .map(|request| request.url.path().to_owned())
        .collect();
    assert!(requested.iter().any(|p| p == "/allowed"), "{requested:?}");
}
