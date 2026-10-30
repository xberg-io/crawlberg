use super::*;

mod lifecycle_chrome {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/common/lifecycle_chrome.rs"));
}

#[derive(Clone)]
struct Events(Arc<std::sync::Mutex<Vec<(tracing::Level, String)>>>);

struct Message<'a>(&'a mut String);

impl tracing::field::Visit for Message<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write as _;
        write!(self.0, " {}={value:?}", field.name()).expect("event field");
    }
}

impl tracing::Subscriber for Events {
    fn register_callsite(&self, _metadata: &'static tracing::Metadata<'static>) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::sometimes()
    }
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        *metadata.level() <= tracing::Level::INFO
    }
    fn new_span(&self, _attrs: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut message = String::new();
        event.record(&mut Message(&mut message));
        self.0
            .lock()
            .expect("events")
            .push((*event.metadata().level(), message));
    }
    fn enter(&self, _span: &tracing::span::Id) {}
    fn exit(&self, _span: &tracing::span::Id) {}
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

fn config(private_pipe: bool) -> BrowserPoolConfig {
    BrowserPoolConfig {
        private_pipe,
        chrome_args: lifecycle_chrome::args(&["--disable-features=TranslateUI".to_owned()]),
        chrome_path: std::env::var_os("CRAWLBERG_TEST_CHROME_PATH")
            .or_else(|| std::env::var_os("CRAWLBERG_TEST_CHROME"))
            .map(Into::into),
        ..BrowserPoolConfig::default()
    }
}

fn capture(f: impl FnOnce(&Events)) -> Vec<(tracing::Level, String)> {
    let events = Events(Arc::default());
    // ~keep Retain a second dispatcher so concurrent no-subscriber calls cannot cache Never.
    let _other = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let _subscriber = tracing::subscriber::set_default(events.clone());
    f(&events);
    let captured = events.0.lock().expect("events").clone();
    assert!(
        captured
            .iter()
            .any(|(_, message)| message.contains("734 owned browser running")),
        "capture must observe the real browser fixture"
    );
    captured
}

fn assert_clean_phase(pool: &BrowserPool, events: &Events, phase: &str) {
    let captured = events.0.lock().expect("events").clone();
    assert_no_failures(&captured, &format!("{phase}, pipe={}", pool.config.private_pipe));
    assert!(
        pool.is_healthy(),
        "{phase}: owned browser must stay healthy: {captured:?}"
    );
}

async fn warm(pool: &BrowserPool, events: &Events) -> std::path::PathBuf {
    pool.warm().await.expect("real Chrome must launch");
    assert_clean_phase(pool, events, "after pool warm");
    let page = pool.acquire_page().await.expect("real Chrome page");
    assert_clean_phase(pool, events, "after page acquisition");
    assert_eq!(
        page.page()
            .evaluate("21 * 2")
            .await
            .expect("Chrome script")
            .into_value::<i64>()
            .expect("number"),
        42
    );
    assert_clean_phase(pool, events, "after page evaluation");
    page.close().await;
    assert_clean_phase(pool, events, "after page close");
    let profile = pool_profile_dir(pool).await;
    assert!(
        chrome_process_count_for_profile(&profile) > 0,
        "the owned Chrome must be running"
    );
    tracing::info!("734 owned browser running");
    profile
}

#[test]
#[serial_test::serial(owned_browser_teardown)]
fn should_end_owned_browser_drop_and_shutdown_without_failure_logs() {
    for (explicit, private_pipe) in [
        (false, false),
        (true, false),
        #[cfg(unix)]
        (false, true),
        #[cfg(unix)]
        (true, true),
    ] {
        let events = capture(|events| {
            let runtime = runtime();
            let pool = BrowserPool::new(config(private_pipe));
            let profile = runtime.block_on(warm(&pool, events));
            if explicit {
                runtime.block_on(pool.shutdown());
            }
            drop(pool);
            runtime.block_on(async {
                tokio::time::timeout(Duration::from_secs(15), async {
                    while profile.exists() || chrome_process_count_for_profile(&profile) != 0 {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                })
                .await
                .expect("owned Chrome cleanup");
            });
            drop(runtime);
        });
        let failures: Vec<_> = events
            .iter()
            .filter(|(level, _)| *level == tracing::Level::ERROR || *level == tracing::Level::WARN)
            .collect();
        assert_eq!(
            failures.len(),
            0,
            "explicit={explicit}, pipe={private_pipe}: clean owned cleanup must not report failures: {events:?}"
        );
    }
}

fn assert_no_failures(events: &[(tracing::Level, String)], scenario: &str) {
    let failures: Vec<_> = events
        .iter()
        .filter(|(level, _)| *level == tracing::Level::ERROR || *level == tracing::Level::WARN)
        .collect();
    assert_eq!(failures.len(), 0, "{scenario}: {events:?}");
}

#[test]
#[serial_test::serial(owned_browser_teardown)]
fn should_consume_authoritative_profile_retirement_without_failure_logs_or_second_family_kill() {
    for private_pipe in [
        false,
        #[cfg(unix)]
        true,
    ] {
        let events = capture(|events| {
            let runtime = runtime();
            let pool = BrowserPool::new(config(private_pipe));
            let profile = runtime.block_on(warm(&pool, events));
            let before = KILL_BROWSER_CALLS_HERE.with(std::cell::Cell::get);
            runtime.block_on(async {
                let state = pool.state.lock().await.take().expect("owned browser state");
                state.fail_closed().await;
            });
            assert_eq!(
                KILL_BROWSER_CALLS_HERE.with(std::cell::Cell::get) - before,
                0,
                "the early profile teardown must be the single authoritative kill"
            );
            assert_eq!(chrome_process_count_for_profile(&profile), 0);
            assert!(!profile.exists());
            drop(pool);
            runtime.block_on(async { tokio::time::sleep(Duration::from_millis(100)).await });
            drop(runtime);
        });
        assert_no_failures(&events, &format!("authoritative retirement, pipe={private_pipe}"));
    }
}

async fn direct_browser_termination(kill: bool, private_pipe: bool) {
    let profile = tempfile::tempdir().expect("owned profile");
    let browser_config = build_pool_launch_builder(profile.path(), &config(private_pipe))
        .expect("Chrome config")
        .build()
        .expect("Chrome binary must exist");
    let (mut browser, handler) = Browser::launch(browser_config).await.expect("real Chrome must launch");
    let (task, _) = spawn_watched_handler(handler);
    let page = browser.new_page("about:blank").await.expect("real Chrome page");
    assert_eq!(
        page.evaluate("21 * 2")
            .await
            .expect("script")
            .into_value::<i64>()
            .expect("number"),
        42
    );
    tracing::info!("734 owned browser running");
    if kill {
        browser.kill().await.expect("owned child").expect("kill owned Chrome");
    } else {
        browser.close().await.expect("close owned Chrome");
        browser.wait().await.expect("reap owned Chrome");
    }
    tokio::time::timeout(Duration::from_secs(15), task)
        .await
        .expect("owned handler must end")
        .expect("handler task");
    tokio::time::timeout(Duration::from_secs(15), async {
        while chrome_process_count_for_profile(profile.path()) != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("owned Chrome processes must end");
    assert_eq!(chrome_process_count_for_profile(profile.path()), 0);
}

#[test]
#[serial_test::serial(owned_browser_teardown)]
fn should_end_direct_owned_browser_close_and_kill_without_failure_logs() {
    for (kill, private_pipe) in [
        (false, false),
        (true, false),
        #[cfg(unix)]
        (false, true),
        #[cfg(unix)]
        (true, true),
    ] {
        let events = capture(|_| runtime().block_on(direct_browser_termination(kill, private_pipe)));
        assert_no_failures(&events, &format!("direct owned stop, kill={kill}, pipe={private_pipe}"));
    }
}

#[test]
#[serial_test::serial(owned_browser_teardown)]
fn should_report_an_active_owned_browser_transport_failure() {
    for private_pipe in [
        false,
        #[cfg(unix)]
        true,
    ] {
        let events = capture(|events| {
            let runtime = runtime();
            let pool = BrowserPool::new(config(private_pipe));
            let profile = runtime.block_on(warm(&pool, events));
            runtime.block_on(async {
                assert_eq!(kill_pool_chrome(&pool).await, profile);
                tokio::time::timeout(Duration::from_secs(15), async {
                    while pool.is_healthy() || profile.exists() || chrome_process_count_for_profile(&profile) != 0 {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .expect("unexpected loss must fail closed and reap");
            });
            drop(pool);
            drop(runtime);
        });
        assert!(
            events.iter().any(|(level, _)| *level == tracing::Level::ERROR),
            "unexpected active loss remains an error: {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|(_, message)| message.contains("CDP") && message.contains("failed")),
            "capture must see the actual transport failure: {events:?}"
        );
    }
}

#[test]
#[serial_test::serial(owned_browser_teardown)]
fn should_retire_an_owned_browser_with_a_live_page_without_controller_failure_logs() {
    for private_pipe in [
        false,
        #[cfg(unix)]
        true,
    ] {
        let events = capture(|events| {
            let runtime = runtime();
            let pool = BrowserPool::new(config(private_pipe));
            let profile = runtime.block_on(warm(&pool, events));
            runtime.block_on(async {
                let page = pool.acquire_page().await.expect("live owned page");
                assert_eq!(
                    page.page()
                        .evaluate("21 * 2")
                        .await
                        .expect("script")
                        .into_value::<i64>()
                        .expect("number"),
                    42
                );
                let state = pool.state.lock().await.take().expect("owned browser state");
                state.fail_closed().await;
                drop(page);
                tokio::time::timeout(Duration::from_secs(15), async {
                    while profile.exists() || chrome_process_count_for_profile(&profile) != 0 {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                })
                .await
                .expect("owned Chrome cleanup");
            });
            drop(pool);
            drop(runtime);
        });
        assert_no_failures(&events, &format!("live-page owned retirement, pipe={private_pipe}"));
    }
}

#[test]
#[serial_test::serial(owned_browser_teardown)]
fn should_mark_owned_kill_before_family_exit_without_polling_the_later_browser_kill() {
    let events = capture(|_| {
        runtime().block_on(async {
            let profile = tempfile::tempdir().expect("owned profile");
            let path = profile.path().to_path_buf();
            let browser_config = build_pool_launch_builder(&path, &config(false))
                .expect("Chrome config")
                .build()
                .expect("Chrome binary");
            let (browser, handler) = Browser::launch(browser_config).await.expect("real Chrome launch");
            let (handler, _) = spawn_watched_handler(handler);
            let page = browser.new_page("about:blank").await.expect("owned page");
            assert_eq!(
                page.evaluate("21 * 2")
                    .await
                    .expect("script")
                    .into_value::<i64>()
                    .expect("number"),
                42
            );
            drop(page);
            tracing::info!("734 owned browser running");
            let ended = handler.abort_handle();
            let kill = kill_browser(browser, handler, path.clone(), Duration::from_secs(15));
            let mut kill = std::pin::pin!(kill);
            assert!(
                futures::poll!(kill.as_mut()).is_pending(),
                "family walk must yield before later Browser.kill"
            );
            tokio::time::timeout(Duration::from_secs(15), async {
                while !ended.is_finished() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("family exit must end the handler while kill future is held");
            kill.await;
            assert_eq!(chrome_process_count_for_profile(&path), 0);
            assert!(!path.exists());
        })
    });
    assert_no_failures(&events, "early family kill intent");
}
