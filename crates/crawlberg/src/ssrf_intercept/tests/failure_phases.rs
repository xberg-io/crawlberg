use super::*;

async fn captured_child_events<F: std::future::Future>(future: F) -> (F::Output, Vec<serde_json::Value>) {
    use tracing::instrument::WithSubscriber as _;

    let captured = tempfile::NamedTempFile::new().expect("captured controller events");
    let writer = captured.reopen().expect("capture writer");
    let subscriber = tracing_subscriber::fmt()
        .json()
        .without_time()
        .with_max_level(tracing::Level::ERROR)
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
        })
    );
}

#[tokio::test]
async fn a_missing_child_acknowledgement_reports_the_deadline_phase() {
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
        tokio::time::timeout(Duration::from_secs(2), controller)
            .await
            .expect("acknowledgement deadline")
            .expect("controller task")
    })
    .await;
    assert!(!ended, "an unanswered boundary must still fail closed");
    assert_eq!(events.len(), 1, "one first controller failure must be reported");
    assert_eq!(events[0]["fields"]["phase"], "child_target_controller");
    assert_eq!(events[0]["fields"]["reason"], "pending_command_deadline");
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
