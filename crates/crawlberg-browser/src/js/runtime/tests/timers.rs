use super::setup_runtime;

#[tokio::test(flavor = "current_thread")]
async fn should_wait_for_timer_delay_and_cancel_pending_timers() {
    let mut rt = setup_runtime("<html><body></body></html>");
    let started = std::time::Instant::now();
    let value = rt.evaluate_for_cdp(
        "new Promise(resolve => { const id = setTimeout(() => resolve('cancelled'), 5); clearTimeout(id); setTimeout(resolve, 80, 'done'); })",
        true, true,
    ).await.expect("timer promise");
    assert_eq!(value.value, Some(serde_json::json!("done")));
    assert!(started.elapsed() >= std::time::Duration::from_millis(70));
}

#[test]
fn should_bound_timer_polling_with_a_pending_fetch() {
    const CHILD_FLAG: &str = "CRAWLBERG_TIMER_POLL_CHILD";
    if let Ok(scenario) = std::env::var(CHILD_FLAG) {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(poll_fetch(&scenario));
        return;
    }
    for scenario in ["settles", "never", "wedged_callback", "wedged_event_loop"] {
        let mut child = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "js::runtime::tests::timers::should_bound_timer_polling_with_a_pending_fetch",
                "--nocapture",
            ])
            .env(CHILD_FLAG, scenario)
            .spawn()
            .expect("spawn timer regression");
        let started = std::time::Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().expect("poll child") {
                break status;
            }
            if started.elapsed() > std::time::Duration::from_secs(12) {
                child.kill().expect("stop wedged child");
                child.wait().expect("reap child");
                panic!("timer scenario {scenario} exceeded the external 12s deadline");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        assert!(status.success(), "timer scenario {scenario} failed: {status}");
    }
}

#[derive(Debug)]
struct LoopbackValidator;

#[async_trait::async_trait]
impl crate::net::ssrf::SsrfValidator for LoopbackValidator {
    async fn validate(&self, url: &url::Url) -> Result<(), String> {
        if url.host_str() == Some("127.0.0.1") {
            Ok(())
        } else {
            Err("loopback only".into())
        }
    }
}

async fn poll_fetch(scenario: &str) {
    let (url, requests, server, received) = start_fetch_server(scenario == "settles").await;
    let settles = scenario == "settles";
    let mut rt = setup_runtime("<html><body></body></html>");
    rt.set_url(&url);
    rt.set_ssrf_validator(std::sync::Arc::new(LoopbackValidator));
    rt.execute_script(
        "fetch",
        &format!("globalThis.fr = 'pending'; fetch('{url}').then(() => {{ globalThis.fr = 'done'; }});"),
    )
    .expect("fetch script");
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        tokio::select! {
            accepted = received => accepted.expect("fetch request received"),
            result = rt.run_event_loop() => panic!("fetch settled before the server received it: {result:?}"),
        }
    })
    .await
    .expect("server receives the pending fetch");
    assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 1);
    let expression = if scenario == "wedged_callback" {
        "new Promise(resolve => setTimeout(() => { while (true) {} }, 10))"
    } else {
        "new Promise(resolve => { const poll = () => globalThis.fr !== 'pending' ? resolve(1) : setTimeout(poll, 10); poll(); })"
    };
    let started = std::time::Instant::now();
    let result = if scenario == "wedged_event_loop" {
        rt.execute_script("timer", "setTimeout(() => { while (true) {} }, 10);")
            .expect("schedule timer");
        rt.run_event_loop().await.map(|()| None)
    } else {
        rt.evaluate_for_cdp(expression, true, true).await.map(|info| info.value)
    };
    if settles {
        assert_eq!(result.expect("fetch settles"), Some(serde_json::json!(1.0)));
    } else {
        let error = result.expect_err("evaluation must be bounded");
        assert!(error.contains("5s") || error.contains("terminated"), "{error}");
        assert!(started.elapsed() < std::time::Duration::from_secs(8));
    }
    assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(rt.evaluate("42").expect("runtime recovers"), serde_json::json!(42.0));
    server.abort();
    let _ = server.await;
}

#[tokio::test(flavor = "current_thread")]
async fn should_repeat_interval_tasks_until_cleared() {
    let mut rt = setup_runtime("<html><body></body></html>");
    let value = rt.evaluate_for_cdp(
        "new Promise(resolve => { let count = 0; const id = setInterval(step => { count += step; if (count === 3) { clearInterval(id); resolve(count); } }, 10, 1); })",
        true, true,
    ).await.expect("interval completes");
    assert_eq!(value.value, Some(serde_json::json!(3.0)));
    rt.run_event_loop()
        .await
        .expect("cleared interval leaves no pending timer");
}

async fn start_fetch_server(
    settles: bool,
) -> (
    String,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
    tokio::task::JoinHandle<()>,
    tokio::sync::oneshot::Receiver<()>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let url = format!("http://{}/slow", listener.local_addr().expect("address"));
    let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let recorded = requests.clone();
    let (sent, received) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut request = [0; 4096];
        let read = socket.read(&mut request).await.expect("read request");
        assert!(read > 0, "fetch sent request bytes");
        recorded.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        sent.send(()).expect("request notification");
        if settles {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await
                .expect("response");
        } else {
            std::future::pending::<()>().await;
        }
    });
    (url, requests, server, received)
}

#[tokio::test(flavor = "current_thread")]
async fn should_retain_page_html_when_a_yielding_interval_keeps_running() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let url = format!("http://{}/", listener.local_addr().expect("address"));
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut request = [0; 4096];
        let read = socket.read(&mut request).await.expect("read page request");
        assert!(read > 0);
        let html = "<html><body><h1>Retained page</h1><script>globalThis.ticks = 0; setInterval(() => { globalThis.ticks += 1; }, 10);</script></body></html>";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
            html.len()
        );
        socket.write_all(response.as_bytes()).await.expect("page response");
    });
    let context = crate::context::BrowserContext::with_ssrf(
        "interval-context".into(),
        None,
        false,
        None,
        std::sync::Arc::new(LoopbackValidator),
        false,
    )
    .expect("context");
    let mut page = crate::page::Page::new("interval-page".into(), std::sync::Arc::new(context));
    tokio::time::timeout(std::time::Duration::from_secs(3), page.navigate(&url))
        .await
        .expect("yielding timers preserve the navigation deadline")
        .expect("page navigation");
    assert_eq!(
        page.evaluate_result("document.querySelector('h1').textContent")
            .expect("page heading"),
        serde_json::json!("Retained page")
    );
    assert_eq!(
        page.evaluate_result("globalThis.ticks > 0").expect("interval ran"),
        serde_json::json!(true)
    );
    server.await.expect("server finished");
}
