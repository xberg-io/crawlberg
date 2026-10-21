use super::super::{
    ChildPending, ChildPendingKind, RootWaiter, StructuralTarget, child_pending, unowned_arm_is_independent,
};
use super::*;
use async_tungstenite::tungstenite::Message;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

async fn reply(socket: &mut TestChildSocket, value: Value) {
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .expect("send controlled CDP reply");
}

#[tokio::test]
async fn a_launched_browser_ui_session_error_requires_an_all_target_absence_census() {
    for kind in ["browser_ui", "page", "worker"] {
        for waiting in [false, true] {
            let page = watched("ROOT");
            let mut state = shared_with(&page, "OTHER");
            state.origin = BrowserOrigin::Launched;
            let shared = Arc::new(state);
            let owned = register_owned_context(&shared, &page);
            let context = BrowserContextId::new("DEFAULT");
            let (mut socket, commands, controller) =
                test_child_controller(Arc::clone(&shared), Duration::from_secs(1)).await;
            send_attached_target_with_waiting(&mut socket, "S-UI", "OTHER", kind, None, &context, waiting).await;
            let arm = next_child_message(&mut socket).await;
            assert_eq!(arm["method"], "Target.setAutoAttach");
            reply(
                &mut socket,
                json!({"id": arm["id"], "error": {"code": -32001,
        "message": "Session with given id not found."}}),
            )
            .await;
            let message = tokio::time::timeout(Duration::from_secs(1), socket.next())
                .await
                .expect("the missing session must be checked promptly")
                .expect("the controller must request proof before closing")
                .expect("read the census request");
            assert!(message.is_text(), "expected an absence census, got {message:?}");
            let census: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
            assert_eq!(census["method"], "Target.getTargets");
            assert_eq!(census["params"], json!({"filter": [{}]}));
            reply(&mut socket, json!({"id": census["id"], "result": {"targetInfos": []}})).await;
            send_attached_target_with_waiting(&mut socket, "S-ROOT", "ROOT", "page", None, &owned, false).await;
            let root_arm = next_child_message(&mut socket).await;
            assert_eq!(root_arm["sessionId"], "S-ROOT");
            answer_child_command(&mut socket, &root_arm).await;
            commands
                .send(super::super::ChildCommand::Stop)
                .await
                .expect("stop checked controller");
            assert!(
                controller
                    .await
                    .expect("controller must remain healthy after proved absence")
            );
        }
    }
}

struct CensusFixture {
    shared: Arc<Shared>,
    page: Arc<WatchedPage>,
    socket: TestChildSocket,
    _commands: mpsc::Sender<super::super::ChildCommand>,
    controller: tokio::task::JoinHandle<bool>,
    census: Value,
}

async fn census_fixture() -> CensusFixture {
    let page = watched("ROOT");
    let mut state = shared_with(&page, "OTHER");
    state.origin = BrowserOrigin::Launched;
    let shared = Arc::new(state);
    register_owned_context(&shared, &page);
    let (mut socket, commands, controller) = test_child_controller(Arc::clone(&shared), Duration::from_secs(1)).await;
    send_attached_target_with_waiting(
        &mut socket,
        "S-UI",
        "OTHER",
        "browser_ui",
        None,
        &BrowserContextId::new("DEFAULT"),
        false,
    )
    .await;
    let arm = next_child_message(&mut socket).await;
    reply(&mut socket, json!({"id": arm["id"], "error": {"code": -32001}})).await;
    let census = next_child_message(&mut socket).await;
    assert_eq!(census["method"], "Target.getTargets");
    assert_eq!(census["params"], json!({"filter": [{}]}));
    CensusFixture {
        shared,
        page,
        socket,
        _commands: commands,
        controller,
        census,
    }
}

async fn assert_closed(controller: tokio::task::JoinHandle<bool>) {
    assert!(
        !tokio::time::timeout(Duration::from_secs(1), controller)
            .await
            .expect("unproved retirement must fail promptly")
            .expect("controller task")
    );
}

#[tokio::test]
async fn an_unowned_absence_census_rejects_live_rejected_missing_and_malformed_results() {
    let results = [
        json!({"error": {"code": -32000}}),
        json!({"result": {}}),
        json!({"result": {"targetInfos": null}}),
        json!({"result": {"targetInfos": {}}}),
        json!({"result": {"targetInfos": [{"targetId": "OTHER"}]}}),
    ];
    for mut value in results {
        let mut fixture = census_fixture().await;
        value["id"] = fixture.census["id"].clone();
        reply(&mut fixture.socket, value).await;
        assert_closed(fixture.controller).await;
    }
    for kind in ["page", "tab", "browser", "browser_ui"] {
        let mut fixture = census_fixture().await;
        reply(
            &mut fixture.socket,
            json!({"id": fixture.census["id"], "result": {"targetInfos": [{
                "targetId": "OTHER", "type": kind, "title": "", "url": "", "attached": false,
                "canAccessOpener": false, "browserContextId": "DEFAULT"
            }]}}),
        )
        .await;
        assert_closed(fixture.controller).await;
    }
}

#[tokio::test]
async fn an_unanswered_unowned_census_never_acknowledges_retirement() {
    let mut fixture = census_fixture().await;
    assert!(
        tokio::time::timeout(Duration::from_millis(25), &mut fixture.controller)
            .await
            .is_err()
    );
    fixture
        .socket
        .close(None)
        .await
        .expect("close unanswered census socket");
    assert_closed(fixture.controller).await;
}

#[tokio::test]
async fn an_unowned_census_rejects_changed_ownership_context_and_generation() {
    for change in 0..11 {
        let mut fixture = census_fixture().await;
        {
            let mut registry = lock(&fixture.shared.registry);
            let target = TargetId::new("OTHER");
            let context = BrowserContextId::new("DEFAULT");
            match change {
                0 => registry.register_watched(target, Arc::clone(&fixture.page)),
                1 => registry.register_pending(target, context),
                2 => registry.register_pending_shared(target),
                3 => registry.register_quarantined(target, Some(context)),
                4 => registry.register_context(context, None),
                5 => {
                    registry.retired_contexts.push_back(context);
                }
                6 => {
                    registry.opened.insert(target, Some(context));
                }
                7 => {
                    registry.context_targets.insert(target, context);
                }
                8 => {
                    registry.shared_context = true;
                }
                9 => {
                    registry.remove_target(&target);
                    registry.register_other(target);
                }
                10 => {
                    registry.register_context(context.clone(), None);
                    registry.contexts.remove(&context);
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

#[tokio::test]
async fn an_unowned_census_cannot_retire_a_second_session_or_its_pending_arm() {
    for (session, target) in [("S-SECOND", "OTHER"), ("S-UI", "SECOND")] {
        let mut fixture = census_fixture().await;
        send_attached_target_with_waiting(
            &mut fixture.socket,
            session,
            target,
            "page",
            None,
            &BrowserContextId::new("DEFAULT"),
            true,
        )
        .await;
        let arm = next_child_message(&mut fixture.socket).await;
        assert_eq!(arm["method"], "Target.setAutoAttach");
        reply(
            &mut fixture.socket,
            json!({"id": fixture.census["id"], "result": {"targetInfos": []}}),
        )
        .await;
        assert_closed(fixture.controller).await;
    }
}

#[tokio::test]
async fn a_session_error_cannot_select_a_replacement_target_generation() {
    let page = watched("ROOT");
    let mut state = shared_with(&page, "OTHER");
    state.origin = BrowserOrigin::Launched;
    let shared = Arc::new(state);
    register_owned_context(&shared, &page);
    let (mut socket, _commands, controller) = test_child_controller(Arc::clone(&shared), Duration::from_secs(1)).await;
    send_attached_target_with_waiting(
        &mut socket,
        "S-UI",
        "OTHER",
        "browser_ui",
        None,
        &BrowserContextId::new("DEFAULT"),
        false,
    )
    .await;
    let arm = next_child_message(&mut socket).await;
    {
        let mut registry = lock(&shared.registry);
        registry.remove_target(&TargetId::new("OTHER"));
        registry.register_other(TargetId::new("OTHER"));
    }
    reply(&mut socket, json!({"id": arm["id"], "error": {"code": -32001}})).await;
    assert_closed(controller).await;
}

#[tokio::test]
async fn a_nested_non_tab_session_cannot_use_an_unowned_root_census() {
    let page = watched("ROOT");
    let mut state = shared_with(&page, "OTHER");
    state.origin = BrowserOrigin::Launched;
    let shared = Arc::new(state);
    register_owned_context(&shared, &page);
    let (mut socket, _commands, controller) = test_child_controller(Arc::clone(&shared), Duration::from_secs(1)).await;
    reply(
        &mut socket,
        json!({"sessionId": "OWNED-PARENT", "method": "Target.attachedToTarget", "params": {
            "sessionId": "S-CHILD", "waitingForDebugger": false, "targetInfo": {
                "targetId": "OTHER", "type": "worker", "title": "", "url": "", "attached": true,
                "canAccessOpener": false, "browserContextId": "DEFAULT"
            }
        }}),
    )
    .await;
    let arm = next_child_message(&mut socket).await;
    reply(&mut socket, json!({"id": arm["id"], "error": {"code": -32001}})).await;
    assert_closed(controller).await;
}

#[derive(Default)]
struct IndependentState {
    pending: HashMap<u64, ChildPending>,
    roots: HashMap<TargetId, RootWaiter>,
    armed: HashMap<TargetId, HashSet<String>>,
    sessions: HashMap<String, TargetId>,
    structural: HashMap<String, StructuralTarget>,
}

impl IndependentState {
    fn candidate() -> Self {
        Self {
            sessions: HashMap::from([("S-UI".to_owned(), TargetId::new("OTHER"))]),
            ..Self::default()
        }
    }

    fn independent(&self) -> bool {
        unowned_arm_is_independent(
            &TargetId::new("OTHER"),
            "S-UI",
            &self.pending,
            &self.roots,
            &self.armed,
            &self.sessions,
            &self.structural,
        )
    }

    fn command(&mut self, kind: ChildPendingKind) {
        self.pending.insert(9, child_pending(kind));
    }
}

#[test]
fn an_unowned_census_rejects_each_dependent_controller_obligation() {
    let mutations: &[fn(&mut IndependentState)] = &[
        |state| {
            state.roots.insert(TargetId::new("OTHER"), oneshot::channel().0);
        },
        |state| {
            state.armed.insert(TargetId::new("OTHER"), HashSet::new());
        },
        |state| {
            state
                .armed
                .insert(TargetId::new("ROOT"), HashSet::from(["S-UI".to_owned()]));
        },
        |state| {
            state.sessions.insert("S-SECOND".to_owned(), TargetId::new("OTHER"));
        },
        |state| {
            state.sessions.remove("S-UI");
        },
        |state| {
            state.command(ChildPendingKind::Boundary {
                session: "S-UI".to_owned(),
            })
        },
        |state| {
            state.command(ChildPendingKind::StructuralArm {
                session: "S-UI".to_owned(),
            })
        },
        |state| {
            state.command(ChildPendingKind::ForeignRetirementCensus {
                target: TargetId::new("OTHER"),
                session: "S-SECOND".to_owned(),
            })
        },
        |state| {
            state.command(ChildPendingKind::RecursiveArm {
                target: TargetId::new("OTHER"),
                session: "S-SECOND".to_owned(),
                waiting_for_debugger: false,
                structural_parent: None,
                unowned: None,
            })
        },
        |state| {
            state.command(ChildPendingKind::RecursiveArm {
                target: TargetId::new("SECOND"),
                session: "S-SECOND".to_owned(),
                waiting_for_debugger: false,
                structural_parent: Some("S-UI".to_owned()),
                unowned: None,
            })
        },
        |state| {
            state.structural.insert(
                "S-TAB".to_owned(),
                StructuralTarget {
                    target: TargetId::new("OTHER"),
                    context: None,
                    session: "S-TAB".to_owned(),
                    waiting_for_debugger: true,
                    authorized: true,
                    arm_acknowledged: false,
                    child_boundary_acknowledged: false,
                    unowned: None,
                    parent_session_present: false,
                    proof_observation: Default::default(),
                },
            );
        },
    ];
    assert!(IndependentState::candidate().independent());
    for (case, mutation) in mutations.iter().enumerate() {
        let mut state = IndependentState::candidate();
        mutation(&mut state);
        assert!(!state.independent(), "dependent obligation case {case}");
    }
}

#[tokio::test]
async fn an_unowned_census_preserves_an_unrelated_owned_root_resume() {
    let page = watched("ROOT");
    let mut state = shared_with(&page, "OTHER");
    state.origin = BrowserOrigin::Launched;
    let shared = Arc::new(state);
    let owned = register_owned_context(&shared, &page);
    let (mut socket, commands, controller) = test_child_controller(Arc::clone(&shared), Duration::from_secs(1)).await;
    send_attached_target_with_waiting(&mut socket, "S-ROOT", "ROOT", "page", None, &owned, true).await;
    let root_arm = next_child_message(&mut socket).await;
    answer_child_command(&mut socket, &root_arm).await;
    let resume = next_child_message(&mut socket).await;
    assert_eq!(resume["method"], "Runtime.runIfWaitingForDebugger");
    let (done, mut root_ready) = oneshot::channel();
    commands
        .send(super::super::ChildCommand::Arm {
            root: page.root.clone(),
            done,
        })
        .await
        .expect("arm root");
    send_attached_target_with_waiting(
        &mut socket,
        "S-UI",
        "OTHER",
        "worker",
        None,
        &BrowserContextId::new("DEFAULT"),
        true,
    )
    .await;
    let arm = next_child_message(&mut socket).await;
    reply(&mut socket, json!({"id": arm["id"], "error": {"code": -32001}})).await;
    let census = next_child_message(&mut socket).await;
    assert_eq!(census["method"], "Target.getTargets");
    reply(&mut socket, json!({"id": census["id"], "result": {"targetInfos": []}})).await;
    send_attached_target_with_waiting(
        &mut socket,
        "S-BARRIER",
        "BARRIER",
        "worker",
        None,
        &BrowserContextId::new("DEFAULT"),
        false,
    )
    .await;
    let barrier = next_child_message(&mut socket).await;
    assert_eq!(barrier["sessionId"], "S-BARRIER");
    answer_child_command(&mut socket, &barrier).await;
    assert!(matches!(
        root_ready.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    answer_child_command(&mut socket, &resume).await;
    tokio::time::timeout(Duration::from_secs(1), root_ready)
        .await
        .expect("unrelated root boundary must remain acknowledged")
        .expect("root resume waiter")
        .expect("owned root remains armed");
    commands.send(super::super::ChildCommand::Stop).await.expect("stop");
    assert!(controller.await.expect("controller remains healthy"));
}

struct BlockedArmSocket {
    inbound: mpsc::Receiver<Message>,
    outbound: mpsc::Sender<Message>,
    entered: Option<oneshot::Sender<()>>,
    released: Option<oneshot::Receiver<()>>,
}

impl super::super::child_transport::ChildSocket for BlockedArmSocket {
    type Error = async_tungstenite::tungstenite::Error;

    fn into_cdp_error(error: Self::Error) -> chromiumoxide::error::CdpError {
        chromiumoxide::error::CdpError::Ws(error)
    }

    async fn send(&mut self, message: Message) -> Result<(), Self::Error> {
        let value: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
        if value["sessionId"] == "S-UI" && value["method"] == "Target.setAutoAttach" {
            self.entered
                .take()
                .expect("one blocked recursive write")
                .send(())
                .unwrap();
            self.released.take().expect("release blocked arm").await.unwrap();
        }
        self.outbound
            .send(message)
            .await
            .map_err(|_| Self::Error::ConnectionClosed)
    }

    async fn close(
        &mut self,
        _frame: Option<async_tungstenite::tungstenite::protocol::CloseFrame>,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn next(&mut self) -> Option<Result<Message, Self::Error>> {
        self.inbound.recv().await.map(Ok)
    }
}

async fn channel_reply(sender: &mpsc::Sender<Message>, value: Value) {
    sender.send(Message::Text(value.to_string().into())).await.unwrap();
}

async fn channel_command(receiver: &mut mpsc::Receiver<Message>) -> Value {
    let message = tokio::time::timeout(Duration::from_secs(1), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    serde_json::from_str(message.to_text().unwrap()).unwrap()
}

#[tokio::test]
async fn an_arm_write_cannot_capture_a_replacement_generation_after_its_await() {
    let page = watched("ROOT");
    let mut state = shared_with(&page, "OTHER");
    state.origin = BrowserOrigin::Launched;
    let shared = Arc::new(state);
    register_owned_context(&shared, &page);
    let (inbound, receive) = mpsc::channel(8);
    let (send, mut outbound) = mpsc::channel(8);
    let (entered, blocked) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let mut socket = BlockedArmSocket {
        inbound: receive,
        outbound: send,
        entered: Some(entered),
        released: Some(released),
    };
    let (commands, mut receiver) = mpsc::channel(8);
    let owned = Arc::clone(&shared);
    let controller = tokio::spawn(async move {
        super::super::serve_child_targets_for_browser(
            &mut socket,
            &mut receiver,
            &owned,
            Duration::from_secs(1),
            None,
            false,
        )
        .await
    });
    let (done, enabled) = oneshot::channel();
    commands
        .send(super::super::ChildCommand::Enable { done })
        .await
        .unwrap();
    let global = channel_command(&mut outbound).await;
    channel_reply(&inbound, json!({"id": global["id"], "result": {}})).await;
    enabled.await.unwrap().unwrap();
    channel_reply(
        &inbound,
        json!({"method": "Target.attachedToTarget", "params": {
            "sessionId": "S-UI", "waitingForDebugger": false, "targetInfo": {
                "targetId": "OTHER", "type": "browser_ui", "title": "", "url": "", "attached": true,
                "canAccessOpener": false, "browserContextId": "DEFAULT"
            }
        }}),
    )
    .await;
    blocked.await.expect("recursive arm write must enter its await");
    {
        let mut registry = lock(&shared.registry);
        registry.remove_target(&TargetId::new("OTHER"));
        registry.register_other(TargetId::new("OTHER"));
    }
    release.send(()).unwrap();
    let arm = channel_command(&mut outbound).await;
    channel_reply(&inbound, json!({"id": arm["id"], "error": {"code": -32001}})).await;
    assert_closed(controller).await;
}

#[tokio::test]
async fn a_recursive_arm_without_independent_context_lineage_or_exact_error_still_fails_closed() {
    for case in [
        "missing_context",
        "opener",
        "opener_frame",
        "parent_frame",
        "other_error",
        "owned_context",
    ] {
        let page = watched("ROOT");
        let mut state = shared_with(&page, "OTHER");
        state.origin = BrowserOrigin::Launched;
        let shared = Arc::new(state);
        let owned = register_owned_context(&shared, &page);
        let (mut socket, _commands, controller) =
            test_child_controller(Arc::clone(&shared), Duration::from_secs(1)).await;
        let mut info = json!({"targetId": "OTHER", "type": "worker", "title": "", "url": "", "attached": true,
            "canAccessOpener": false, "browserContextId": "DEFAULT"});
        match case {
            "missing_context" => {
                info.as_object_mut().unwrap().remove("browserContextId");
            }
            "opener" => info["openerId"] = json!("UNKNOWN"),
            "opener_frame" => info["openerFrameId"] = json!("UNKNOWN"),
            "parent_frame" => info["parentFrameId"] = json!("UNKNOWN"),
            "owned_context" => info["browserContextId"] = json!(owned.inner()),
            _ => {}
        }
        reply(
            &mut socket,
            json!({"method": "Target.attachedToTarget", "params": {
                "sessionId": "S-UI", "waitingForDebugger": false, "targetInfo": info
            }}),
        )
        .await;
        let arm = next_child_message(&mut socket).await;
        assert_eq!(arm["method"], "Target.setAutoAttach", "case {case}");
        let code = if case == "other_error" { -32000 } else { -32001 };
        reply(&mut socket, json!({"id": arm["id"], "error": {"code": code}})).await;
        assert_closed(controller).await;
    }
}
