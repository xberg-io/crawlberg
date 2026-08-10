// Feature-gated test diagnostics; `allow` (not `expect`) so a clippy run with the
// browser features disabled — where the eprintln is cfg'd out — doesn't warn.
#![allow(clippy::print_stderr)]

#[cfg(feature = "browser-native")]
use std::sync::OnceLock;
#[cfg(any(feature = "browser-chromiumoxide", feature = "browser-native"))]
use std::time::Duration;

use crawlberg::ScrollDirection;
#[cfg(any(feature = "browser-chromiumoxide", feature = "browser-native"))]
use crawlberg::{BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig};
use crawlberg::{CrawlError, PageAction, create_engine, interact, validate_actions};
#[cfg(any(feature = "browser-chromiumoxide", feature = "browser-native"))]
use base64::Engine as _;

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

#[cfg(feature = "browser-native")]
static ALLOW_PRIVATE: OnceLock<()> = OnceLock::new();

#[cfg(feature = "browser-native")]
fn allow_private_network() {
    ALLOW_PRIVATE.get_or_init(|| {
        // ~keep SAFETY: OnceLock writes this env var once before any network call is made.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "1");
        }
    });
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
        ..CrawlConfig::default()
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
        // ~keep Both variants mean the same thing: this runner has no usable Chrome.
        // ubuntu-24.04-arm ships none, and chromiumoxide reports that as a config
        // error at build time rather than a launch failure.
        Err(CrawlError::BrowserError(message))
            if message.contains("failed to launch browser") || message.contains("auto detect a chrome executable") =>
        {
            eprintln!("skipping chromiumoxide interact test because no usable Chrome was found: {message}");
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

#[cfg(feature = "browser-native")]
#[tokio::test]
async fn native_interact_click_type_wait_scroll_execute_js_and_scrape() {
    allow_private_network();
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
        ..CrawlConfig::default()
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
    allow_private_network();
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
        ..CrawlConfig::default()
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
    allow_private_network();
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
        ..CrawlConfig::default()
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
    allow_private_network();
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
        ..CrawlConfig::default()
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
    allow_private_network();
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
        ..CrawlConfig::default()
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
    allow_private_network();
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
        ..CrawlConfig::default()
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
    assert!(matches!(wait, Err(CrawlError::InvalidConfig(message)) if message.contains("wait selector")));

    let scroll = validate_actions(&[PageAction::Scroll {
        direction: ScrollDirection::Down,
        selector: Some(String::new()),
        amount: None,
    }]);
    assert!(matches!(scroll, Err(CrawlError::InvalidConfig(message)) if message.contains("scroll selector")));
}

#[test]
fn validation_rejects_i64_min_scroll_amount() {
    let result = validate_actions(&[PageAction::Scroll {
        direction: ScrollDirection::Down,
        selector: None,
        amount: Some(i64::MIN),
    }]);

    assert!(matches!(result, Err(CrawlError::InvalidConfig(message)) if message.contains("scroll amount")));
}

#[cfg(not(feature = "browser-chromiumoxide"))]
#[tokio::test]
async fn no_chromiumoxide_backend_interact_returns_unsupported() {
    let engine = create_engine(None).unwrap();

    let result = interact(&engine, "https://example.com", vec![PageAction::Scrape]).await;

    match result {
        Err(CrawlError::Unsupported(message)) => {
            assert!(message.contains("browser-chromiumoxide"));
        }
        other => panic!("expected Unsupported, got {other:?}"),
    }
}
