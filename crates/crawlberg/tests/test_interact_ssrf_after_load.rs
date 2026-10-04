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

/// Assert the action at `index` failed with the SSRF policy error that names the denied URL.
fn assert_action_refused(test_name: &str, result: &InteractionResult, index: usize) {
    let action = &result.action_results[index];
    let error = action.error.as_deref().unwrap_or_default();
    assert!(
        !action.success && error.contains("ssrf_policy_violation") && error.contains("/secret"),
        "{test_name}: action {index} must fail with the policy error, got {:?}",
        result.action_results
    );
}

#[tokio::test]
async fn interact_fails_a_click_whose_navigation_was_refused() {
    let test_name = "interact_fails_a_click_whose_navigation_was_refused";
    let denied = denied_server().await;
    let (_site, seed) = seed_site(&format!(r#"<a id="go" href="{}">go</a>"#, denied_url(&denied))).await;
    let Some(result) = run(test_name, &seed, vec![click("#go")]).await else {
        return;
    };
    assert_action_refused(test_name, &result, 0);
    assert!(
        result.action_results[1].success,
        "{test_name}: the wait after the click sent nothing refused: {:?}",
        result.action_results
    );
}

/// A script whose `fetch()` is refused fails: the refusal counts for the script action, or for
/// the wait after it when the check received the pause after the action's grace, as on a busy
/// host. Either way, exactly one action fails, and the address receives nothing.
#[tokio::test]
async fn interact_fails_a_script_whose_fetch_was_refused() {
    let test_name = "interact_fails_a_script_whose_fetch_was_refused";
    let denied = denied_server().await;
    let (_site, seed) = seed_site("<p>start</p>").await;
    let script = format!(
        "fetch({:?}, {{ mode: 'no-cors' }}).catch(() => {{}}); return true",
        denied_url(&denied)
    );
    let Some(result) = run(test_name, &seed, vec![execute_js(&script)]).await else {
        return;
    };
    let failed: Vec<usize> = result
        .action_results
        .iter()
        .enumerate()
        .filter(|(_, action)| !action.success)
        .map(|(index, _)| index)
        .collect();
    assert!(
        failed == [0] || failed == [1],
        "{test_name}: the script action or the wait after it must fail, and only one of them, got {:?}",
        result.action_results
    );
    assert_action_refused(test_name, &result, failed[0]);
    assert_refused(test_name, &denied, &result).await;
}

/// A request refused during the extra wait, after the navigation settled and before the first
/// action, fails no action, and the result lists its address.
#[tokio::test]
async fn interact_lists_a_request_refused_before_the_actions_and_fails_no_action() {
    let test_name = "interact_lists_a_request_refused_before_the_actions_and_fails_no_action";
    let denied = denied_server().await;
    let body = format!(
        "<p>start</p><script>setTimeout(() => fetch({:?}, {{ mode: 'no-cors' }}).catch(() => {{}}), 700);</script>",
        denied_url(&denied)
    );
    let (_site, seed) = seed_site(&body).await;
    let mut config = config();
    config.browser.extra_wait = Some(Duration::from_millis(1000));
    let engine = create_engine(Some(config)).expect("engine must build");
    let result = match interact(&engine, &seed, vec![execute_js("return 1")]).await {
        Ok(result) => result,
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
            return;
        }
        Err(error) => panic!("{test_name}: interact must succeed: {error:?}"),
    };
    assert!(
        result.action_results.iter().all(|action| action.success),
        "{test_name}: {:?}",
        result.action_results
    );
    assert_eq!(
        result.ssrf_refused_urls,
        [denied_url(&denied)],
        "{test_name}: the result must list the refused address"
    );
    assert_refused(test_name, &denied, &result).await;
}

/// A request a script schedules for later is not blamed on whichever action runs when it is
/// refused.
#[tokio::test]
async fn interact_fails_no_later_action_for_a_request_an_earlier_one_scheduled() {
    let test_name = "interact_fails_no_later_action_for_a_request_an_earlier_one_scheduled";
    let denied = denied_server().await;
    let (_site, seed) = seed_site("<p>start</p>").await;
    let schedule = format!(
        "setTimeout(() => fetch({:?}, {{ mode: 'no-cors' }}).catch(() => {{}}), 300); return true",
        denied_url(&denied)
    );
    let actions = vec![execute_js(&schedule), execute_js("return 1"), execute_js("return 2")];
    let engine = create_engine(Some(config())).expect("engine must build");
    let result = match interact(&engine, &seed, actions).await {
        Ok(result) => result,
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
            return;
        }
        Err(error) => panic!("{test_name}: interact must succeed: {error:?}"),
    };
    assert!(
        result.action_results.iter().all(|action| action.success),
        "{test_name}: {:?}",
        result.action_results
    );
}

/// On an external browser reached through `browser.endpoint`, the interaction's page starts
/// with the browser's own cookies.
#[tokio::test]
async fn interact_on_an_external_browser_starts_with_its_cookies() {
    let test_name = "interact_on_an_external_browser_starts_with_its_cookies";
    let (site, seed) = seed_site("<p>start</p>").await;
    let Some(mut other_client) = common::launch_external_chrome_with_cookie(test_name, &seed).await else {
        return;
    };
    let mut config = config();
    config.browser.endpoint = Some(other_client.websocket_address().clone());
    let engine = create_engine(Some(config)).expect("engine must build");
    let result = interact(&engine, &seed, vec![execute_js("return 1")]).await;
    let received = site.received_requests().await.expect("recording");
    let sent = received
        .iter()
        .find(|request| request.url.path() == "/")
        .and_then(|request| request.headers.get("cookie"))
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let _ = other_client.close().await;
    let _ = other_client.wait().await;
    result.unwrap_or_else(|error| panic!("{test_name}: interact must succeed: {error:?}"));
    assert!(
        sent.contains("owner=1"),
        "{test_name}: the interaction's page must start with the external browser's cookie, sent {sent:?}"
    );
}

/// A main-frame navigation refused during the extra wait, before any action, leaves Chrome's
/// error page in place of the page, so the session fails with the SSRF policy error.
#[tokio::test]
async fn interact_fails_when_the_page_navigates_to_a_denied_address_before_the_actions() {
    let test_name = "interact_fails_when_the_page_navigates_to_a_denied_address_before_the_actions";
    let denied = denied_server().await;
    let body = format!(
        "<p>start</p><script>setTimeout(() => {{ location.href = {:?}; }}, 500);</script>",
        denied_url(&denied)
    );
    let (_site, seed) = seed_site(&body).await;
    let mut config = config();
    config.browser.extra_wait = Some(Duration::from_millis(1500));
    let engine = create_engine(Some(config)).expect("engine must build");
    let outcome = interact(&engine, &seed, vec![execute_js("return 1")]).await;
    let received = denied.received_requests().await.expect("request recording is on");
    match outcome {
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
        }
        Err(CrawlError::SsrfPolicyViolation { url, .. }) => {
            assert_eq!(
                url,
                denied_url(&denied),
                "{test_name}: the error must name the refused address"
            );
            assert!(
                received.is_empty(),
                "{test_name}: the denied address must receive nothing"
            );
        }
        other => panic!("{test_name}: the session must fail with the SSRF policy error, got {other:?}"),
    }
}

/// A session that ends on Chrome's error page for a navigation the policy refused keeps its
/// result: the refusal fails the click and is listed, the final HTML is empty rather than Chrome's
/// page, and the final URL is the refused URL.
#[tokio::test]
async fn interact_that_ends_on_a_refused_navigation_returns_no_html_and_the_refused_url() {
    let test_name = "interact_that_ends_on_a_refused_navigation_returns_no_html_and_the_refused_url";
    let denied = denied_server().await;
    let (_site, seed) = seed_site(&format!(
        r#"<p>start</p><a id="go" href="{}">go</a>"#,
        denied_url(&denied)
    ))
    .await;
    let Some(result) = run(test_name, &seed, vec![click("#go")]).await else {
        return;
    };
    assert_action_refused(test_name, &result, 0);
    assert_eq!(
        (result.final_html.as_str(), result.final_url.as_str()),
        ("", denied_url(&denied).as_str()),
        "{test_name}: the session ends on the refused navigation"
    );
    assert!(
        result.ssrf_refused_urls.contains(&denied_url(&denied)),
        "{test_name}: the refused URL must be listed: {:?}",
        result.ssrf_refused_urls
    );
}
