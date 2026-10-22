use super::*;
use async_tungstenite::tungstenite::Message;
use serde_json::{Value, json};

async fn reply(socket: &mut TestChildSocket, value: Value) {
    socket.send(Message::Text(value.to_string().into())).await.unwrap();
}

struct PendingResume {
    shared: Arc<Shared>,
    page: Arc<WatchedPage>,
    socket: TestChildSocket,
    commands: mpsc::Sender<super::super::ChildCommand>,
    controller: tokio::task::JoinHandle<bool>,
    owned_context: BrowserContextId,
    target_context: BrowserContextId,
    resume: Value,
}

async fn pending_resume(owned: bool) -> PendingResume {
    let page = watched("ROOT");
    let mut state = shared_with(&page, "OTHER");
    state.origin = BrowserOrigin::Launched;
    let shared = Arc::new(state);
    let owned_context = register_owned_context(&shared, &page);
    let target_context = if owned {
        owned_context.clone()
    } else {
        BrowserContextId::new("DEFAULT")
    };
    let target = if owned { "ROOT" } else { "OTHER" };
    let (mut socket, commands, controller) = test_child_controller(Arc::clone(&shared), Duration::from_secs(1)).await;
    send_attached_target_with_waiting(&mut socket, "S-CANDIDATE", target, "page", None, &target_context, true).await;
    let arm = next_child_message(&mut socket).await;
    assert_eq!(arm["method"], "Target.setAutoAttach");
    assert_eq!(arm["sessionId"], "S-CANDIDATE");
    if !owned {
        let registry = lock(&shared.registry);
        assert!(matches!(
            registry.ownership.get(&TargetId::new(target)),
            Some(TargetOwnership::Other)
        ));
        assert!(!registry.contexts.contains_key(&target_context));
        assert!(!registry.context_targets.contains_key(&TargetId::new(target)));
        assert!(!registry.opened.contains_key(&TargetId::new(target)));
    }
    answer_child_command(&mut socket, &arm).await;
    let resume = next_child_message(&mut socket).await;
    assert_eq!(resume["method"], "Runtime.runIfWaitingForDebugger");
    assert_eq!(resume["sessionId"], "S-CANDIDATE");
    PendingResume {
        shared,
        page,
        socket,
        commands,
        controller,
        owned_context,
        target_context,
        resume,
    }
}

async fn detach_other(fixture: &mut PendingResume) {
    reply(
        &mut fixture.socket,
        json!({"method": "Target.detachedFromTarget", "params": {
            "sessionId": "S-CANDIDATE", "targetId": "OTHER"
        }}),
    )
    .await;
}

async fn reject_without_another_command(mut fixture: PendingResume) {
    let next = tokio::time::timeout(Duration::from_secs(3), fixture.socket.next())
        .await
        .expect("unproved resume retirement must fail promptly");
    if let Some(Ok(message)) = next {
        assert!(
            !message.is_text(),
            "unproved retirement must not issue a command: {message:?}"
        );
    }
    assert!(
        !tokio::time::timeout(Duration::from_secs(3), fixture.controller)
            .await
            .expect("unproved resume retirement must close the controller")
            .unwrap()
    );
}

struct HeldOwnedResume {
    resume: Value,
    ready: oneshot::Receiver<Result<(), String>>,
}

async fn hold_owned_root(fixture: &mut PendingResume) -> HeldOwnedResume {
    send_attached_target_with_waiting(
        &mut fixture.socket,
        "S-ROOT",
        "ROOT",
        "page",
        None,
        &fixture.owned_context,
        true,
    )
    .await;
    let arm = next_child_message(&mut fixture.socket).await;
    assert_eq!(arm["method"], "Target.setAutoAttach");
    assert_eq!(arm["sessionId"], "S-ROOT");
    answer_child_command(&mut fixture.socket, &arm).await;
    let resume = next_child_message(&mut fixture.socket).await;
    assert_eq!(resume["method"], "Runtime.runIfWaitingForDebugger");
    assert_eq!(resume["sessionId"], "S-ROOT");
    let (done, mut ready) = oneshot::channel();
    fixture
        .commands
        .send(super::super::ChildCommand::Arm {
            root: TargetId::new("ROOT"),
            done,
        })
        .await
        .unwrap();
    let (done, barrier) = oneshot::channel();
    fixture
        .commands
        .send(super::super::ChildCommand::Enable { done })
        .await
        .unwrap();
    barrier.await.unwrap().unwrap();
    assert!(matches!(ready.try_recv(), Err(oneshot::error::TryRecvError::Empty)));
    HeldOwnedResume { resume, ready }
}

async fn prove_owned_root_still_works(mut fixture: PendingResume, mut owned: HeldOwnedResume) {
    assert!(matches!(
        owned.ready.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    answer_child_command(&mut fixture.socket, &owned.resume).await;
    tokio::time::timeout(Duration::from_secs(3), owned.ready)
        .await
        .expect("the owned root still requires its own resume acknowledgement")
        .unwrap()
        .unwrap();
    fixture.commands.send(super::super::ChildCommand::Stop).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(3), fixture.controller)
            .await
            .expect("absence-proved controller must stop promptly")
            .unwrap()
    );
}

async fn replay_held_resume(fixture: &mut PendingResume, acknowledged: Option<bool>) {
    match acknowledged {
        Some(true) => answer_child_command(&mut fixture.socket, &fixture.resume).await,
        Some(false) => {
            reply(
                &mut fixture.socket,
                json!({"id": fixture.resume["id"], "error": {"code": -32001}}),
            )
            .await
        }
        None => return,
    }
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
    assert_eq!(barrier["method"], "Target.setAutoAttach");
    assert_eq!(
        barrier["sessionId"], "S-BARRIER",
        "a held reply must not issue another resume"
    );
    answer_child_command(&mut fixture.socket, &barrier).await;
}

async fn prove_resume_retirement_case(ended: &str, live: bool, late_reply: Option<bool>) {
    let mut fixture = pending_resume(false).await;
    let mut owned = hold_owned_root(&mut fixture).await;
    if ended == "before_error" {
        detach_other(&mut fixture).await;
    }
    reply(
        &mut fixture.socket,
        json!({"id": fixture.resume["id"], "error": {"code": -32001}}),
    )
    .await;
    let census = next_child_message(&mut fixture.socket).await;
    assert_eq!(census["method"], "Target.getTargets");
    assert_eq!(census["params"], json!({"filter": [{}]}));
    assert!(census.get("sessionId").is_none());
    assert_ne!(census["id"], fixture.resume["id"]);
    if ended == "during_census" {
        detach_other(&mut fixture).await;
    }
    replay_held_resume(&mut fixture, late_reply).await;
    assert!(matches!(
        owned.ready.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    let targets = if live {
        vec![
            json!({"targetId": "OTHER", "type": "page", "title": "", "url": "", "attached": false,
            "canAccessOpener": false, "browserContextId": fixture.target_context.inner()}),
        ]
    } else {
        Vec::new()
    };
    reply(
        &mut fixture.socket,
        json!({"id": census["id"], "result": {"targetInfos": targets}}),
    )
    .await;
    if live {
        reject_without_another_command(fixture).await;
    } else {
        prove_owned_root_still_works(fixture, owned).await;
    }
}

#[tokio::test]
async fn an_unowned_resume_session_error_requires_absence_and_never_retires_an_owned_resume() {
    let mut completed = 0;
    for ended in ["none", "before_error", "during_census"] {
        for live in [false, true] {
            for late_reply in [None, Some(true), Some(false)] {
                prove_resume_retirement_case(ended, live, late_reply).await;
                completed += 1;
            }
        }
    }
    let mut owned = pending_resume(true).await;
    reply(
        &mut owned.socket,
        json!({"id": owned.resume["id"], "error": {"code": -32001}}),
    )
    .await;
    reject_without_another_command(owned).await;
    completed += 1;
    assert_eq!(completed, 19);
}

fn mutate_resume_proof(fixture: &PendingResume, change: &str) -> Option<Arc<()>> {
    let mut registry = lock(&fixture.shared.registry);
    let target = TargetId::new("OTHER");
    match change {
        "context_reuse" => {
            let context = fixture.target_context.clone();
            let original = registry.unowned_context_observers[&context].upgrade().unwrap();
            registry.register_context(context.clone(), None);
            registry.contexts.remove(&context);
            let replacement = registry.observe_unowned_context(&context).unwrap();
            assert!(!Arc::ptr_eq(&original, &replacement));
            assert!(!registry.unowned_context_matches(&context, &original));
            return Some(replacement);
        }
        "watched_target" => registry.register_watched(target, Arc::clone(&fixture.page)),
        "target_reuse" => {
            let original = registry.target_generations[&target].clone();
            registry.register_watched(target.clone(), Arc::clone(&fixture.page));
            registry.remove_target(&target);
            registry.register_other(target.clone());
            assert!(matches!(registry.ownership.get(&target), Some(TargetOwnership::Other)));
            assert!(!Arc::ptr_eq(&original, &registry.target_generations[&target]));
        }
        "owned_frame" => {
            let frame = registry
                .current_frame_owner(target.clone(), Some(Arc::clone(&fixture.page)))
                .unwrap();
            registry.frames.insert(FrameId::new("FRAME-OTHER"), frame);
            assert!(matches!(registry.ownership.get(&target), Some(TargetOwnership::Other)));
        }
        "opened_target" => {
            registry.opened.insert(target, Some(fixture.owned_context.clone()));
        }
        _ => unreachable!("unknown resume proof mutation: {change}"),
    }
    None
}

#[tokio::test]
async fn an_unowned_resume_census_rejects_reused_identity_and_new_owned_obligations() {
    let cases = [
        "target_reattach",
        "session_reattach",
        "context_reuse",
        "watched_target",
        "target_reuse",
        "owned_frame",
        "opened_target",
        "malformed_targets",
        "missing_result",
        "unanswered",
        "held_other_error",
    ];
    assert_eq!(cases.len(), 11);
    let mut completed = 0;
    for change in cases {
        let mut fixture = pending_resume(false).await;
        reply(
            &mut fixture.socket,
            json!({"id": fixture.resume["id"], "error": {"code": -32001}}),
        )
        .await;
        let census = next_child_message(&mut fixture.socket).await;
        assert_eq!(census["method"], "Target.getTargets");
        assert_eq!(census["params"], json!({"filter": [{}]}));
        assert!(census.get("sessionId").is_none());
        let retained_context = if change == "target_reattach" || change == "session_reattach" {
            let (session, target) = if change == "target_reattach" {
                ("S-REPLACEMENT", "OTHER")
            } else {
                ("S-CANDIDATE", "REPLACEMENT")
            };
            send_attached_target_with_waiting(
                &mut fixture.socket,
                session,
                target,
                "page",
                None,
                &fixture.target_context,
                true,
            )
            .await;
            None
        } else if change == "malformed_targets" {
            reply(
                &mut fixture.socket,
                json!({"id": census["id"], "result": {"targetInfos": {}}}),
            )
            .await;
            None
        } else if change == "missing_result" {
            reply(&mut fixture.socket, json!({"id": census["id"]})).await;
            None
        } else if change == "unanswered" {
            assert!(
                tokio::time::timeout(Duration::from_millis(25), &mut fixture.controller)
                    .await
                    .is_err()
            );
            fixture
                .socket
                .close(None)
                .await
                .expect("close an unanswered census socket");
            None
        } else if change == "held_other_error" {
            reply(
                &mut fixture.socket,
                json!({"id": fixture.resume["id"], "error": {"code": -32000}}),
            )
            .await;
            None
        } else {
            let replacement = mutate_resume_proof(&fixture, change);
            reply(
                &mut fixture.socket,
                json!({"id": census["id"], "result": {"targetInfos": []}}),
            )
            .await;
            replacement
        };
        reject_without_another_command(fixture).await;
        drop(retained_context);
        completed += 1;
    }
    assert_eq!(completed, 11);
}
