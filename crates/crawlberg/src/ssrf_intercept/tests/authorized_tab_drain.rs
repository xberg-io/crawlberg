use super::*;
use async_tungstenite::tungstenite::Message;
use serde_json::{Value, json};

async fn reply(socket: &mut TestChildSocket, value: Value) {
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .expect("send CDP fixture event");
}

#[tokio::test]
async fn an_acked_unowned_tab_with_a_pending_child_requires_both_targets_absent_before_retirement() {
    let mut exercised = 0;
    for (child_detached, live_child, late_child_reply) in [
        (false, false, None),
        (false, true, None),
        (false, false, Some(true)),
        (false, false, Some(false)),
        (true, false, None),
        (true, true, None),
        (true, false, Some(true)),
        (true, false, Some(false)),
    ] {
        let page = watched("ROOT");
        let mut state = shared_with(&page, "OTHER");
        state.origin = BrowserOrigin::Launched;
        let shared = Arc::new(state);
        let owned = register_owned_context(&shared, &page);
        let unknown = BrowserContextId::new("DEFAULT");
        let (mut socket, commands, controller) =
            test_child_controller(Arc::clone(&shared), Duration::from_secs(1)).await;
        send_attached_target_with_waiting(&mut socket, "S-TAB", "TAB", "tab", None, &unknown, false).await;
        let tab_arm = next_child_message(&mut socket).await;
        assert_eq!(tab_arm["method"], "Target.setAutoAttach");
        assert_eq!(tab_arm["sessionId"], "S-TAB");
        answer_child_command(&mut socket, &tab_arm).await;
        let mut unrelated_arms = Vec::new();
        for (session, target) in [
            ("S-UNRELATED-ONE", "UNRELATED-ONE"),
            ("S-UNRELATED-TWO", "UNRELATED-TWO"),
        ] {
            send_attached_target_with_waiting(&mut socket, session, target, "browser_ui", None, &unknown, true).await;
            let arm = next_child_message(&mut socket).await;
            assert_eq!(arm["method"], "Target.setAutoAttach");
            assert_eq!(arm["sessionId"], session);
            unrelated_arms.push(arm);
        }
        reply(
            &mut socket,
            json!({"sessionId": "S-TAB", "method": "Target.attachedToTarget", "params": {
                "sessionId": "S-CHILD", "waitingForDebugger": false,
                "targetInfo": {"targetId": "CHILD", "type": "page", "title": "", "url": "",
                    "attached": true, "canAccessOpener": false, "browserContextId": "DEFAULT"}
            }}),
        )
        .await;
        let child_arm = next_child_message(&mut socket).await;
        assert_eq!(child_arm["method"], "Target.setAutoAttach");
        assert_eq!(child_arm["sessionId"], "S-CHILD");
        {
            let registry = lock(&shared.registry);
            assert!(!registry.contexts.contains_key(&unknown));
            assert!(!registry.retired_contexts.contains(&unknown));
            assert!(!registry.ownership.contains_key(&TargetId::new("TAB")));
            assert!(matches!(
                registry.ownership.get(&TargetId::new("CHILD")),
                Some(TargetOwnership::Other)
            ));
            assert!(!registry.context_targets.contains_key(&TargetId::new("CHILD")));
            assert!(!registry.opened.contains_key(&TargetId::new("CHILD")));
        }
        send_attached_target_with_waiting(&mut socket, "S-ROOT", "ROOT", "page", None, &owned, true).await;
        let root_arm = next_child_message(&mut socket).await;
        assert_eq!(root_arm["sessionId"], "S-ROOT");
        answer_child_command(&mut socket, &root_arm).await;
        let root_resume = next_child_message(&mut socket).await;
        assert_eq!(root_resume["method"], "Runtime.runIfWaitingForDebugger");
        assert_eq!(root_resume["sessionId"], "S-ROOT");
        let (done, mut root_ready) = oneshot::channel();
        commands
            .send(super::super::ChildCommand::Arm {
                root: page.root.clone(),
                done,
            })
            .await
            .unwrap();
        if child_detached {
            reply(
                &mut socket,
                json!({"method": "Target.detachedFromTarget", "params": {
                    "sessionId": "S-CHILD", "targetId": "CHILD"
                }}),
            )
            .await;
        }
        reply(
            &mut socket,
            json!({"method": "Target.detachedFromTarget", "params": {
                "sessionId": "S-TAB", "targetId": "TAB"
            }}),
        )
        .await;
        let census = tokio::time::timeout(Duration::from_secs(3), next_child_message(&mut socket))
            .await
            .expect("detached tab must obtain an absence proof before retiring any child command");
        assert_eq!(census["method"], "Target.getTargets");
        assert_eq!(census["params"], json!({"filter": [{}]}));
        assert!(census.get("sessionId").is_none());
        if let Some(acknowledged) = late_child_reply {
            if acknowledged {
                answer_child_command(&mut socket, &child_arm).await;
            } else {
                reply(&mut socket, json!({"id": child_arm["id"], "error": {"code": -32001}})).await;
            }
            send_attached_target_with_waiting(
                &mut socket,
                "S-BARRIER",
                "BARRIER",
                "worker",
                None,
                &BrowserContextId::new("UNRELATED"),
                false,
            )
            .await;
            let barrier = next_child_message(&mut socket).await;
            assert_eq!(barrier["method"], "Target.setAutoAttach");
            assert_eq!(
                barrier["sessionId"], "S-BARRIER",
                "the child ACK must not issue a resume during census"
            );
            answer_child_command(&mut socket, &barrier).await;
        }
        let mut targets = vec![
            json!({"targetId": "UNRELATED-ONE", "type": "browser_ui", "title": "", "url": "",
                "attached": true, "canAccessOpener": false, "browserContextId": "DEFAULT"}),
            json!({"targetId": "UNRELATED-TWO", "type": "browser_ui", "title": "", "url": "",
                "attached": true, "canAccessOpener": false, "browserContextId": "DEFAULT"}),
        ];
        if live_child {
            targets.push(json!({"targetId": "CHILD", "type": "page", "title": "", "url": "",
                "attached": true, "canAccessOpener": false, "browserContextId": "DEFAULT"}));
        }
        reply(
            &mut socket,
            json!({"id": census["id"], "result": {"targetInfos": targets}}),
        )
        .await;
        if live_child {
            let response = tokio::time::timeout(Duration::from_secs(3), socket.next())
                .await
                .expect("a live child without an acknowledged boundary must fail closed");
            if let Some(Ok(message)) = response {
                assert!(
                    !message.is_text(),
                    "a live unarmed child must never be resumed: {message:?}"
                );
            }
            assert!(
                !tokio::time::timeout(Duration::from_secs(3), controller)
                    .await
                    .expect("live child rejection must be bounded")
                    .unwrap()
            );
            exercised += 1;
            continue;
        }
        reply(&mut socket, json!({"id": child_arm["id"], "error": {"code": -32001}})).await;
        for arm in &unrelated_arms {
            answer_child_command(&mut socket, arm).await;
            let resume = next_child_message(&mut socket).await;
            assert_eq!(resume["method"], "Runtime.runIfWaitingForDebugger");
            assert_eq!(
                resume["sessionId"], arm["sessionId"],
                "unrelated same-context ACK must survive"
            );
            answer_child_command(&mut socket, &resume).await;
        }
        assert!(matches!(
            root_ready.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        answer_child_command(&mut socket, &root_resume).await;
        tokio::time::timeout(Duration::from_secs(3), root_ready)
            .await
            .expect("unrelated owned root obligation must survive family retirement")
            .unwrap()
            .unwrap();
        commands.send(super::super::ChildCommand::Stop).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(3), controller)
                .await
                .expect("absence-proved retirement must remain healthy")
                .unwrap()
        );
        exercised += 1;
    }
    assert_eq!(exercised, 8);
}
