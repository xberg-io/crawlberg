use super::*;

#[derive(Clone)]
struct Events(Arc<std::sync::Mutex<Vec<(tracing::Level, String)>>>);

struct Message<'a>(&'a mut String);

impl tracing::field::Visit for Message<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            *self.0 = format!("{value:?}");
        }
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
        chrome_path: std::env::var_os("CRAWLBERG_TEST_CHROME").map(Into::into),
        ..BrowserPoolConfig::default()
    }
}

fn capture(f: impl FnOnce()) -> Vec<(tracing::Level, String)> {
    let events = Events(Arc::default());
    // ~keep Retain a second dispatcher so concurrent no-subscriber calls cannot cache Never.
    let _other = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let _subscriber = tracing::subscriber::set_default(events.clone());
    f();
    let captured = events.0.lock().expect("events").clone();
    assert!(
        captured
            .iter()
            .any(|(_, message)| message.contains("734 owned browser running")),
        "capture must observe the real browser fixture"
    );
    captured
}

async fn warm(pool: &BrowserPool) -> std::path::PathBuf {
    pool.warm().await.expect("real Chrome must launch");
    let page = pool.acquire_page().await.expect("real Chrome page");
    assert_eq!(
        page.page()
            .evaluate("21 * 2")
            .await
            .expect("Chrome script")
            .into_value::<i64>()
            .expect("number"),
        42
    );
    page.close().await;
    let profile = pool_profile_dir(pool).await;
    assert!(
        chrome_process_count_for_profile(&profile) > 0,
        "the owned Chrome must be running"
    );
    tracing::info!("734 owned browser running");
    profile
}

#[test]
fn should_end_owned_browser_drop_and_shutdown_without_failure_logs() {
    for (explicit, private_pipe) in [
        (false, false),
        (true, false),
        #[cfg(unix)]
        (false, true),
        #[cfg(unix)]
        (true, true),
    ] {
        let events = capture(|| {
            let runtime = runtime();
            let pool = BrowserPool::new(config(private_pipe));
            let profile = runtime.block_on(warm(&pool));
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
            "explicit={explicit}, pipe={private_pipe}: clean owned cleanup must not report failures: {failures:?}"
        );
    }
}

#[test]
fn should_consume_authoritative_profile_retirement_without_second_family_kill() {
    let private_pipe = false;
    let runtime = runtime();
    let pool = BrowserPool::new(config(private_pipe));
    let profile = runtime.block_on(warm(&pool));
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
}

#[test]
fn should_report_an_active_owned_browser_transport_failure() {
    for private_pipe in [
        false,
        #[cfg(unix)]
        true,
    ] {
        let events = capture(|| {
            let runtime = runtime();
            let pool = BrowserPool::new(config(private_pipe));
            let profile = runtime.block_on(warm(&pool));
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
