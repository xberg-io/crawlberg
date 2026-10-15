use super::*;

async fn launch_owned(private_pipe: bool) -> (chromiumoxide::Browser, chromiumoxide::Handler, tempfile::TempDir) {
    let profile = tempfile::tempdir().expect("owned profile");
    let mut builder = chromiumoxide::browser::BrowserConfig::builder()
        .no_sandbox()
        .new_headless_mode()
        .manage_child_targets(false)
        .private_pipe(private_pipe)
        .user_data_dir(profile.path());
    if let Some(chrome) =
        std::env::var_os("CRAWLBERG_TEST_CHROME_PATH").or_else(|| std::env::var_os("CRAWLBERG_TEST_CHROME"))
    {
        builder = builder.chrome_executable(chrome);
    }
    let config = crate::browser_pool::apply_default_args(builder, &[])
        .build()
        .expect("Chrome config");
    let (browser, handler) = chromiumoxide::Browser::launch(config)
        .await
        .expect("real Chrome must launch");
    (browser, handler, profile)
}

async fn enable_real_child<S: super::super::super::child_transport::ChildSocket>(socket: &mut S)
where
    S::Error: std::fmt::Debug,
{
    socket
        .send(async_tungstenite::tungstenite::Message::Text(
            serde_json::json!({
                "id": 1, "method": "Target.setAutoAttach", "params": {
                "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true,
                "filter": [{"type":"browser","exclude":true},{"type":"page","exclude":true},{}]
                }
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("real child enable");
    let receipt = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let reply = socket
                .next()
                .await
                .expect("actual child response")
                .expect("child response");
            if let async_tungstenite::tungstenite::Message::Text(text) = reply {
                let value: serde_json::Value = serde_json::from_str(&text).expect("CDP JSON");
                if value["id"] == 1 {
                    break value;
                }
            }
        }
    })
    .await
    .expect("actual child enable receipt");
    assert_eq!(receipt, serde_json::json!({"id":1,"result":{}}));
}

async fn stop_owner(owner: &mut chromiumoxide::Browser, borrowed: &mut Option<chromiumoxide::Browser>, stop: &str) {
    if borrowed.is_some() {
        assert!(borrowed.as_mut().expect("borrowed browser").kill().await.is_none());
        owner.kill().await.expect("owner child").expect("owner kill");
    } else if stop == "kill" {
        owner.kill().await.expect("owned child").expect("owned kill");
    } else {
        if stop == "close" {
            let close = owner.close();
            let mut close = std::pin::pin!(close);
            assert!(
                futures::poll!(close.as_mut()).is_pending(),
                "unpolled primary handler holds close acknowledgement"
            );
        }
        let pid = owner.process_id().expect("actual owned PID");
        assert!(
            std::process::Command::new("kill")
                .args(["-KILL", &pid.to_string()])
                .status()
                .expect("OS hard kill")
                .success()
        );
    }
}

async fn open_child(browser: &chromiumoxide::Browser, private_pipe: bool) -> super::super::super::ControllerSocket {
    if private_pipe {
        let mut socket = tokio::time::timeout(Duration::from_secs(5), browser.pipe_client().expect("owned pipe"))
            .await
            .expect("pipe client must initialize without primary polling")
            .expect("pipe client");
        enable_real_child(&mut socket).await;
        super::super::super::ControllerSocket::Pipe(socket)
    } else {
        let (mut socket, _) = async_tungstenite::tokio::connect_async(browser.websocket_address().as_str())
            .await
            .expect("actual child connection");
        enable_real_child(&mut socket).await;
        super::super::super::ControllerSocket::WebSocket(socket)
    }
}

async fn child_end_before_primary(stop: &str, borrowed: bool, private_pipe: bool) -> (bool, Vec<serde_json::Value>) {
    captured_child_events(async {
        let (mut owner, owner_handler, profile) = launch_owned(private_pipe).await;
        let endpoint = owner.websocket_address().clone();
        let (mut borrowed_browser, borrowed_handler) = if borrowed {
            let (browser, handler) = chromiumoxide::Browser::connect(endpoint.as_str())
                .await
                .expect("borrowed connection");
            (Some(browser), Some(handler))
        } else {
            (None, None)
        };
        let socket = open_child(&owner, private_pipe).await;
        assert!(!owner.observed_owned_termination());
        stop_owner(&mut owner, &mut borrowed_browser, stop).await;
        assert!(!owner.observed_owned_termination(), "primary handler remains unpolled");
        let browser = borrowed_browser.as_ref().unwrap_or(&owner);
        let page = watched("ROOT");
        let mut shared = shared_with(&page, "OTHER");
        shared.origin = if borrowed {
            BrowserOrigin::External
        } else {
            BrowserOrigin::Launched
        };
        let shared = Arc::new(shared);
        let (_commands, receiver) = mpsc::channel(super::super::super::CHILD_CONTROLLER_PENDING_LIMIT);
        let ended = tokio::time::timeout(
            Duration::from_secs(15),
            super::super::super::serve_child_targets(socket, receiver, &shared, browser),
        )
        .await
        .expect("child ends while primary handler is held");
        assert!(!shared.stopping.load(Ordering::Acquire));
        owner.wait().await.expect("reap stopped Chrome");
        drop(borrowed_handler);
        drop(owner_handler);
        drop(profile);
        ended
    })
    .await
}

#[tokio::test]
async fn should_accept_owned_child_transport_end_before_primary_observes_it() {
    let (ended, events) = child_end_before_primary("kill", false, false).await;
    assert!(ended);
    assert_eq!(events, Vec::<serde_json::Value>::new());
}

#[tokio::test]
async fn should_report_active_child_transport_loss_without_owned_stop_intent() {
    let (ended, events) = child_end_before_primary("active", false, false).await;
    assert!(!ended);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["level"], "ERROR");
    assert_eq!(events[0]["fields"]["phase"], "child_target_controller");
}

#[tokio::test]
async fn should_report_borrowed_child_transport_loss_when_owner_stops_chrome() {
    let (ended, events) = child_end_before_primary("kill", true, false).await;
    assert!(!ended);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["level"], "ERROR");
    assert_eq!(events[0]["fields"]["phase"], "child_target_controller");
}

#[tokio::test]
async fn should_accept_owned_child_transport_end_after_close_requested_before_primary_poll() {
    let (ended, events) = child_end_before_primary("close", false, false).await;
    assert!(ended);
    assert_eq!(events, Vec::<serde_json::Value>::new());
}

#[tokio::test]
async fn should_accept_owned_pipe_child_end_after_kill_before_primary_poll() {
    let (ended, events) = child_end_before_primary("kill", false, true).await;
    assert!(ended);
    assert_eq!(events, Vec::<serde_json::Value>::new());
}

#[tokio::test]
async fn should_accept_owned_pipe_child_end_after_close_requested_before_primary_poll() {
    let (ended, events) = child_end_before_primary("close", false, true).await;
    assert!(ended);
    assert_eq!(events, Vec::<serde_json::Value>::new());
}

#[tokio::test]
async fn should_report_active_pipe_child_transport_loss_without_owned_stop_intent() {
    let (ended, events) = child_end_before_primary("active", false, true).await;
    assert!(!ended);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["level"], "ERROR");
    assert_eq!(events[0]["fields"]["phase"], "child_target_controller");
}

async fn ended_owned_primary() -> (chromiumoxide::Browser, super::super::super::Events, tempfile::TempDir) {
    let (mut browser, handler, profile) = launch_owned(false).await;
    let handler = crate::browser_pool::spawn_handler(handler);
    let events = super::super::super::Events {
        paused: browser
            .event_listener::<EventRequestPaused>()
            .await
            .expect("paused listener"),
        created: browser
            .event_listener::<EventTargetCreated>()
            .await
            .expect("created listener"),
        destroyed: browser
            .event_listener::<super::super::super::EventTargetDestroyed>()
            .await
            .expect("destroyed listener"),
    };
    browser.kill().await.expect("owned child").expect("owned kill");
    tokio::time::timeout(Duration::from_secs(15), handler)
        .await
        .expect("primary must end")
        .expect("primary task");
    assert!(browser.observed_owned_termination());
    (browser, events, profile)
}

#[tokio::test]
async fn should_accept_owned_primary_event_stream_end_without_shared_stopping() {
    let (ended, events) = captured_child_events(async {
        let (browser, listeners, profile) = ended_owned_primary().await;
        let shared = Arc::new(shared_with(&watched("ROOT"), "OTHER"));
        let (_commands, receiver) = mpsc::unbounded_channel();
        let ended = tokio::time::timeout(
            Duration::from_secs(5),
            super::super::super::serve(Arc::new(browser), Arc::clone(&shared), listeners, receiver),
        )
        .await
        .expect("closed event streams must finish");
        assert!(!shared.stopping.load(Ordering::Acquire));
        drop(profile);
        ended
    })
    .await;
    assert!(ended);
    assert_eq!(events, Vec::<serde_json::Value>::new());
}

#[tokio::test]
async fn should_accept_owned_target_census_cancellation_after_primary_transport_end() {
    let (absent, events) = captured_child_events(async {
        let (browser, _listeners, profile) = ended_owned_primary().await;
        let shared = shared_with(&watched("ROOT"), "OTHER");
        let absent = tokio::time::timeout(
            Duration::from_secs(5),
            super::super::super::owned_targets_absent(&browser, &shared),
        )
        .await
        .expect("canceled census must finish");
        assert!(!shared.stopping.load(Ordering::Acquire));
        drop(profile);
        absent
    })
    .await;
    assert!(absent);
    assert_eq!(events, Vec::<serde_json::Value>::new());
}
