use super::*;
use async_tungstenite::tungstenite::Message;
use serde_json::{Value, json};

async fn reply(socket: &mut TestChildSocket, value: Value) {
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .expect("send CDP fixture event");
}

struct TabFixture {
    shared: Arc<Shared>,
    page: Arc<WatchedPage>,
    socket: TestChildSocket,
    commands: mpsc::Sender<super::super::ChildCommand>,
    controller: tokio::task::JoinHandle<bool>,
    arm: Value,
    census: Value,
}

async fn reject_structural_census_at_capacity() {
    const TAB_COUNT: usize = 128;
    assert_eq!(super::super::CHILD_CONTROLLER_PENDING_LIMIT, TAB_COUNT * 2);
    let page = watched("ROOT");
    let mut state = shared_with(&page, "OTHER");
    state.origin = BrowserOrigin::Launched;
    let shared = Arc::new(state);
    register_owned_context(&shared, &page);
    let (mut socket, _commands, controller) = test_child_controller(shared, Duration::from_secs(1)).await;
    let mut observed_arms = 0;
    for index in 0..TAB_COUNT {
        let session = format!("S-CAPACITY-{index}");
        let target = format!("CAPACITY-{index}");
        let context = BrowserContextId::new(format!("CAPACITY-CONTEXT-{index}"));
        send_attached_target_with_waiting(&mut socket, &session, &target, "tab", None, &context, false).await;
        let arm = next_child_message(&mut socket).await;
        assert_eq!(arm["method"], "Target.setAutoAttach");
        assert_eq!(arm["sessionId"], session);
        observed_arms += 1;
    }
    assert_eq!(observed_arms, TAB_COUNT);
    reply(
        &mut socket,
        json!({"method": "Target.detachedFromTarget", "params": {
            "sessionId": "S-CAPACITY-0", "targetId": "CAPACITY-0"
        }}),
    )
    .await;
    let response = tokio::time::timeout(Duration::from_secs(3), socket.next())
        .await
        .expect("capacity rejection must close the controller socket");
    if let Some(Ok(message)) = response {
        assert!(
            !message.is_text(),
            "capacity exhaustion must not issue another command: {message:?}"
        );
    }
    assert_closed(controller).await;
}

async fn detached_tab() -> TabFixture {
    let page = watched("ROOT");
    let mut state = shared_with(&page, "OTHER");
    state.origin = BrowserOrigin::Launched;
    let shared = Arc::new(state);
    register_owned_context(&shared, &page);
    let (mut socket, commands, controller) = test_child_controller(Arc::clone(&shared), Duration::from_secs(1)).await;
    send_attached_target_with_waiting(
        &mut socket,
        "S-TAB",
        "TAB",
        "tab",
        None,
        &BrowserContextId::new("DEFAULT"),
        false,
    )
    .await;
    let arm = next_child_message(&mut socket).await;
    assert_eq!(arm["method"], "Target.setAutoAttach");
    reply(
        &mut socket,
        json!({"method": "Target.detachedFromTarget", "params": {"sessionId": "S-TAB", "targetId": "TAB"}}),
    )
    .await;
    let census = next_child_message(&mut socket).await;
    assert_eq!(census["method"], "Target.getTargets");
    assert_eq!(census["params"], json!({"filter": [{}]}));
    TabFixture {
        shared,
        page,
        socket,
        commands,
        controller,
        arm,
        census,
    }
}

async fn assert_closed(controller: tokio::task::JoinHandle<bool>) {
    assert!(
        !tokio::time::timeout(Duration::from_secs(3), controller)
            .await
            .expect("unproved tab must fail promptly")
            .expect("controller task")
    );
}

#[tokio::test]
async fn a_never_authorized_detached_tab_requires_absence_before_its_exact_arm_can_retire() {
    for late_error in [false, true] {
        let mut fixture = detached_tab().await;
        let owned = lock(&fixture.shared.registry).context_targets[&fixture.page.root].clone();
        send_attached_target_with_waiting(&mut fixture.socket, "S-ROOT", "ROOT", "page", None, &owned, true).await;
        let root_arm = next_child_message(&mut fixture.socket).await;
        answer_child_command(&mut fixture.socket, &root_arm).await;
        let resume = next_child_message(&mut fixture.socket).await;
        assert_eq!(resume["method"], "Runtime.runIfWaitingForDebugger");
        let (done, mut root_ready) = oneshot::channel();
        fixture
            .commands
            .send(super::super::ChildCommand::Arm {
                root: fixture.page.root.clone(),
                done,
            })
            .await
            .unwrap();
        if late_error {
            reply(
                &mut fixture.socket,
                json!({"id": fixture.arm["id"], "error": {"code": -32001}}),
            )
            .await;
        }
        reply(
            &mut fixture.socket,
            json!({"id": fixture.census["id"], "result": {"targetInfos": []}}),
        )
        .await;
        send_attached_target_with_waiting(
            &mut fixture.socket,
            "S-BARRIER",
            "BARRIER",
            "worker",
            None,
            &BrowserContextId::new("UNRELATED"),
            false,
        )
        .await;
        let barrier = next_child_message(&mut fixture.socket).await;
        assert_eq!(barrier["sessionId"], "S-BARRIER");
        answer_child_command(&mut fixture.socket, &barrier).await;
        assert!(matches!(
            root_ready.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        answer_child_command(&mut fixture.socket, &resume).await;
        tokio::time::timeout(Duration::from_secs(1), root_ready)
            .await
            .expect("unrelated root obligation must survive")
            .unwrap()
            .unwrap();
        fixture
            .commands
            .send(super::super::ChildCommand::Stop)
            .await
            .expect("stop controller");
        assert!(
            fixture
                .controller
                .await
                .expect("absent independent tab leaves controller healthy")
        );
    }
}

#[tokio::test]
async fn an_unowned_tab_census_rejects_live_invalid_unanswered_and_acknowledged_arms() {
    for case in [
        "at_capacity",
        "live",
        "missing",
        "null",
        "malformed",
        "error",
        "unanswered",
        "arm_ack",
        "arm_error",
        "destroyed",
        "wrong_arm",
    ] {
        if case == "at_capacity" {
            reject_structural_census_at_capacity().await;
            continue;
        }
        let mut fixture = detached_tab().await;
        match case {
            "unanswered" => {
                assert!(
                    tokio::time::timeout(Duration::from_millis(25), &mut fixture.controller)
                        .await
                        .is_err()
                );
                fixture.socket.close(None).await.expect("close unproved census");
            }
            "arm_ack" => {
                answer_child_command(&mut fixture.socket, &fixture.arm).await;
                reply(
                    &mut fixture.socket,
                    json!({"id": fixture.census["id"], "result": {"targetInfos": []}}),
                )
                .await;
            }
            "arm_error" => {
                reply(
                    &mut fixture.socket,
                    json!({"id": fixture.arm["id"], "error": {"code": -32000}}),
                )
                .await
            }
            "destroyed" => {
                reply(
                    &mut fixture.socket,
                    json!({"method": "Target.targetDestroyed", "params": {"targetId": "TAB"}}),
                )
                .await
            }
            "wrong_arm" => {
                send_attached_target_with_waiting(
                    &mut fixture.socket,
                    "S-OTHER",
                    "OTHER-TAB",
                    "tab",
                    None,
                    &BrowserContextId::new("SECOND"),
                    false,
                )
                .await;
                let other = next_child_message(&mut fixture.socket).await;
                reply(
                    &mut fixture.socket,
                    json!({"id": other["id"], "error": {"code": -32001}}),
                )
                .await;
            }
            _ => {
                let mut value = match case {
                    "live" => {
                        json!({"result": {"targetInfos": [{"targetId": "TAB", "type": "tab", "title": "", "url": "", "attached": false, "canAccessOpener": false}]}})
                    }
                    "missing" => json!({"result": {}}),
                    "null" => json!({"result": {"targetInfos": null}}),
                    "malformed" => json!({"result": {"targetInfos": {}}}),
                    _ => json!({"error": {"code": -32001}}),
                };
                value["id"] = fixture.census["id"].clone();
                reply(&mut fixture.socket, value).await;
            }
        }
        assert_closed(fixture.controller).await;
    }
}

#[tokio::test]
async fn a_detached_tab_cannot_retire_after_ownership_lineage_or_identity_changes() {
    for case in [
        "watched",
        "pending",
        "quarantined",
        "shared",
        "context",
        "transient_context",
        "retired",
        "opened",
        "mapping",
        "transient_watched",
        "transient_other",
        "transient_iframe",
        "replacement",
        "child",
        "held_child",
    ] {
        let mut fixture = detached_tab().await;
        if case == "child" || case == "held_child" {
            if case == "held_child" {
                reply(
                    &mut fixture.socket,
                    json!({"id": fixture.arm["id"], "error": {"code": -32001}}),
                )
                .await;
            }
            reply(&mut fixture.socket, json!({"sessionId": "S-TAB", "method": "Target.attachedToTarget", "params": {
                "sessionId": "S-CHILD", "waitingForDebugger": false, "targetInfo": {"targetId": "CHILD", "type": "worker", "title": "", "url": "", "attached": true,
                    "canAccessOpener": false, "browserContextId": "DEFAULT"}
            }})).await;
            let child_arm = next_child_message(&mut fixture.socket).await;
            assert_eq!(child_arm["method"], "Target.setAutoAttach");
        } else if case == "replacement" {
            send_attached_target_with_waiting(
                &mut fixture.socket,
                "S-REPLACEMENT",
                "TAB",
                "tab",
                None,
                &BrowserContextId::new("DEFAULT"),
                true,
            )
            .await;
            let replacement = next_child_message(&mut fixture.socket).await;
            assert_eq!(replacement["sessionId"], "S-REPLACEMENT");
        } else {
            let mut registry = lock(&fixture.shared.registry);
            let target = TargetId::new("TAB");
            let context = BrowserContextId::new("DEFAULT");
            match case {
                "transient_watched" => {
                    registry.register_watched(target.clone(), Arc::clone(&fixture.page));
                    registry.remove_target(&target);
                }
                "transient_other" => {
                    registry.register_other(target.clone());
                    registry.remove_target(&target);
                }
                "transient_iframe" => {
                    let owned = registry.context_targets[&fixture.page.root].clone();
                    let info = serde_json::from_value(
                        json!({"targetId": "TAB", "type": "iframe", "title": "", "url": "", "attached": true,
                        "canAccessOpener": false, "browserContextId": owned.inner()}),
                    )
                    .unwrap();
                    super::super::adopt_target_in_registry(&mut registry, &info);
                    registry.remove_target(&target);
                }
                "watched" => registry.register_watched(target, Arc::clone(&fixture.page)),
                "pending" => registry.register_pending(target, context),
                "quarantined" => registry.register_quarantined(target, Some(context)),
                "shared" => registry.shared_context = true,
                "context" => registry.register_context(context, None),
                "transient_context" => {
                    registry.register_context(context.clone(), None);
                    registry.contexts.remove(&context);
                }
                "retired" => registry.retired_contexts.push_back(context),
                "opened" => {
                    registry.opened.insert(TargetId::new("ANOTHER"), Some(context));
                }
                "mapping" => {
                    registry
                        .context_targets
                        .insert(target, BrowserContextId::new("DIFFERENT"));
                }
                _ => unreachable!(),
            }
        }
        reply(
            &mut fixture.socket,
            json!({"id": fixture.census["id"], "result": {"targetInfos": []}}),
        )
        .await;
        assert_closed(fixture.controller).await;
    }
}
