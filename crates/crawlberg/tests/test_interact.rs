#[cfg(feature = "browser-chromiumoxide")]
mod common;
#[cfg(feature = "browser-chromiumoxide")]
use common::{announce_chrome_skip, is_missing_chrome_message};

#[cfg(any(feature = "browser-chromiumoxide", feature = "browser-native"))]
use std::time::Duration;

#[cfg(any(feature = "browser-chromiumoxide", feature = "browser-native"))]
use base64::Engine as _;
#[cfg(feature = "browser-chromiumoxide")]
use crawlberg::HostMatcher;
use crawlberg::ScrollDirection;
#[cfg(any(feature = "browser-chromiumoxide", feature = "browser-native"))]
use crawlberg::{BrowserBackend, BrowserConfig, BrowserMode};
use crawlberg::{CrawlConfig, CrawlError, PageAction, SsrfPolicy, create_engine, interact, validate_actions};

#[cfg(any(feature = "browser-chromiumoxide", feature = "browser-native"))]
use wiremock::matchers::{method, path};
#[cfg(any(feature = "browser-chromiumoxide", feature = "browser-native"))]
use wiremock::{Mock, MockServer, ResponseTemplate};

#[cfg(feature = "browser-native")]
const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
#[cfg(feature = "browser-native")]
const NATIVE_VIEWPORT_SCREENSHOT_HEIGHT: u32 = 720;

#[cfg(feature = "browser-native")]
fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || !bytes.starts_with(PNG_SIGNATURE) {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some((width, height))
}

/// Builds a `CrawlConfig` whose SSRF policy permits private networks, so wiremock's
/// 127.0.0.1 servers are reachable.
///
// ~keep Uses the `allow_private_networks` config seam rather than the
// `CRAWLBERG_ALLOW_PRIVATE_NETWORK` env var: writing that variable is a process-global mutation
// that races every concurrent `std::env::var` read (`SsrfPolicy::from_env`, reached from
// `CrawlConfig::default()`) in this binary's other tests, aborting the process on glibc
// with no failing test name.
#[cfg(any(feature = "browser-chromiumoxide", feature = "browser-native"))]
fn allow_private_config() -> CrawlConfig {
    CrawlConfig::builder().allow_private_networks(true).build()
}

#[cfg(feature = "browser-chromiumoxide")]
#[tokio::test]
async fn chromiumoxide_interact_click_wait_screenshot_and_scrape() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"
                    <html>
                      <body style="height: 2000px">
                        <button id="go">Go</button>
                        <div id="status">idle</div>
                        <script>
                          document.getElementById('go').addEventListener('click', () => {
                            document.getElementById('status').textContent = 'clicked';
                            const done = document.createElement('div');
                            done.id = 'done';
                            done.textContent = 'ready';
                            document.body.appendChild(done);
                          });
                        </script>
                      </body>
                    </html>
                    "#,
            "text/html",
        ))
        .mount(&mock)
        .await;

    let config = CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            mode: BrowserMode::Always,
            timeout: Duration::from_secs(15),
            eval_script: Some("document.body.setAttribute('data-eval-script', 'ran')".to_string()),
            ..BrowserConfig::default()
        },
        ..allow_private_config()
    };
    let engine = create_engine(Some(config)).unwrap();

    let result = interact(
        &engine,
        &mock.uri(),
        vec![
            PageAction::Click {
                selector: "#go".to_string(),
            },
            PageAction::Wait {
                milliseconds: None,
                selector: Some("#done".to_string()),
            },
            PageAction::Scroll {
                direction: ScrollDirection::Down,
                selector: None,
                amount: Some(100),
            },
            PageAction::Screenshot { full_page: Some(false) },
            PageAction::Scrape,
        ],
    )
    .await;

    let result = match result {
        Ok(result) => result,
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip("chromiumoxide_interact_click_wait_screenshot_and_scrape", &message);
            return;
        }
        Err(error) => panic!("interact should succeed: {error:?}"),
    };

    assert_eq!(result.action_results.len(), 5);
    assert!(
        result.action_results.iter().all(|action| action.success),
        "all actions should succeed: {:?}",
        result.action_results
    );
    assert!(result.final_html.contains("clicked"));
    assert!(result.final_html.contains("id=\"done\""));
    assert!(result.final_html.contains("data-eval-script=\"ran\""));
    let screenshot_bytes = result
        .screenshot
        .as_ref()
        .expect("screenshot action must populate InteractionResult.screenshot");
    assert!(!screenshot_bytes.is_empty());
    let expected_screenshot_base64 = base64::engine::general_purpose::STANDARD.encode(screenshot_bytes);
    assert_eq!(
        result.screenshot_base64.as_deref(),
        Some(expected_screenshot_base64.as_str()),
        "screenshot_base64 must carry the same bytes as screenshot so bindings do not lose them"
    );
    let scrape_data = result
        .action_results
        .last()
        .and_then(|action| action.data.as_ref())
        .and_then(|data| data.get("html"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    assert!(scrape_data.contains("clicked"));
}

/// A server with a start page at `/` that runs `late_navigation` 300 ms after it loads, and a
/// download at `/dl` that answers 501. Chrome cannot show that download, so it commits its own
/// error page in place of the start page.
#[cfg(feature = "browser-chromiumoxide")]
async fn late_501_download_site(late_navigation: &str) -> MockServer {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            format!(
                "<html><body><p id=\"start\">start page</p>\
                 <script>setTimeout(() => {{ {late_navigation} }}, 300)</script></body></html>"
            ),
            "text/html",
        ))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/dl"))
        .respond_with(
            ResponseTemplate::new(501)
                .set_body_raw("bin", "application/octet-stream")
                .append_header("content-disposition", "attachment; filename=x.bin"),
        )
        .mount(&mock)
        .await;
    mock
}

#[cfg(feature = "browser-chromiumoxide")]
async fn assert_download_requested(test_name: &str, mock: &MockServer) {
    let requests = mock.received_requests().await.unwrap_or_default();
    assert!(
        requests.iter().any(|request| request.url.path() == "/dl"),
        "{test_name}: the start page never reached the download, so Chrome never showed its error page"
    );
}

#[cfg(feature = "browser-chromiumoxide")]
fn chromiumoxide_interact_config() -> CrawlConfig {
    CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            mode: BrowserMode::Always,
            timeout: Duration::from_secs(15),
            ..BrowserConfig::default()
        },
        ..allow_private_config()
    }
}

/// A session that ends on Chrome's error page fails with a browser error that names the URL Chrome
/// could not show, with its credentials redacted. It never returns Chrome's page as the final HTML.
#[cfg(feature = "browser-chromiumoxide")]
#[tokio::test]
async fn chromiumoxide_interact_fails_when_the_session_ends_on_chrome_s_error_page() {
    let test_name = "chromiumoxide_interact_fails_when_the_session_ends_on_chrome_s_error_page";
    let mock = late_501_download_site(
        "location.assign(new URL('/dl', location.href).href.replace('http://', 'http://user:secret@'))",
    )
    .await;
    let engine = create_engine(Some(chromiumoxide_interact_config())).unwrap();

    let result = interact(
        &engine,
        &mock.uri(),
        vec![
            PageAction::Wait {
                milliseconds: Some(2000),
                selector: None,
            },
            PageAction::Scrape,
        ],
    )
    .await;

    match result {
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
        }
        Err(CrawlError::BrowserError { message, .. }) => {
            assert_download_requested(test_name, &mock).await;
            assert!(
                message.contains("***:***@") && message.contains("/dl") && message.contains("error page"),
                "{test_name}: the error must name the redacted URL Chrome could not show: {message}"
            );
            assert!(
                !message.contains("secret"),
                "{test_name}: the error must not carry the URL's password: {message}"
            );
        }
        other => panic!("{test_name}: Chrome's error page must fail the session: {other:?}"),
    }
}

/// A Scrape action run while the page shows Chrome's error page fails, and its data is never that
/// page. The session then goes back to the start page, so it ends on a real page and succeeds.
#[cfg(feature = "browser-chromiumoxide")]
#[tokio::test]
async fn chromiumoxide_interact_scrape_fails_on_chrome_s_error_page() {
    let test_name = "chromiumoxide_interact_scrape_fails_on_chrome_s_error_page";
    // ~keep The flag stops the start page from starting the download again when the session
    // ~keep goes back to it.
    let mock = late_501_download_site(
        "if (!sessionStorage.getItem('left')) { sessionStorage.setItem('left', '1'); location.assign('/dl'); }",
    )
    .await;
    let engine = create_engine(Some(chromiumoxide_interact_config())).unwrap();

    let result = interact(
        &engine,
        &mock.uri(),
        vec![
            PageAction::Wait {
                milliseconds: Some(2000),
                selector: None,
            },
            PageAction::Scrape,
            PageAction::ExecuteJs {
                script: "history.back()".to_string(),
            },
            PageAction::Wait {
                milliseconds: Some(2000),
                selector: None,
            },
        ],
    )
    .await;

    let result = match result {
        Ok(result) => result,
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
            return;
        }
        Err(error) => panic!("{test_name}: the session ends on the start page and must succeed: {error:?}"),
    };

    assert_download_requested(test_name, &mock).await;
    let scrape = &result.action_results[1];
    assert_eq!(scrape.action_type, "scrape", "{test_name}");
    assert!(
        !scrape.success && scrape.data.is_none(),
        "{test_name}: the Scrape action must fail with no data on Chrome's error page: {scrape:?}"
    );
    let error = scrape.error.as_deref().unwrap_or_default();
    assert!(
        error.contains("/dl") && error.contains("error page"),
        "{test_name}: the Scrape error must name the URL Chrome could not show: {error}"
    );
    assert!(
        result.final_html.contains("start page"),
        "{test_name}: the session must end on the start page: {}",
        result.final_html
    );
}

/// An ExecuteJs action run while the page shows Chrome's error page fails, through a check of
/// the page made just before the script runs (#355). A second ExecuteJs action then goes back to
/// the start page, so the session ends on a real page and interact() succeeds.
#[cfg(feature = "browser-chromiumoxide")]
#[tokio::test]
async fn chromiumoxide_interact_execute_js_fails_on_chrome_s_error_page() {
    let test_name = "chromiumoxide_interact_execute_js_fails_on_chrome_s_error_page";
    // ~keep The flag stops the start page from starting the download again when the session
    // ~keep goes back to it.
    let mock = late_501_download_site(
        "if (!sessionStorage.getItem('left')) { sessionStorage.setItem('left', '1'); location.assign('/dl'); }",
    )
    .await;
    let engine = create_engine(Some(chromiumoxide_interact_config())).unwrap();

    let result = interact(
        &engine,
        &mock.uri(),
        vec![
            PageAction::Wait {
                milliseconds: Some(2000),
                selector: None,
            },
            PageAction::ExecuteJs {
                script: "document.title".to_string(),
            },
            PageAction::ExecuteJs {
                script: "history.back()".to_string(),
            },
            PageAction::Wait {
                milliseconds: Some(2000),
                selector: None,
            },
        ],
    )
    .await;

    let result = match result {
        Ok(result) => result,
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
            return;
        }
        Err(error) => panic!("{test_name}: the session ends on the start page and must succeed: {error:?}"),
    };

    assert_download_requested(test_name, &mock).await;
    let execute_js = &result.action_results[1];
    assert_eq!(execute_js.action_type, "executeJs", "{test_name}");
    assert!(
        !execute_js.success && execute_js.data.is_none(),
        "{test_name}: the ExecuteJs action must fail with no data on Chrome's error page: {execute_js:?}"
    );
    let error = execute_js.error.as_deref().unwrap_or_default();
    assert!(
        error.contains("/dl") && error.contains("error page"),
        "{test_name}: the ExecuteJs error must name the URL Chrome could not show: {error}"
    );
    assert!(
        result.final_html.contains("start page"),
        "{test_name}: the session must end on the start page: {}",
        result.final_html
    );
}

/// A Screenshot action run while the page shows Chrome's error page fails, through a check of
/// the page made just before the capture (#355). The session then goes back to the start page,
/// so it ends on a real page and succeeds.
#[cfg(feature = "browser-chromiumoxide")]
#[tokio::test]
async fn chromiumoxide_interact_screenshot_fails_on_chrome_s_error_page() {
    let test_name = "chromiumoxide_interact_screenshot_fails_on_chrome_s_error_page";
    // ~keep The flag stops the start page from starting the download again when the session
    // ~keep goes back to it.
    let mock = late_501_download_site(
        "if (!sessionStorage.getItem('left')) { sessionStorage.setItem('left', '1'); location.assign('/dl'); }",
    )
    .await;
    let engine = create_engine(Some(chromiumoxide_interact_config())).unwrap();

    let result = interact(
        &engine,
        &mock.uri(),
        vec![
            PageAction::Wait {
                milliseconds: Some(2000),
                selector: None,
            },
            PageAction::Screenshot { full_page: Some(false) },
            PageAction::ExecuteJs {
                script: "history.back()".to_string(),
            },
            PageAction::Wait {
                milliseconds: Some(2000),
                selector: None,
            },
        ],
    )
    .await;

    let result = match result {
        Ok(result) => result,
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
            return;
        }
        Err(error) => panic!("{test_name}: the session ends on the start page and must succeed: {error:?}"),
    };

    assert_download_requested(test_name, &mock).await;
    let screenshot_action = &result.action_results[1];
    assert_eq!(screenshot_action.action_type, "screenshot", "{test_name}");
    assert!(
        !screenshot_action.success && screenshot_action.data.is_none(),
        "{test_name}: the Screenshot action must fail with no data on Chrome's error page: {screenshot_action:?}"
    );
    let error = screenshot_action.error.as_deref().unwrap_or_default();
    assert!(
        error.contains("/dl") && error.contains("error page"),
        "{test_name}: the Screenshot error must name the URL Chrome could not show: {error}"
    );
    assert!(
        result.final_html.contains("start page"),
        "{test_name}: the session must end on the start page: {}",
        result.final_html
    );
}

/// A script run on Chrome's error page that navigates away from it reports the failure, because
/// the page was checked before the script ran, and the script runs exactly once: the session ends
/// on the start page, where one `history.back()` leads. Run twice, it would go past the start page
/// to `about:blank`; not run, the session would end on the error page and fail.
#[cfg(feature = "browser-chromiumoxide")]
#[tokio::test]
async fn chromiumoxide_interact_script_that_leaves_chrome_s_error_page_runs_once_and_fails() {
    let test_name = "chromiumoxide_interact_script_that_leaves_chrome_s_error_page_runs_once_and_fails";
    // ~keep The flag stops the start page from starting the download again when the session
    // ~keep goes back to it.
    let mock = late_501_download_site(
        "if (!sessionStorage.getItem('left')) { sessionStorage.setItem('left', '1'); location.assign('/dl'); }",
    )
    .await;
    let engine = create_engine(Some(chromiumoxide_interact_config())).unwrap();

    let result = interact(
        &engine,
        &mock.uri(),
        vec![
            PageAction::Wait {
                milliseconds: Some(2000),
                selector: None,
            },
            PageAction::ExecuteJs {
                script: "history.back()".to_string(),
            },
            PageAction::Wait {
                milliseconds: Some(2000),
                selector: None,
            },
        ],
    )
    .await;

    let result = match result {
        Ok(result) => result,
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
            return;
        }
        Err(error) => panic!("{test_name}: the script must leave the error page, so the session succeeds: {error:?}"),
    };

    assert_download_requested(test_name, &mock).await;
    let execute_js = &result.action_results[1];
    assert_eq!(execute_js.action_type, "executeJs", "{test_name}");
    assert!(
        !execute_js.success && execute_js.data.is_none(),
        "{test_name}: a script run on Chrome's error page must report failure even when it navigates \
         away: {execute_js:?}"
    );
    let error = execute_js.error.as_deref().unwrap_or_default();
    assert!(
        error.contains("/dl") && error.contains("error page"),
        "{test_name}: the ExecuteJs error must name the URL Chrome could not show: {error}"
    );
    assert!(
        result.final_html.contains("start page"),
        "{test_name}: the script must run once, going back to the start page and no further: {}",
        result.final_html
    );
}

/// A page that navigates to a refused address during the wait fails the session with the SSRF
/// policy error, even though Chrome reports the navigation it waited for as fine (#369). The
/// error does not carry the credentials of the refused URL.
#[cfg(feature = "browser-chromiumoxide")]
#[tokio::test]
async fn chromiumoxide_interact_fails_on_a_refused_navigation_during_the_wait() {
    let test_name = "chromiumoxide_interact_fails_on_a_refused_navigation_during_the_wait";
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "<html><body><p>start page</p><script>setTimeout(() => \
             location.assign('http://user:secret@169.254.169.254/latest'), 50)</script></body></html>",
            "text/html",
        ))
        .mount(&mock)
        .await;
    // ~keep The allowlist lets the mock server's loopback address through and keeps every other
    // ~keep private address refused, so only the page's own navigation is blocked.
    let config = CrawlConfig {
        ssrf: SsrfPolicy {
            allowlist: vec![HostMatcher::cidr("127.0.0.0/8").expect("literal CIDR is valid")],
            ..SsrfPolicy::default()
        },
        ..chromiumoxide_interact_config()
    };
    let engine = create_engine(Some(config)).unwrap();

    let result = interact(
        &engine,
        &mock.uri(),
        vec![PageAction::Wait {
            milliseconds: Some(100),
            selector: None,
        }],
    )
    .await;

    match result {
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
        }
        Err(error @ CrawlError::SsrfPolicyViolation { .. }) => {
            let message = error.to_string();
            assert!(
                message.contains("169.254.169.254"),
                "{test_name}: the error must name the refused address: {message}"
            );
            assert!(
                !message.contains("secret"),
                "{test_name}: the error must not carry the URL's password: {message}"
            );
        }
        other => panic!("{test_name}: the refused navigation must fail with the SSRF policy error: {other:?}"),
    }
}

/// A refused iframe document must not fail the session: only a refused main-frame navigation
/// does.
#[cfg(feature = "browser-chromiumoxide")]
#[tokio::test]
async fn chromiumoxide_interact_succeeds_with_a_refused_iframe() {
    let test_name = "chromiumoxide_interact_succeeds_with_a_refused_iframe";
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "<html><body><p>start</p><iframe src=\"http://169.254.169.254/frame\"></iframe></body></html>",
            "text/html",
        ))
        .mount(&mock)
        .await;
    // ~keep Same allowlist as the sibling refused-navigation test above: the mock server's
    // ~keep loopback address is let through, every other private address stays refused.
    let config = CrawlConfig {
        ssrf: SsrfPolicy {
            allowlist: vec![HostMatcher::cidr("127.0.0.0/8").expect("literal CIDR is valid")],
            ..SsrfPolicy::default()
        },
        ..chromiumoxide_interact_config()
    };
    let engine = create_engine(Some(config)).unwrap();

    let result = interact(
        &engine,
        &mock.uri(),
        vec![PageAction::Wait {
            milliseconds: Some(300),
            selector: None,
        }],
    )
    .await;

    match result {
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
        }
        Ok(result) => {
            assert!(
                result.final_html.contains("start"),
                "{test_name}: a refused iframe document must not fail the session: {}",
                result.final_html
            );
        }
        other => panic!("{test_name}: a refused iframe document must not fail the session: {other:?}"),
    }
}

#[cfg(feature = "browser-native")]
#[tokio::test]
async fn native_interact_click_type_wait_scroll_execute_js_and_scrape() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"
                    <html>
                      <body style="height: 2000px">
                        <button id="go">Go</button>
                        <form id="form" action="/submitted">
                          <input id="name" name="name" value="">
                        </form>
                        <div id="status">idle</div>
                        <div id="events"></div>
                        <script>
                          const events = [];
                          const record = event => {
                            events.push(event.type);
                            document.getElementById('events').textContent = events.join(',');
                          };
                          document.getElementById('go').addEventListener('click', () => {
                            document.getElementById('status').textContent = 'clicked';
                            const done = document.createElement('div');
                            done.id = 'done';
                            done.textContent = 'ready';
                            document.body.appendChild(done);
                          });
                          document.getElementById('go').addEventListener('mousedown', record);
                          document.getElementById('go').addEventListener('click', record);
                          document.getElementById('go').addEventListener('mouseup', record);
                          document.getElementById('name').addEventListener('input', () => {
                            document.body.setAttribute('data-name', document.getElementById('name').value);
                          });
                        </script>
                      </body>
                    </html>
                    "#,
            "text/html",
        ))
        .mount(&mock)
        .await;

    let config = CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Native,
            mode: BrowserMode::Always,
            timeout: Duration::from_secs(15),
            eval_script: Some("document.body.setAttribute('data-eval-script', 'ran')".to_string()),
            ..BrowserConfig::default()
        },
        ..allow_private_config()
    };
    let engine = create_engine(Some(config)).unwrap();

    let result = interact(
        &engine,
        &mock.uri(),
        vec![
            PageAction::Click {
                selector: "#go".to_string(),
            },
            PageAction::TypeText {
                selector: "#name".to_string(),
                text: "crawlberg".to_string(),
            },
            PageAction::Wait {
                milliseconds: None,
                selector: Some("#done".to_string()),
            },
            PageAction::Scroll {
                direction: ScrollDirection::Down,
                selector: None,
                amount: Some(100),
            },
            PageAction::ExecuteJs {
                script: "document.querySelector('#name').value".to_string(),
            },
            PageAction::Press {
                key: "Backspace".to_string(),
            },
            PageAction::ExecuteJs {
                script: "document.querySelector('#name').value".to_string(),
            },
            PageAction::ExecuteJs {
                script: "throw new Error('boom')".to_string(),
            },
            PageAction::Wait {
                milliseconds: Some(1_000),
                selector: Some("##".to_string()),
            },
            PageAction::Screenshot { full_page: Some(false) },
            PageAction::Scrape,
        ],
    )
    .await
    .unwrap();

    assert_eq!(result.action_results.len(), 11);
    assert!(
        result.action_results[..7].iter().all(|action| action.success),
        "non-screenshot actions should succeed: {:?}",
        result.action_results
    );
    assert!(!result.action_results[7].success);
    assert!(
        result.action_results[7]
            .error
            .as_deref()
            .is_some_and(|error| !error.is_empty())
    );
    assert!(!result.action_results[8].success);
    assert!(
        result.action_results[8]
            .error
            .as_deref()
            .is_some_and(|error| error.contains("selector syntax error")),
        "invalid selector should surface an evaluation error: {:?}",
        result.action_results[8]
    );
    assert!(result.action_results[9].success);
    assert!(result.action_results[10].success);
    assert!(result.final_html.contains("clicked"));
    assert!(result.final_html.contains("id=\"done\""));
    assert!(result.final_html.contains("data-eval-script=\"ran\""));
    assert!(result.final_html.contains("mousedown,mouseup,click"));
    assert!(result.final_html.contains("data-name=\"crawlber"));
    assert!(
        result
            .screenshot
            .as_deref()
            .is_some_and(|bytes| bytes.starts_with(PNG_SIGNATURE))
    );

    let typed_value = result.action_results[4]
        .data
        .as_ref()
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    assert_eq!(typed_value, "crawlberg");

    let backspaced_value = result.action_results[6]
        .data
        .as_ref()
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    assert_eq!(backspaced_value, "crawlber");

    let screenshot_data = result.action_results[9]
        .data
        .as_ref()
        .expect("screenshot action should return metadata");
    assert_eq!(
        screenshot_data.get("format").and_then(serde_json::Value::as_str),
        Some("png")
    );
    assert_eq!(
        screenshot_data.get("full_page").and_then(serde_json::Value::as_bool),
        Some(false)
    );
    assert!(
        screenshot_data
            .get("bytes")
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|bytes| bytes > 0)
    );

    let scrape_data = result.action_results[10]
        .data
        .as_ref()
        .and_then(|data| data.get("html"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    assert!(scrape_data.contains("clicked"));
}

#[cfg(feature = "browser-native")]
#[tokio::test]
async fn native_interact_full_page_screenshot_returns_png() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"
                    <html>
                      <body style="margin: 0">
                        <main style="height: 1800px; background: #f6f6f6">
                          <h1>Native screenshot</h1>
                        </main>
                      </body>
                    </html>
                    "#,
            "text/html",
        ))
        .mount(&mock)
        .await;

    let config = CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Native,
            mode: BrowserMode::Always,
            timeout: Duration::from_secs(15),
            ..BrowserConfig::default()
        },
        ..allow_private_config()
    };
    let engine = create_engine(Some(config)).unwrap();

    let result = interact(
        &engine,
        &mock.uri(),
        vec![PageAction::Screenshot { full_page: Some(true) }],
    )
    .await
    .unwrap();

    assert_eq!(result.action_results.len(), 1);
    assert!(result.action_results[0].success);
    assert!(
        result
            .screenshot
            .as_deref()
            .is_some_and(|bytes| bytes.starts_with(PNG_SIGNATURE))
    );
    assert_eq!(
        result.action_results[0]
            .data
            .as_ref()
            .and_then(|data| data.get("full_page"))
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    assert!(
        result
            .screenshot
            .as_deref()
            .and_then(png_dimensions)
            .is_some_and(|(_, height)| height > NATIVE_VIEWPORT_SCREENSHOT_HEIGHT)
    );
    let screenshot_bytes = result
        .screenshot
        .as_ref()
        .expect("screenshot action must populate InteractionResult.screenshot");
    let expected_screenshot_base64 = base64::engine::general_purpose::STANDARD.encode(screenshot_bytes);
    assert_eq!(
        result.screenshot_base64.as_deref(),
        Some(expected_screenshot_base64.as_str()),
        "screenshot_base64 must carry the same bytes as screenshot so bindings do not lose them"
    );
}

#[cfg(feature = "browser-native")]
#[tokio::test]
async fn native_interact_link_click_navigates() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"
                    <html>
                      <body>
                        <a id="next" href="/next">Next</a>
                      </body>
                    </html>
                    "#,
            "text/html",
        ))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/next"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"<html><body><h1 id="arrived">Arrived</h1></body></html>"#,
            "text/html",
        ))
        .mount(&mock)
        .await;

    let config = CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Native,
            mode: BrowserMode::Always,
            timeout: Duration::from_secs(15),
            ..BrowserConfig::default()
        },
        ..allow_private_config()
    };
    let engine = create_engine(Some(config)).unwrap();

    let result = interact(
        &engine,
        &mock.uri(),
        vec![
            PageAction::Click {
                selector: "#next".to_string(),
            },
            PageAction::Scrape,
        ],
    )
    .await
    .unwrap();

    assert!(result.action_results.iter().all(|action| action.success));
    assert!(result.final_url.ends_with("/next"));
    assert!(result.final_html.contains("id=\"arrived\""));
}

#[cfg(feature = "browser-native")]
#[tokio::test]
async fn native_interact_click_respects_prevent_default() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"
                    <html>
                      <body>
                        <a id="next" href="/next">Next</a>
                        <div id="status">idle</div>
                        <script>
                          document.getElementById('next').addEventListener('click', event => {
                            event.preventDefault();
                            document.getElementById('status').textContent = 'stayed';
                          });
                        </script>
                      </body>
                    </html>
                    "#,
            "text/html",
        ))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/next"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"<html><body><h1 id="arrived">Arrived</h1></body></html>"#,
            "text/html",
        ))
        .mount(&mock)
        .await;

    let config = CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Native,
            mode: BrowserMode::Always,
            timeout: Duration::from_secs(15),
            ..BrowserConfig::default()
        },
        ..allow_private_config()
    };
    let engine = create_engine(Some(config)).unwrap();

    let result = interact(
        &engine,
        &mock.uri(),
        vec![
            PageAction::Click {
                selector: "#next".to_string(),
            },
            PageAction::Scrape,
        ],
    )
    .await
    .unwrap();

    assert!(result.action_results.iter().all(|action| action.success));
    assert_eq!(result.final_url, format!("{}/", mock.uri()));
    assert!(result.final_html.contains("stayed"));
    assert!(!result.final_html.contains("id=\"arrived\""));
}

#[cfg(feature = "browser-native")]
#[tokio::test]
async fn native_interact_press_enter_submits_focused_form() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"
                    <html>
                      <body>
                        <form action="/submitted">
                          <input id="name" name="name" value="">
                        </form>
                      </body>
                    </html>
                    "#,
            "text/html",
        ))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/submitted"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"<html><body><h1 id="submitted">Submitted</h1></body></html>"#,
            "text/html",
        ))
        .mount(&mock)
        .await;

    let config = CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Native,
            mode: BrowserMode::Always,
            timeout: Duration::from_secs(15),
            ..BrowserConfig::default()
        },
        ..allow_private_config()
    };
    let engine = create_engine(Some(config)).unwrap();

    let result = interact(
        &engine,
        &mock.uri(),
        vec![
            PageAction::TypeText {
                selector: "#name".to_string(),
                text: "ada".to_string(),
            },
            PageAction::Press {
                key: "Enter".to_string(),
            },
            PageAction::Scrape,
        ],
    )
    .await
    .unwrap();

    assert!(result.action_results.iter().all(|action| action.success));
    assert!(
        result.final_url.ends_with("/submitted?name=ada"),
        "unexpected final URL: {}",
        result.final_url
    );
    assert!(result.final_html.contains("id=\"submitted\""));
}

#[cfg(feature = "browser-native")]
#[tokio::test]
async fn native_interact_keyboard_prevent_default_blocks_defaults() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"
                    <html>
                      <body>
                        <form action="/submitted">
                          <input id="name" name="name" value="keep">
                        </form>
                        <script>
                          const input = document.getElementById('name');
                          input.addEventListener('keypress', event => {
                            if (event.key === 'x') event.preventDefault();
                          });
                          input.addEventListener('keydown', event => {
                            if (event.key === 'Backspace' || event.key === 'Enter') {
                              event.preventDefault();
                            }
                          });
                        </script>
                      </body>
                    </html>
                    "#,
            "text/html",
        ))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/submitted"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"<html><body><h1 id="submitted">Submitted</h1></body></html>"#,
            "text/html",
        ))
        .mount(&mock)
        .await;

    let config = CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Native,
            mode: BrowserMode::Always,
            timeout: Duration::from_secs(15),
            ..BrowserConfig::default()
        },
        ..allow_private_config()
    };
    let engine = create_engine(Some(config)).unwrap();

    let result = interact(
        &engine,
        &mock.uri(),
        vec![
            PageAction::TypeText {
                selector: "#name".to_string(),
                text: "x".to_string(),
            },
            PageAction::ExecuteJs {
                script: "document.querySelector('#name').value".to_string(),
            },
            PageAction::Press {
                key: "Backspace".to_string(),
            },
            PageAction::ExecuteJs {
                script: "document.querySelector('#name').value".to_string(),
            },
            PageAction::Press {
                key: "Enter".to_string(),
            },
            PageAction::Scrape,
        ],
    )
    .await
    .unwrap();

    assert!(result.action_results.iter().all(|action| action.success));
    let after_type = result.action_results[1]
        .data
        .as_ref()
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let after_backspace = result.action_results[3]
        .data
        .as_ref()
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    assert_eq!(after_type, "keep");
    assert_eq!(after_backspace, "keep");
    assert_eq!(result.final_url, format!("{}/", mock.uri()));
    assert!(!result.final_html.contains("id=\"submitted\""));
}

#[test]
fn validation_rejects_empty_wait_and_scroll_selectors() {
    let wait = validate_actions(&[PageAction::Wait {
        milliseconds: None,
        selector: Some(String::new()),
    }]);
    assert!(matches!(wait, Err(CrawlError::InvalidConfig { message, .. }) if message.contains("wait selector")));

    let scroll = validate_actions(&[PageAction::Scroll {
        direction: ScrollDirection::Down,
        selector: Some(String::new()),
        amount: None,
    }]);
    assert!(matches!(scroll, Err(CrawlError::InvalidConfig { message, .. }) if message.contains("scroll selector")));
}

#[test]
fn validation_rejects_i64_min_scroll_amount() {
    let result = validate_actions(&[PageAction::Scroll {
        direction: ScrollDirection::Down,
        selector: None,
        amount: Some(i64::MIN),
    }]);

    assert!(matches!(result, Err(CrawlError::InvalidConfig { message, .. }) if message.contains("scroll amount")));
}

#[cfg(not(feature = "browser-chromiumoxide"))]
#[tokio::test]
async fn no_chromiumoxide_backend_interact_returns_unsupported() {
    let engine = create_engine(None).unwrap();

    let result = interact(&engine, "https://example.com", vec![PageAction::Scrape]).await;

    assert!(
        matches!(&result, Err(CrawlError::Unsupported { message, .. }) if message.contains("browser-chromiumoxide")),
        "expected Unsupported mentioning browser-chromiumoxide, got {result:?}"
    );
}

/// The deny-side counterpart to `allow_private_config` above: explicit `SsrfPolicy::default()`
/// rather than `CrawlConfig::default()`/`CrawlConfig::builder().build()`, both of which read
/// `SsrfPolicy::from_env()` and so would make this test's denial depend on
/// `CRAWLBERG_ALLOW_PRIVATE_NETWORK` in the ambient environment. Deny-private is pinned here
/// regardless of env, for the same reason `allow_private_config` pins allow-private.
fn deny_private_config() -> CrawlConfig {
    CrawlConfig {
        ssrf: SsrfPolicy::default(),
        ..CrawlConfig::builder().build()
    }
}

/// xberg-io/crawlberg#74: `interact()` on the default `BrowserBackend::Chromiumoxide` backend
/// enforced no SSRF policy at all. This must fail on the pre-flight check in `interact::run`,
/// before any browser is launched -- a literal IP keeps this hermetic (no DNS) and needs no
/// Chrome binary, so it runs in CI on every platform regardless of which browser feature is
/// compiled in.
#[tokio::test]
async fn interact_rejects_a_private_target_before_launching_any_browser() {
    let engine = create_engine(Some(deny_private_config())).unwrap();

    let result = interact(&engine, "http://127.0.0.1:9/", vec![PageAction::Scrape]).await;

    assert!(
        matches!(&result, Err(CrawlError::SsrfPolicyViolation { .. })),
        "a loopback target must be rejected by SSRF policy before any browser work, got {result:?}"
    );
    let message = result.unwrap_err().to_string();
    assert!(
        message.contains("ssrf_policy_violation"),
        "the hand-written e2e suites match on the literal `ssrf_policy_violation` string; got: {message}"
    );
}

/// Same as above for a cloud-metadata address, the other address class the interception layer
/// (`ssrf_intercept.rs`) names explicitly. Also needs no Chrome.
#[tokio::test]
async fn interact_rejects_a_cloud_metadata_target_before_launching_any_browser() {
    let engine = create_engine(Some(deny_private_config())).unwrap();

    let result = interact(
        &engine,
        "http://169.254.169.254/latest/meta-data/",
        vec![PageAction::Scrape],
    )
    .await;

    assert!(
        matches!(&result, Err(CrawlError::SsrfPolicyViolation { .. })),
        "a cloud metadata target must be rejected by SSRF policy before any browser work, got {result:?}"
    );
}
