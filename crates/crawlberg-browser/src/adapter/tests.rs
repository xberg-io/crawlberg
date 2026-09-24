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
