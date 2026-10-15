use super::*;

async fn captured_child_events<F: std::future::Future>(future: F) -> (F::Output, Vec<serde_json::Value>) {
    use tracing::instrument::WithSubscriber as _;

    let captured = tempfile::NamedTempFile::new().expect("captured controller events");
    let writer = captured.reopen().expect("capture writer");
    let subscriber = tracing_subscriber::fmt()
        .json()
        .without_time()
        .with_max_level(tracing::Level::WARN)
        .with_writer(move || writer.try_clone().expect("capture writer clone"))
        .finish();
    let result = future.with_subscriber(subscriber).await;
    let text = std::fs::read_to_string(captured.path()).expect("captured events");
    let events = text
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("event JSON"))
        .collect();
    assert!(
        !text.contains("SENSITIVE-PAYLOAD"),
        "diagnostics must not expose target or context payloads"
    );
    (result, events)
}

#[tokio::test]
async fn a_closed_child_socket_reports_one_failure_phase_without_payloads() {
    let (ended, events) = captured_child_events(async {
        let page = watched("ROOT-SENSITIVE-PAYLOAD");
        let shared = Arc::new(shared_with(&page, "OTHER-SENSITIVE-PAYLOAD"));
        let (mut socket, _commands, controller) = test_child_controller(shared, Duration::from_secs(1)).await;
        socket.close(None).await.expect("close the controlled peer");
        controller.await.expect("controller task")
    })
    .await;
    assert!(!ended, "the lost connection must still fail closed");
    assert_eq!(events.len(), 1, "one first controller failure must be reported");
    assert_eq!(events[0]["level"], "ERROR");
    assert_eq!(events[0]["target"], "crawlberg::ssrf_intercept");
    assert_eq!(
        events[0]["fields"],
        serde_json::json!({
            "message": "browser child-target controller failed closed",
            "phase": "child_target_controller",
            "reason": "socket_close_frame",
            "browser_handler_task_id": "None",
        })
    );
}

#[tokio::test]
async fn a_missing_child_acknowledgement_reports_nothing_while_it_waits() {
    let (ended, events) = captured_child_events(async {
        let page = watched("ROOT");
        let shared = Arc::new(shared_with(&page, "OTHER"));
        let context = register_owned_context(&shared, &page);
        let (mut socket, commands, controller) = test_child_controller(shared, Duration::from_millis(250)).await;
        let (done, armed) = oneshot::channel();
        commands
            .send(super::super::ChildCommand::Arm {
                root: page.root.clone(),
                done,
            })
            .await
            .expect("arm root");
        let root_arm = begin_root_arm(&mut socket, &context).await;
        finish_root_arm(&mut socket, &root_arm, armed).await;
        send_attached_target(&mut socket, "S-CHILD", "CHILD", Some("ROOT"), &context).await;
        let recursive_arm = next_child_message(&mut socket).await;
        answer_child_command(&mut socket, &recursive_arm).await;
        let resume = next_child_message(&mut socket).await;
        assert_eq!(resume["method"], "Runtime.runIfWaitingForDebugger");
        let_time_pass(Duration::from_secs(5)).await;
        assert!(
            !controller.is_finished(),
            "an unanswered resume is waited for, not timed"
        );
        commands.send(super::super::ChildCommand::Stop).await.expect("stop");
        controller.await.expect("controller task")
    })
    .await;
    assert!(ended, "a stop ends the wait with a healthy controller");
    assert_eq!(events, Vec::<serde_json::Value>::new(), "waiting is not a failure");
}

#[tokio::test]
async fn a_child_socket_ending_during_requested_stop_reports_no_failure() {
    let (ended, events) = captured_child_events(async {
        let page = watched("ROOT");
        let shared = Arc::new(shared_with(&page, "OTHER"));
        let (mut socket, _commands, controller) =
            test_child_controller(Arc::clone(&shared), Duration::from_secs(1)).await;
        shared.stopping.store(true, Ordering::Release);
        socket.close(None).await.expect("close the stopping peer");
        controller.await.expect("controller task")
    })
    .await;
    assert!(
        !ended,
        "stopping must not change the disconnected controller's return value"
    );
    assert_eq!(
        events,
        Vec::<serde_json::Value>::new(),
        "a requested stop is not unexpected failure"
    );
}

#[tokio::test]
async fn an_unproved_running_child_ends_the_controller_before_further_traffic() {
    use async_tungstenite::tungstenite::{Error, Message, error::ProtocolError};

    let (ended, events) = captured_child_events(async {
        let page = watched("ROOT");
        let shared = Arc::new(shared_with(&page, "OTHER"));
        let owned_context = register_owned_context(&shared, &page);
        let unregistered_context = BrowserContextId::new("UNREGISTERED-CONTEXT");
        assert_ne!(owned_context, unregistered_context);
        let (mut socket, _commands, controller) =
            test_child_controller(Arc::clone(&shared), Duration::from_secs(2)).await;
        send_attached_target_with_waiting(
            &mut socket,
            "S-UNPROVED",
            "UNPROVED",
            "worker",
            Some("ROOT"),
            &unregistered_context,
            false,
        )
        .await;
        let probe_sent = socket.send(Message::Ping(b"after-unproved-child".to_vec().into())).await;
        if probe_sent.is_ok() {
            let response = tokio::time::timeout(Duration::from_secs(2), socket.next())
                .await
                .expect("the controller must terminate the controlled socket");
            assert!(
                matches!(
                    response,
                    None | Some(Ok(Message::Close(_)))
                        | Some(Err(Error::Protocol(ProtocolError::ResetWithoutClosingHandshake)))
                ),
                "an unproved running child must end the controller without resuming, closing or processing further traffic: {response:?}"
            );
        }
        let ended = tokio::time::timeout(Duration::from_secs(2), controller)
            .await
            .expect("unproved boundary termination")
            .expect("controller task");
        assert!(matches!(
            lock(&shared.registry).ownership.get(&TargetId::new("UNPROVED")),
            Some(TargetOwnership::Quarantined)
        ));
        ended
    })
    .await;
    assert!(!ended, "an unproved running child must fail closed");
    assert_eq!(events.len(), 1, "one first controller failure must be reported");
    assert_eq!(events[0]["fields"]["phase"], "child_target_controller");
    assert_eq!(events[0]["fields"]["reason"], "unproved_attached_boundary");
}

async fn attach_structural_page(socket: &mut TestChildSocket, context: &BrowserContextId, child_ack: bool) {
    send_attached_target_from_parent(
        socket,
        "S-PAGE-SENSITIVE-PAYLOAD",
        "PAGE-SENSITIVE-PAYLOAD",
        "page",
        None,
        context,
        false,
        Some("S-TAB-SENSITIVE-PAYLOAD"),
    )
    .await;
    let page_arm = next_child_message(socket).await;
    if !child_ack {
        return;
    }
    answer_child_command(socket, &page_arm).await;
    let page_resume = next_child_message(socket).await;
    assert_eq!(page_resume["method"], "Runtime.runIfWaitingForDebugger");
    answer_child_command(socket, &page_resume).await;
}

/// A tab whose chain is incomplete in the way the flags say: the tab is not resumed, the
/// controller reports nothing and keeps running, and a stop ends it healthy.
async fn incomplete_structural_chain_is_held(tab_ack: bool, child_present: bool, child_ack: bool, waiting: bool) {
    let (ended, events) = captured_child_events(async {
        let page = watched("ROOT-SENSITIVE-PAYLOAD");
        let shared = Arc::new(shared_with(&page, "OTHER-SENSITIVE-PAYLOAD"));
        let context = register_owned_context(&shared, &page);
        let (mut socket, commands, controller) = test_child_controller(shared, Duration::from_millis(250)).await;
        send_attached_target_with_waiting(
            &mut socket,
            "S-TAB-SENSITIVE-PAYLOAD",
            "TAB-SENSITIVE-PAYLOAD",
            "tab",
            None,
            &context,
            waiting,
        )
        .await;
        let tab_arm = next_child_message(&mut socket).await;
        if tab_ack {
            answer_child_command(&mut socket, &tab_arm).await;
        }
        if child_present {
            attach_structural_page(&mut socket, &context, child_ack).await;
        }
        let_time_pass(Duration::from_secs(5)).await;
        assert!(
            !controller.is_finished(),
            "an incomplete chain is waited for, not timed"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(100), socket.next())
                .await
                .is_err(),
            "a tab with an incomplete chain must not be resumed"
        );
        commands.send(super::super::ChildCommand::Stop).await.expect("stop");
        controller.await.expect("controller task")
    })
    .await;
    assert!(ended, "a stop ends the wait with a healthy controller");
    assert_eq!(events, Vec::<serde_json::Value>::new(), "waiting is not a failure");
}

#[tokio::test]
async fn an_incomplete_structural_chain_is_held_with_no_event() {
    for (tab_ack, child_present, child_ack, waiting) in [
        (false, false, false, true),
        (true, false, false, false),
        (false, true, true, true),
        (true, true, false, true),
    ] {
        incomplete_structural_chain_is_held(tab_ack, child_present, child_ack, waiting).await;
    }
}

#[tokio::test]
async fn the_child_failure_identifies_its_primary_handler_task() {
    let parent = tokio::spawn(std::future::pending::<()>());
    let expected = format!("{:?}", Some(parent.id()));
    let (ended, events) = captured_child_events(async {
        let page = watched("ROOT");
        let mut shared = shared_with(&page, "OTHER");
        shared.handler_abort = Some(parent.abort_handle());
        let (mut socket, _commands, controller) = test_child_controller(Arc::new(shared), Duration::from_secs(1)).await;
        socket.close(None).await.expect("close controlled peer");
        controller.await.expect("controller task")
    })
    .await;
    parent.abort();
    assert!(!ended);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["fields"]["browser_handler_task_id"], expected);
}

async fn primary_connection_failure_events() -> Option<(String, Vec<serde_json::Value>)> {
    use chromiumoxide::browser::{Browser, BrowserConfig};

    let directory = tempfile::tempdir().expect("owned correlation probe profile");
    let mut builder = BrowserConfig::builder()
        .no_sandbox()
        .new_headless_mode()
        .manage_child_targets(false)
        .user_data_dir(directory.path());
    if let Some(chrome) = crate::browser_pool::chrome_executable(None) {
        builder = builder.chrome_executable(chrome);
    }
    let config = crate::browser_pool::tests::expect_chrome_or_skip("handler correlation", builder.build())?;
    let (mut owner, _owner_handler) = Browser::launch(config).await.expect("launch probe Chrome");
    let (expected, events) = captured_child_events(supervised_handler_failure(&mut owner)).await;
    Some((expected, events))
}

async fn supervised_handler_failure(owner: &mut chromiumoxide::browser::Browser) -> String {
    let (browser, handler) = crate::browser_pool::connect_endpoint(owner.websocket_address())
        .await
        .expect("connect primary probe client");
    let parent = crate::browser_pool::spawn_handler(handler);
    let expected = format!("{:?}", Some(parent.id()));
    let firewall = super::super::BrowserFirewall::start_supervised(
        Arc::new(browser),
        super::super::BrowserOrigin::External,
        super::super::PageContext::Copied,
        parent.abort_handle(),
    )
    .await
    .expect("start supervised probe firewall");
    let shared = Arc::clone(&firewall.handle().shared);
    assert_eq!(
        shared.handler_abort.as_ref().map(tokio::task::AbortHandle::id),
        Some(parent.id())
    );
    let (mut socket, _commands, controller) = test_child_controller(shared, Duration::from_secs(1)).await;
    socket.close(None).await.expect("close controlled child peer");
    assert!(!controller.await.expect("controlled child task"));
    firewall.stop().await;
    assert!(
        matches!(owner.kill().await, Some(Ok(()))),
        "kill only the owned probe Chrome"
    );
    tokio::time::timeout(Duration::from_secs(5), parent)
        .await
        .expect("primary transport termination")
        .expect("primary handler task");
    expected
}

#[tokio::test]
async fn a_primary_transport_failure_and_child_failure_share_the_supervised_handler_identity() {
    let Some((expected, events)) = primary_connection_failure_events().await else {
        return;
    };
    let child = events
        .iter()
        .find(|event| event["fields"]["reason"] == "socket_close_frame")
        .expect("controlled child failure event");
    assert_eq!(child["fields"]["browser_handler_task_id"], expected);
    let parent = events
        .iter()
        .find(|event| event["fields"]["message"] == "the browser's CDP websocket failed; its CDP handler ends")
        .expect("primary transport failure event");
    assert_eq!(parent["fields"]["browser_handler_task_id"], expected);
    assert_eq!(parent["target"], "crawlberg::browser_pool");
}

#[tokio::test]
async fn a_requested_stop_ends_an_incomplete_structural_chain() {
    let (ended, events) = captured_child_events(async {
        let page = watched("ROOT-SENSITIVE-PAYLOAD");
        let shared = Arc::new(shared_with(&page, "OTHER-SENSITIVE-PAYLOAD"));
        let context = register_owned_context(&shared, &page);
        let (mut socket, commands, controller) =
            test_child_controller(Arc::clone(&shared), Duration::from_millis(250)).await;
        shared.stopping.store(true, Ordering::Release);
        send_attached_target_with_waiting(
            &mut socket,
            "S-TAB-SENSITIVE-PAYLOAD",
            "TAB-SENSITIVE-PAYLOAD",
            "tab",
            None,
            &context,
            true,
        )
        .await;
        let tab_arm = next_child_message(&mut socket).await;
        answer_child_command(&mut socket, &tab_arm).await;
        commands.send(super::super::ChildCommand::Stop).await.expect("stop");
        tokio::time::timeout(Duration::from_secs(2), controller)
            .await
            .expect("the stop must end the wait")
            .expect("controller task")
    })
    .await;
    assert!(ended, "a stop ends an incomplete chain with a healthy controller");
    assert_eq!(
        events,
        Vec::<serde_json::Value>::new(),
        "a requested stop is not a failure"
    );
}

mod teardown_controls;

#[cfg(unix)]
mod owned_teardown;

async fn reject_child_acknowledgement(close: bool) -> (bool, Vec<serde_json::Value>) {
    captured_child_events(async {
        let page = watched("ROOT-SENSITIVE-PAYLOAD");
        let shared = Arc::new(shared_with(&page, "OTHER-SENSITIVE-PAYLOAD"));
        let context = register_owned_context(&shared, &page);
        if close {
            begin_ending(&page);
        }
        let (mut socket, _commands, controller) = test_child_controller(shared, Duration::from_secs(2)).await;
        send_attached_target_with_waiting(&mut socket, "S-TAB", "TAB", "tab", None, &context, true).await;
        let tab_arm = next_child_message(&mut socket).await;
        send_attached_target_from_parent(
            &mut socket,
            "S-CHILD-SENSITIVE-PAYLOAD",
            "CHILD-SENSITIVE-PAYLOAD",
            if close { "page" } else { "worker" },
            Some("ROOT-SENSITIVE-PAYLOAD"),
            &context,
            false,
            Some("S-TAB"),
        )
        .await;
        let command = if close {
            next_child_message(&mut socket).await
        } else {
            let recursive = next_child_message(&mut socket).await;
            answer_child_command(&mut socket, &recursive).await;
            next_child_message(&mut socket).await
        };
        assert_eq!(
            command["method"],
            if close {
                "Target.closeTarget"
            } else {
                "Runtime.runIfWaitingForDebugger"
            }
        );
        answer_child_command(&mut socket, &tab_arm).await;
        reject_child_response(&mut socket, &command, controller).await
    })
    .await
}

#[tokio::test]
async fn a_rejected_child_acknowledgement_reports_only_kind_and_code() {
    for close in [false, true] {
        let (ended, events) = reject_child_acknowledgement(close).await;
        assert!(!ended, "rejected acknowledgements must still fail closed");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["level"], "ERROR");
        assert_eq!(
            events[0]["fields"],
            serde_json::json!({
                "message": "browser child-target controller failed closed",
                "phase": "child_target_controller",
                "reason": "boundary_or_close_ack",
                "browser_handler_task_id": "None",
                "pending_command": if close { "close" } else { "boundary" },
                "cdp_error_code": -32001,
            })
        );
    }
}

async fn reject_child_response(
    socket: &mut TestChildSocket,
    command: &serde_json::Value,
    controller: tokio::task::JoinHandle<bool>,
) -> bool {
    socket
        .send(async_tungstenite::tungstenite::Message::Text(
            serde_json::json!({"id": command["id"], "error": {
                "code": -32001, "message": "SENSITIVE-PAYLOAD session gone"
            }})
            .to_string()
            .into(),
        ))
        .await
        .expect("send rejected acknowledgement");
    tokio::time::timeout(Duration::from_secs(2), controller)
        .await
        .expect("rejected acknowledgement must end controller")
        .expect("controller task")
}
