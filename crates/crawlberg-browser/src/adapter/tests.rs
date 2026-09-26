//! Tests for the native browser adapter's executor and render paths.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

use super::*;

/// Permits the loopback address these tests serve from.
///
/// ~keep This replaces a `std::env::set_var` that a `OnceLock` guarded. The lock made the
/// ~keep write happen once, but nothing stopped the ~130 other tests in this binary from
/// ~keep being inside `std::env::var` at the time -- `DefaultSsrfValidator::from_env` sits
/// ~keep on the construction path of almost all of them. glibc may realloc `environ` under
/// ~keep a concurrent `getenv`, which aborts the process with no Rust panic (issue #48).
/// ~keep Reaching a private address is a policy decision, so inject it, exactly as
/// ~keep `net::client::tests::RecordingValidator` already does.
#[derive(Debug)]
struct AllowLoopbackValidator;

#[async_trait::async_trait]
impl SsrfValidator for AllowLoopbackValidator {
    async fn validate(&self, _url: &Url) -> Result<(), String> {
        Ok(())
    }
}

/// A config whose SSRF policy admits the loopback test server.
fn test_config() -> NativeBrowserConfig {
    NativeBrowserConfig {
        ssrf: Some(Arc::new(AllowLoopbackValidator)),
        ..NativeBrowserConfig::default()
    }
}

fn assert_send<T: Send>(_: T) {}

/// Outer `tokio::time::timeout` margin layered on top of `EXECUTE_JS_TIMEOUT` /
/// `EVAL_SCRIPT_TIMEOUT` in the watchdog-recovery tests below. It only bounds how
/// long the test itself waits for the watchdog to act — the watchdog's actual
/// reclaim deadline is `EXECUTE_JS_TIMEOUT` / `EVAL_SCRIPT_TIMEOUT`, unchanged.
/// ~keep Widened from 15s: CI runners are slower than local dev machines and were
/// ~keep occasionally exceeding a 15s margin even though the watchdog reclaimed the
/// ~keep isolate correctly, producing a false failure rather than a real regression.
const WATCHDOG_RECLAIM_OUTER_SAFETY_MARGIN: Duration = Duration::from_secs(45);

#[test]
fn native_browser_executor_futures_are_send() {
    let executor =
        NativeBrowserExecutor::new(NativeBrowserExecutorConfig::with_workers(1)).expect("executor should start");
    let config = NativeBrowserConfig::default();
    let actions = vec![NativePageAction::Scrape];

    assert_send(executor.render_url("http://example.com", &config));
    assert_send(executor.interact_url("http://example.com", &config, &actions, None));
    assert_send(render_url("http://example.com", &config));
    assert_send(interact_url("http://example.com", &config, &actions, None));
}

#[tokio::test]
async fn native_browser_executor_runs_render_jobs_concurrently() {
    let server = TestServer::start().await;
    let executor = NativeBrowserExecutor::new(NativeBrowserExecutorConfig {
        workers: 4,
        queue_capacity_per_worker: 8,
    })
    .expect("executor should start");

    let mut tasks = Vec::new();
    for index in 0..16 {
        let executor = executor.clone();
        let url = format!("{}/page-{index}", server.base_url);
        tasks.push(tokio::spawn(
            async move { executor.render_url(&url, &test_config()).await },
        ));
    }

    let results = futures::future::join_all(tasks).await;
    for result in results {
        let rendered = result.expect("task should join").expect("render should succeed");
        assert!(rendered.html.contains("Native executor"));
    }
    assert!(
        server.max_in_flight.load(Ordering::SeqCst) >= 2,
        "server should observe parallel native requests"
    );
}

#[tokio::test]
async fn native_browser_executor_runs_interact_jobs_concurrently() {
    let server = TestServer::start().await;
    let executor = NativeBrowserExecutor::new(NativeBrowserExecutorConfig {
        workers: 4,
        queue_capacity_per_worker: 8,
    })
    .expect("executor should start");

    let actions = vec![
        NativePageAction::Click {
            selector: "#go".to_owned(),
        },
        NativePageAction::Scrape,
    ];
    let mut tasks = Vec::new();
    for index in 0..12 {
        let executor = executor.clone();
        let actions = actions.clone();
        let url = format!("{}/action-{index}", server.base_url);
        tasks.push(tokio::spawn(async move {
            executor.interact_url(&url, &test_config(), &actions, None).await
        }));
    }

    let results = futures::future::join_all(tasks).await;
    for result in results {
        let interaction = result.expect("task should join").expect("interact should succeed");
        assert!(interaction.action_results.iter().all(|action| action.success));
        assert!(interaction.final_html.contains("clicked"));
    }
    assert!(
        server.max_in_flight.load(Ordering::SeqCst) >= 2,
        "server should observe parallel native interaction requests"
    );
}

#[tokio::test]
async fn native_browser_executor_drops_after_work() {
    let server = TestServer::start().await;
    let executor =
        NativeBrowserExecutor::new(NativeBrowserExecutorConfig::with_workers(2)).expect("executor should start");

    let rendered = executor
        .render_url(&server.base_url, &test_config())
        .await
        .expect("render should succeed");
    assert!(rendered.html.contains("Native executor"));
    drop(executor);
}

/// Proves #60 is fixed: the inverse of
/// `native::hung_execute_js_permanently_pins_the_native_worker_thread` in
/// `crawlberg/src/interact/native.rs`, which showed a hung `ExecuteJs` action wedges
/// the sole worker OS thread forever, so a trivial follow-up job on the same
/// single-worker executor never completes.
///
/// Here the same scenario must now resolve: the watchdog spawned by
/// `BrowserJsRuntime::evaluate_with_timeout` calls `v8::IsolateHandle::terminate_execution`
/// past `EXECUTE_JS_TIMEOUT`, unblocking the worker thread's `execute_script` call. It
/// also checks that `cancel_terminate_execution` actually leaves the isolate usable —
/// not just for a fresh job, but for the *next action in the same job*, which reuses the
/// very `BrowserJsRuntime` that was just terminated.
#[tokio::test]
async fn hung_execute_js_terminates_and_the_native_worker_recovers_for_later_actions() {
    let server = TestServer::start().await;
    let executor =
        NativeBrowserExecutor::new(NativeBrowserExecutorConfig::with_workers(1)).expect("executor should start");
    let config = test_config();

    let same_job_actions = vec![
        NativePageAction::ExecuteJs {
            script: "while (true) {}".to_owned(),
        },
        NativePageAction::ExecuteJs {
            script: "21 + 21".to_owned(),
        },
    ];
    let same_job_outcome = tokio::time::timeout(
        EXECUTE_JS_TIMEOUT + WATCHDOG_RECLAIM_OUTER_SAFETY_MARGIN,
        executor.interact_url(&server.base_url, &config, &same_job_actions, None),
    )
    .await
    .expect("the watchdog must reclaim the isolate well before this outer safety margin")
    .expect("interact_url should return a result, not a transport error");

    assert_eq!(same_job_outcome.action_results.len(), 2);
    let hung = &same_job_outcome.action_results[0];
    assert!(
        !hung.success,
        "a terminated script must surface as a failed action, not a silent success"
    );
    assert!(
        hung.error.as_deref().is_some_and(|e| e.contains("terminated")),
        "a terminated script must produce a clear termination error, got {:?}",
        hung.error
    );
    let recovered = &same_job_outcome.action_results[1];
    assert!(
        recovered.success,
        "the isolate must remain usable for later actions in the same job after a termination, got {:?}",
        recovered.error
    );
    assert_eq!(recovered.data, Some(serde_json::json!(42.0)));

    let followup_outcome = tokio::time::timeout(
        Duration::from_secs(15),
        executor.interact_url(&server.base_url, &config, &[NativePageAction::Scrape], None),
    )
    .await
    .expect(
        "a trivial follow-up job on the same single-worker executor must complete now that the worker thread is free",
    )
    .expect("follow-up interact_url should succeed");

    assert!(
        followup_outcome.action_results[0].success,
        "follow-up Scrape action should succeed"
    );
    assert!(followup_outcome.final_html.contains("Native executor"));
}

/// Proves #71 is fixed for the post-navigation `eval_script` path in `interact_url_local`,
/// which previously called `Page::evaluate_result` with no bound. A non-terminating
/// `eval_script` must be reclaimed by the same watchdog proven for `ExecuteJs`, must
/// surface as a clear termination error rather than hanging the whole job, and must leave
/// the worker OS thread free for the next job on this single-worker executor.
#[tokio::test]
async fn hung_post_navigation_eval_script_terminates_and_the_native_worker_recovers() {
    let server = TestServer::start().await;
    let executor =
        NativeBrowserExecutor::new(NativeBrowserExecutorConfig::with_workers(1)).expect("executor should start");
    let config = NativeBrowserConfig {
        eval_script: Some("while (true) {}".to_owned()),
        ..test_config()
    };

    let outcome = tokio::time::timeout(
        EVAL_SCRIPT_TIMEOUT + Duration::from_secs(15),
        executor.interact_url(&server.base_url, &config, &[NativePageAction::Scrape], None),
    )
    .await
    .expect("the watchdog must reclaim the isolate well before this outer safety margin");

    let error = outcome.expect_err("a hung eval_script must surface as an error, not hang the job");
    let message = error.to_string();
    assert!(
        message.contains("terminated"),
        "a terminated eval_script must produce a clear termination error, got {message:?}"
    );

    let followup_outcome = tokio::time::timeout(
        Duration::from_secs(15),
        executor.interact_url(&server.base_url, &test_config(), &[NativePageAction::Scrape], None),
    )
    .await
    .expect(
        "a trivial follow-up job on the same single-worker executor must complete now that the worker thread is free",
    )
    .expect("follow-up interact_url should succeed");

    assert!(
        followup_outcome.action_results[0].success,
        "follow-up Scrape action should succeed"
    );
    assert!(followup_outcome.final_html.contains("Native executor"));
}

/// Proves #71 is fixed for the render-path `eval_script` in `render_with_context`, which
/// previously called `Page::evaluate` with no bound. Render-path `eval_script` errors are
/// swallowed to `None` (mirroring `Page::evaluate`'s existing null-on-error contract), so a
/// terminated script must not hang the render but must still let the render itself
/// succeed, and must leave the worker OS thread free for the next job.
#[tokio::test]
async fn hung_render_path_eval_script_is_terminated_and_the_native_worker_recovers() {
    let server = TestServer::start().await;
    let executor =
        NativeBrowserExecutor::new(NativeBrowserExecutorConfig::with_workers(1)).expect("executor should start");
    let config = NativeBrowserConfig {
        eval_script: Some("while (true) {}".to_owned()),
        ..test_config()
    };

    let rendered = tokio::time::timeout(
        EVAL_SCRIPT_TIMEOUT + Duration::from_secs(15),
        executor.render_url(&server.base_url, &config),
    )
    .await
    .expect("the watchdog must reclaim the isolate well before this outer safety margin")
    .expect("render should still succeed even though eval_script hung and was terminated");

    assert!(
        rendered.eval_result.is_none(),
        "a terminated eval_script must not surface a spurious result, got {:?}",
        rendered.eval_result
    );
    assert!(rendered.html.contains("Native executor"));

    let followup = tokio::time::timeout(
        Duration::from_secs(15),
        executor.render_url(&server.base_url, &test_config()),
    )
    .await
    .expect("a trivial follow-up render on the same single-worker executor must complete now that the worker thread is free")
    .expect("follow-up render_url should succeed");

    assert!(followup.html.contains("Native executor"));
}

#[tokio::test]
async fn screenshot_content_height_uses_the_dom_scroll_height_when_larger_than_static_hints() {
    let server = TestServer::start().await;
    let context = create_context(&test_config()).await;
    let mut page = Page::new("page-1".to_string(), context);
    navigate_configured(&mut page, &server.base_url, &test_config())
        .await
        .expect("navigation should succeed");
    let html = rendered_html(&page).expect("page should have rendered DOM");

    let height = screenshot_content_height(&mut page, &html);

    assert!(
        height >= SCREENSHOT_VIEWPORT_HEIGHT,
        "content height must never fall below the viewport height, got {height}"
    );
    assert!(
        height <= MAX_NATIVE_SCREENSHOT_HEIGHT,
        "content height must never exceed the native screenshot ceiling, got {height}"
    );
}

#[tokio::test]
async fn render_with_context_evaluates_script_and_captures_network_events_when_configured() {
    let server = TestServer::start().await;
    let config = NativeBrowserConfig {
        eval_script: Some("21 + 21".to_owned()),
        capture_network_events: true,
        ..test_config()
    };

    let rendered = render_url(&server.base_url, &config)
        .await
        .expect("render should succeed");

    assert_eq!(rendered.eval_result, Some(serde_json::json!(42.0)));
    assert!(
        !rendered.network_events.is_empty(),
        "capture_network_events=true should populate network_events"
    );
    let document_event = rendered
        .network_events
        .iter()
        .find(|event| event.resource_type == "Document")
        .expect("a Document network event should be captured for the navigation");
    assert_eq!(document_event.status, 200);
    assert_eq!(document_event.method, "GET");
}

#[tokio::test]
async fn render_with_context_leaves_network_events_empty_when_capture_disabled() {
    let server = TestServer::start().await;
    let config = NativeBrowserConfig {
        capture_network_events: false,
        ..test_config()
    };

    let rendered = render_url(&server.base_url, &config)
        .await
        .expect("render should succeed");

    assert!(
        rendered.network_events.is_empty(),
        "capture_network_events=false must not populate network_events"
    );
}

struct TestServer {
    base_url: String,
    max_in_flight: Arc<AtomicUsize>,
}

impl TestServer {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("test server should bind");
        let addr = listener.local_addr().expect("test server should have local addr");
        let current = Arc::new(AtomicUsize::new(0));
        let max_in_flight = Arc::new(AtomicUsize::new(0));
        let current_for_task = current.clone();
        let max_for_task = max_in_flight.clone();

        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let current = current_for_task.clone();
                let max_in_flight = max_for_task.clone();
                tokio::spawn(async move {
                    let active = current.fetch_add(1, Ordering::SeqCst) + 1;
                    max_in_flight.fetch_max(active, Ordering::SeqCst);

                    let mut buffer = [0_u8; 1024];
                    let _ = stream.read(&mut buffer).await;
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    let body = r#"
                        <html>
                          <body>
                            <button id="go">Go</button>
                            <div id="status">Native executor</div>
                            <script>
                              document.getElementById('go').addEventListener('click', () => {
                                document.getElementById('status').textContent = 'clicked';
                              });
                            </script>
                          </body>
                        </html>
                    "#;
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.shutdown().await;
                    current.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });

        Self {
            base_url: format!("http://{addr}"),
            max_in_flight,
        }
    }
}

#[test]
fn native_browser_config_debug_hides_headers_proxy_and_cookie_values() {
    const SECRET: &str = "sk-live-9f8e7d6c5b4a";
    let config = NativeBrowserConfig {
        extra_headers: HashMap::from([("Authorization".to_owned(), format!("Bearer {SECRET}"))]),
        proxy_url: Some(format!("http://user:{SECRET}@proxy.internal:8080")),
        prior_cookies: vec![NativeCookie {
            name: "session".into(),
            value: SECRET.into(),
            domain: None,
            path: None,
            secure: true,
            http_only: true,
        }],
        ..NativeBrowserConfig::default()
    };
    for rendered in [format!("{config:?}"), format!("{config:#?}")] {
        assert!(!rendered.contains(SECRET), "secret printed: {rendered}");
        assert!(rendered.contains("Authorization"), "header name missing: {rendered}");
        assert!(rendered.contains("session"), "cookie name missing: {rendered}");
    }
}

/// A secret every `Debug` below must hide.
const HEADER_TEST_SECRET: &str = "sk-live-9f8e7d6c5b4a";

/// Request headers as a caller's configuration supplies them. `X-Api-Key` is the case a
/// name denylist misses: a credential under a name nobody can enumerate in advance.
fn request_headers_with_secrets() -> HashMap<String, String> {
    HashMap::from([
        ("Authorization".to_owned(), format!("Bearer {HEADER_TEST_SECRET}")),
        ("cookie".to_owned(), format!("sid={HEADER_TEST_SECRET}")),
        ("Proxy-Authorization".to_owned(), format!("Basic {HEADER_TEST_SECRET}")),
        ("X-Api-Key".to_owned(), HEADER_TEST_SECRET.to_owned()),
        ("accept".to_owned(), "text/html".to_owned()),
    ])
}

/// Response headers as a server returns them: one credential, one plain diagnostic value.
fn response_headers_with_secrets() -> HashMap<String, String> {
    HashMap::from([
        ("set-cookie".to_owned(), format!("sid={HEADER_TEST_SECRET}; HttpOnly")),
        ("content-type".to_owned(), "text/html".to_owned()),
    ])
}

/// Every type that renders a header map, as `(what, rendered, carries_a_response_map)`.
fn header_bearing_debug_renderings() -> Vec<(&'static str, String, bool)> {
    let request_headers = request_headers_with_secrets();
    let response_headers = response_headers_with_secrets();
    let url = Url::parse("https://example.com/").expect("url");
    let native_event = NativeNetworkEvent {
        url: url.to_string(),
        method: "GET".into(),
        resource_type: "document".into(),
        status: 200,
        request_headers: request_headers.clone(),
        response_headers: response_headers.clone(),
        body_size: 0,
        timestamp_ms: 0,
    };
    let page_event = crate::page::NetworkEvent {
        request_id: "1".into(),
        url: url.to_string(),
        method: "GET".into(),
        resource_type: "document".into(),
        status: 200,
        headers: request_headers.clone(),
        response_headers: Arc::new(response_headers.clone()),
        body_size: 0,
        timestamp: 0.0,
    };
    let rendered_page = RenderedPage {
        final_url: url.to_string(),
        status: Some(200),
        html: String::new(),
        headers: response_headers.clone(),
        eval_result: None,
        network_events: vec![native_event.clone()],
        cookies: Vec::new(),
    };
    let response = crate::net::client::Response {
        url: url.clone(),
        status: 200,
        headers: response_headers.clone(),
        body: Vec::new(),
        redirected_from: Vec::new(),
    };
    let request_info = crate::net::client::RequestInfo {
        url: url.clone(),
        method: "GET".into(),
        headers: request_headers.clone(),
        resource_type: crate::net::client::ResourceType::Document,
    };
    let continue_resolution = crate::js::ops::InterceptResolution::Continue {
        url: None,
        method: None,
        headers: Some(request_headers),
        body: None,
    };
    let fulfill_resolution = crate::js::ops::InterceptResolution::Fulfill {
        status: 200,
        headers: response_headers,
        body: String::new(),
    };
    vec![
        ("NativeNetworkEvent", format!("{native_event:?}"), true),
        ("NetworkEvent", format!("{page_event:#?}"), true),
        ("RenderedPage", format!("{rendered_page:?}"), true),
        ("Response", format!("{response:?}"), true),
        ("RequestInfo", format!("{request_info:?}"), false),
        (
            "InterceptResolution::Continue",
            format!("{continue_resolution:?}"),
            false,
        ),
        ("InterceptResolution::Fulfill", format!("{fulfill_resolution:?}"), true),
    ]
}

/// No type that renders a header map may print a credential, and all of them keep the names.
#[test]
fn header_maps_debug_hides_every_credential_and_keeps_names() {
    let renderings = header_bearing_debug_renderings();
    assert_eq!(renderings.len(), 7, "every header-bearing type must be covered");
    for (what, rendered, _) in &renderings {
        assert!(
            !rendered.contains(HEADER_TEST_SECRET),
            "{what} printed a secret: {rendered}"
        );
        assert!(rendered.contains("***"), "{what} printed no placeholder: {rendered}");
        assert!(
            rendered.contains("accept") || rendered.contains("content-type"),
            "{what} dropped the header names: {rendered}"
        );
    }
}

/// A request header map prints no value at all, because a credential can sit under any name;
/// a response header map keeps its non-credential values, which are the debugging value.
#[test]
fn only_a_response_header_map_keeps_a_value() {
    for (what, rendered, carries_a_response_map) in header_bearing_debug_renderings() {
        assert_eq!(
            rendered.contains("text/html"),
            carries_a_response_map,
            "{what}: a response header value must print and a request one must not: {rendered}"
        );
    }
}
