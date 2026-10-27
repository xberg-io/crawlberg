use super::super::{
    CHILD_CONTROLLER_PENDING_LIMIT, ChildPending, ChildPendingKind, Registry, RootWaiter, StructuralTarget,
    UnownedArmProof, adopt_target, child_pending, reconcile_destroyed, unowned_arm_is_independent,
    unowned_arm_registry_matches, unowned_arm_retirement_registry_matches,
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
        let response = tokio::time::timeout(Duration::from_secs(1), fixture.socket.next())
            .await
            .expect("identity reuse must fail before arming a replacement");
        if let Some(Ok(message)) = response {
            assert!(
                !message.is_text(),
                "identity reuse must not issue a command: {message:?}"
            );
        }
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

#[tokio::test]
async fn an_unrelated_context_claim_preserves_a_recursive_absence_proof() {
    for before_error in [true, false] {
        let page = watched("ROOT");
        let mut state = shared_with(&page, "OTHER");
        state.origin = BrowserOrigin::Launched;
        let shared = Arc::new(state);
        let owned = register_owned_context(&shared, &page);
        let (mut socket, commands, controller) =
            test_child_controller(Arc::clone(&shared), Duration::from_secs(1)).await;
        send_attached_target_with_waiting(
            &mut socket,
            "S-OTHER",
            "OTHER",
            "worker",
            None,
            &BrowserContextId::new("DEFAULT"),
            false,
        )
        .await;
        let arm = next_child_message(&mut socket).await;
        assert_eq!(arm["method"], "Target.setAutoAttach");
        if before_error {
            lock(&shared.registry).register_context(BrowserContextId::new("UNRELATED-CONTEXT"), None);
        }
        reply(&mut socket, json!({"id": arm["id"], "error": {"code": -32001}})).await;
        let census = next_child_message(&mut socket).await;
        assert_eq!(census["method"], "Target.getTargets");
        assert_eq!(census["params"], json!({"filter": [{}]}));
        if !before_error {
            lock(&shared.registry).register_context(BrowserContextId::new("UNRELATED-CONTEXT"), None);
        }
        {
            let registry = lock(&shared.registry);
            assert_eq!(registry.contexts.len(), 2);
            assert!(!registry.contexts.contains_key(&BrowserContextId::new("DEFAULT")));
            assert!(!registry.context_targets.contains_key(&TargetId::new("OTHER")));
            assert!(!registry.opened.contains_key(&TargetId::new("OTHER")));
            assert!(registry.others.contains(&TargetId::new("OTHER")));
        }
        reply(&mut socket, json!({"id": census["id"], "result": {"targetInfos": []}})).await;
        send_attached_target_with_waiting(&mut socket, "S-ROOT", "ROOT", "page", None, &owned, false).await;
        let root_arm = next_child_message(&mut socket).await;
        assert_eq!(root_arm["sessionId"], "S-ROOT");
        answer_child_command(&mut socket, &root_arm).await;
        commands.send(super::super::ChildCommand::Stop).await.unwrap();
        assert!(controller.await.unwrap());
    }
}

async fn ended_session_census_fixture(event: &str, waiting: bool, before_error: bool) -> CensusFixture {
    let page = watched("ROOT");
    let mut state = shared_with(&page, "OTHER");
    state.origin = BrowserOrigin::Launched;
    let shared = Arc::new(state);
    register_owned_context(&shared, &page);
    let context = BrowserContextId::new("DEFAULT");
    let target = TargetId::new("OTHER");
    let (mut socket, commands, controller) = test_child_controller(Arc::clone(&shared), Duration::from_secs(1)).await;
    send_attached_target_with_waiting(&mut socket, "S-UI", "OTHER", "browser_ui", None, &context, waiting).await;
    let arm = next_child_message(&mut socket).await;
    assert_eq!(arm["method"], "Target.setAutoAttach");
    assert_eq!(arm["sessionId"], "S-UI");
    let generation = lock(&shared.registry).target_generations[&target].clone();
    let params = if event == "Target.detachedFromTarget" {
        json!({"sessionId": "S-UI", "targetId": "OTHER"})
    } else {
        assert_eq!(event, "Target.targetDestroyed");
        json!({"targetId": "OTHER"})
    };
    if before_error {
        reply(&mut socket, json!({"method": event, "params": params.clone()})).await;
    }
    reply(&mut socket, json!({"id": arm["id"], "error": {"code": -32001}})).await;
    let census = next_child_message(&mut socket).await;
    assert_eq!(census["method"], "Target.getTargets");
    assert_eq!(census["params"], json!({"filter": [{}]}));
    assert!(census.get("sessionId").is_none());
    assert_ne!(census["id"], arm["id"]);
    if !before_error {
        reply(&mut socket, json!({"method": event, "params": params})).await;
    }
    {
        let registry = lock(&shared.registry);
        assert!(Arc::ptr_eq(&registry.target_generations[&target], &generation));
        assert!(matches!(registry.ownership.get(&target), Some(TargetOwnership::Other)));
        assert!(!registry.contexts.contains_key(&context));
        assert!(!registry.retired_contexts.contains(&context));
    }
    CensusFixture {
        shared,
        page,
        socket,
        _commands: commands,
        controller,
        census,
    }
}

#[tokio::test]
async fn an_ended_session_still_requires_absence_before_its_original_pending_arm_can_retire() {
    let mut executed = 0;
    for event in ["Target.detachedFromTarget", "Target.targetDestroyed"] {
        for waiting in [false, true] {
            for (live, before_error) in [(false, true), (true, true), (false, false), (true, false)] {
                let mut fixture = ended_session_census_fixture(event, waiting, before_error).await;
                let targets = if live {
                    vec![
                        json!({"targetId": "OTHER", "type": "browser_ui", "title": "", "url": "",
                        "attached": false, "canAccessOpener": false, "browserContextId": "DEFAULT"}),
                    ]
                } else {
                    Vec::new()
                };
                reply(
                    &mut fixture.socket,
                    json!({"id": fixture.census["id"], "result": {"targetInfos": targets}}),
                )
                .await;
                if live {
                    let response = tokio::time::timeout(Duration::from_secs(1), fixture.socket.next())
                        .await
                        .expect("live ended target rejection must close the socket promptly");
                    if let Some(Ok(message)) = response {
                        assert!(
                            !message.is_text(),
                            "a live ended target must not be resumed: {message:?}"
                        );
                    }
                    assert_closed(fixture.controller).await;
                } else {
                    let owned = lock(&fixture.shared.registry).context_targets[&fixture.page.root].clone();
                    send_attached_target_with_waiting(
                        &mut fixture.socket,
                        "S-ROOT",
                        "ROOT",
                        "page",
                        None,
                        &owned,
                        false,
                    )
                    .await;
                    let root_arm = next_child_message(&mut fixture.socket).await;
                    assert_eq!(root_arm["method"], "Target.setAutoAttach");
                    assert_eq!(root_arm["sessionId"], "S-ROOT");
                    answer_child_command(&mut fixture.socket, &root_arm).await;
                    fixture._commands.send(super::super::ChildCommand::Stop).await.unwrap();
                    assert!(
                        tokio::time::timeout(Duration::from_secs(1), fixture.controller)
                            .await
                            .expect("absence-proved controller must stop promptly")
                            .unwrap()
                    );
                }
                executed += 1;
            }
        }
    }
    assert_eq!(executed, 16);
}

async fn end_unowned_child_session(fixture: &mut CensusFixture, detach_first: bool) {
    let events = if detach_first {
        ["Target.detachedFromTarget", "Target.targetDestroyed"]
    } else {
        ["Target.targetDestroyed", "Target.detachedFromTarget"]
    };
    for event in events {
        let params = if event == "Target.detachedFromTarget" {
            json!({"sessionId": "S-UI", "targetId": "OTHER"})
        } else {
            json!({"targetId": "OTHER"})
        };
        reply(&mut fixture.socket, json!({"method": event, "params": params})).await;
    }
}

async fn assert_owned_root_can_arm(mut fixture: CensusFixture) {
    let (done, armed) = oneshot::channel();
    fixture
        ._commands
        .send(super::super::ChildCommand::Arm {
            root: fixture.page.root.clone(),
            done,
        })
        .await
        .unwrap();
    let owned = lock(&fixture.shared.registry).context_targets[&fixture.page.root].clone();
    send_attached_target_with_waiting(&mut fixture.socket, "S-ROOT", "ROOT", "page", None, &owned, false).await;
    let root_arm = next_child_message(&mut fixture.socket).await;
    assert_eq!(root_arm["method"], "Target.setAutoAttach");
    assert_eq!(root_arm["sessionId"], "S-ROOT");
    answer_child_command(&mut fixture.socket, &root_arm).await;
    tokio::time::timeout(Duration::from_secs(1), armed)
        .await
        .expect("owned root arm acknowledgement")
        .expect("root arm waiter")
        .expect("owned root stays armed");
    fixture._commands.send(super::super::ChildCommand::Stop).await.unwrap();
    assert!(
        fixture
            .controller
            .await
            .expect("absence-proved controller must stay healthy")
    );
}

#[tokio::test]
async fn authoritative_primary_removal_preserves_a_pending_unowned_absence_census() {
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::ERROR)
        .with_test_writer()
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let mut executed = 0;
    for primary_first in [false, true] {
        for detach_first in [false, true] {
            let mut fixture = census_fixture().await;
            let target = TargetId::new("OTHER");
            if primary_first {
                assert!(reconcile_destroyed(&mut lock(&fixture.shared.registry), &target, false));
            }
            end_unowned_child_session(&mut fixture, detach_first).await;
            if !primary_first {
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
                assert!(reconcile_destroyed(&mut lock(&fixture.shared.registry), &target, false));
            }
            {
                let registry = lock(&fixture.shared.registry);
                assert!(!registry.ownership.contains_key(&target));
                assert!(!registry.target_generations.contains_key(&target));
            }
            reply(
                &mut fixture.socket,
                json!({"id": fixture.census["id"], "result": {"targetInfos": []}}),
            )
            .await;
            assert_owned_root_can_arm(fixture).await;
            executed += 1;
        }
    }
    assert_eq!(executed, 4);
}

#[tokio::test]
async fn primary_retirement_history_cannot_outlive_any_intervening_owner_or_context_claim() {
    let mut executed = 0;
    for change in 0..7 {
        let mut fixture = census_fixture().await;
        let target = TargetId::new("OTHER");
        let context = BrowserContextId::new("DEFAULT");
        {
            let mut registry = lock(&fixture.shared.registry);
            assert!(reconcile_destroyed(&mut registry, &target, false));
            match change {
                0 => registry.register_watched(target.clone(), Arc::clone(&fixture.page)),
                1 => registry.register_pending(target.clone(), context.clone()),
                2 => registry.register_pending_shared(target.clone()),
                3 => registry.register_quarantined(target.clone(), Some(context.clone())),
                4 => registry.register_other(target.clone()),
                5 => {
                    registry.register_context(context.clone(), None);
                    registry.contexts.remove(&context);
                }
                6 => registry.remove_target(&target),
                _ => unreachable!(),
            }
            if change != 5 {
                assert!(
                    !registry.unowned_arm_observers.contains_key(&target),
                    "registration case {change}"
                );
                assert!(reconcile_destroyed(&mut registry, &target, false));
            }
        }
        reply(
            &mut fixture.socket,
            json!({"id": fixture.census["id"], "result": {"targetInfos": []}}),
        )
        .await;
        assert_closed(fixture.controller).await;
        executed += 1;
    }
    assert_eq!(executed, 7);
}

#[tokio::test]
async fn direct_iframe_adoption_invalidates_primary_retirement_history() {
    let mut fixture = census_fixture().await;
    let target = TargetId::new("OTHER");
    assert!(reconcile_destroyed(&mut lock(&fixture.shared.registry), &target, false));
    let context = lock(&fixture.shared.registry).context_targets[&fixture.page.root].clone();
    let info = serde_json::from_value(json!({"targetId": "OTHER", "type": "iframe", "title": "", "url": "",
        "attached": true, "canAccessOpener": false, "browserContextId": context.inner()}))
    .unwrap();
    assert!(adopt_target(&fixture.shared, &info).is_none());
    {
        let mut registry = lock(&fixture.shared.registry);
        assert!(matches!(
            registry.ownership.get(&target),
            Some(TargetOwnership::Watched(_))
        ));
        assert!(!registry.unowned_arm_observers.contains_key(&target));
        assert!(reconcile_destroyed(&mut registry, &target, false));
    }
    reply(
        &mut fixture.socket,
        json!({"id": fixture.census["id"], "result": {"targetInfos": []}}),
    )
    .await;
    assert_closed(fixture.controller).await;
}

#[tokio::test]
async fn a_primary_tab_replacement_invalidates_retirement_history_without_an_ownership_entry() {
    let mut fixture = census_fixture().await;
    let target = TargetId::new("OTHER");
    assert!(reconcile_destroyed(&mut lock(&fixture.shared.registry), &target, false));
    let info = serde_json::from_value(json!({"targetId": "OTHER", "type": "tab", "title": "", "url": "",
        "attached": true, "canAccessOpener": false, "browserContextId": "DEFAULT"}))
    .unwrap();
    assert!(adopt_target(&fixture.shared, &info).is_none());
    assert!(
        !lock(&fixture.shared.registry)
            .unowned_arm_observers
            .contains_key(&target)
    );
    reply(
        &mut fixture.socket,
        json!({"id": fixture.census["id"], "result": {"targetInfos": []}}),
    )
    .await;
    assert_closed(fixture.controller).await;
}

#[tokio::test]
async fn primary_retirement_still_requires_a_fresh_census_proving_target_absence() {
    let mut fixture = census_fixture().await;
    assert!(reconcile_destroyed(
        &mut lock(&fixture.shared.registry),
        &TargetId::new("OTHER"),
        false
    ));
    reply(
        &mut fixture.socket,
        json!({"id": fixture.census["id"], "result": {"targetInfos": [{
            "targetId": "OTHER", "type": "browser_ui", "title": "", "url": "", "attached": false,
            "canAccessOpener": false, "browserContextId": "DEFAULT"
        }]}}),
    )
    .await;
    assert_closed(fixture.controller).await;
}

#[test]
fn retirement_observers_bound_live_proofs_and_reclaim_dead_entries() {
    let mut registry = Registry::default();
    let generation = Arc::new(());
    let mut tokens = Vec::new();
    for index in 0..CHILD_CONTROLLER_PENDING_LIMIT {
        tokens.push(
            registry
                .observe_unowned_arm(&TargetId::new(index.to_string()), &generation)
                .expect("proof below capacity"),
        );
    }
    assert_eq!(registry.unowned_arm_observers.len(), CHILD_CONTROLLER_PENDING_LIMIT);
    assert!(
        registry
            .observe_unowned_arm(&TargetId::new("OVERFLOW"), &generation)
            .is_none()
    );
    tokens.clear();
    assert!(
        registry
            .observe_unowned_arm(&TargetId::new("RECLAIMED"), &generation)
            .is_some()
    );
    assert_eq!(registry.unowned_arm_observers.len(), 1);
}

#[tokio::test]
async fn rejected_retirement_diagnostics_identify_the_failed_predicate_without_target_payloads() {
    use tracing::instrument::WithSubscriber as _;
    let captured = tempfile::NamedTempFile::new().unwrap();
    let writer = captured.reopen().unwrap();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .without_time()
        .with_max_level(tracing::Level::WARN)
        .with_writer(move || writer.try_clone().unwrap())
        .finish();
    async {
        let mut fixture = census_fixture().await;
        assert!(reconcile_destroyed(
            &mut lock(&fixture.shared.registry),
            &TargetId::new("OTHER"),
            false
        ));
        lock(&fixture.shared.registry).register_context(BrowserContextId::new("DEFAULT"), None);
        reply(
            &mut fixture.socket,
            json!({"id": fixture.census["id"], "result": {"targetInfos": []}}),
        )
        .await;
        assert_closed(fixture.controller).await;
    }
    .with_subscriber(subscriber)
    .await;
    let text = std::fs::read_to_string(captured.path()).unwrap();
    let events: Vec<Value> = text.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0]["fields"],
        json!({
            "message": "browser child-target controller failed closed", "phase": "child_target_controller",
            "reason": "unowned_retirement_unproved", "browser_handler_task_id": "None", "acknowledged": true,
            "launched": true, "target_absent": true, "registry_matches": false, "context_draining": false,
            "independent": true, "ownership_present": false, "generation_present": false
        })
    );
    assert!(!text.contains("OTHER"));
    assert!(!text.contains("DEFAULT"));
}

async fn primary_removed_before_error_fixture(waiting: bool) -> CensusFixture {
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
        waiting,
    )
    .await;
    let arm = next_child_message(&mut socket).await;
    assert_eq!(arm["sessionId"], "S-UI");
    assert!(reconcile_destroyed(
        &mut lock(&shared.registry),
        &TargetId::new("OTHER"),
        false
    ));
    reply(
        &mut socket,
        json!({"method": "Target.detachedFromTarget", "params": {
            "sessionId": "S-UI", "targetId": "OTHER"
        }}),
    )
    .await;
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

#[tokio::test]
async fn primary_removal_before_the_recursive_arm_error_keeps_only_the_original_absence_proof() {
    let mut executed = 0;
    for waiting in [false, true] {
        let mut fixture = primary_removed_before_error_fixture(waiting).await;
        assert!(reconcile_destroyed(
            &mut lock(&fixture.shared.registry),
            &TargetId::new("OTHER"),
            false
        ));
        reply(
            &mut fixture.socket,
            json!({"id": fixture.census["id"], "result": {"targetInfos": []}}),
        )
        .await;
        assert_owned_root_can_arm(fixture).await;
        executed += 1;
    }
    assert_eq!(executed, 2);
}

#[tokio::test]
async fn historical_retirement_never_authorizes_a_live_resume_or_an_invalid_session() {
    let mut fixture = census_fixture().await;
    let target = TargetId::new("OTHER");
    let context = BrowserContextId::new("DEFAULT");
    {
        let mut registry = lock(&fixture.shared.registry);
        let proof = UnownedArmProof {
            target_generation: Arc::clone(&registry.target_generations[&target]),
            retirement_observer: registry.unowned_arm_observers[&target].proof.upgrade().unwrap(),
            context_generation: registry.unowned_context_observers[&context].upgrade().unwrap(),
            session_lifecycle: Arc::new(std::sync::atomic::AtomicU8::new(UnownedArmProof::ATTACHED)),
            parent_generation: None,
        };
        assert!(unowned_arm_registry_matches(&registry, Some(&context), &target, &proof));
        let observer = registry.unowned_arm_observers.remove(&target).unwrap();
        assert!(unowned_arm_registry_matches(&registry, Some(&context), &target, &proof));
        assert!(
            !unowned_arm_retirement_registry_matches(&registry, Some(&context), &target, &proof),
            "invalidating the observed generation must reject retirement even while Other remains"
        );
        registry.unowned_arm_observers.insert(target.clone(), observer);
        assert!(reconcile_destroyed(&mut registry, &target, false));
        assert!(!unowned_arm_registry_matches(
            &registry,
            Some(&context),
            &target,
            &proof
        ));
        assert!(unowned_arm_retirement_registry_matches(
            &registry,
            Some(&context),
            &target,
            &proof
        ));
        proof
            .session_lifecycle
            .store(UnownedArmProof::INVALID, Ordering::Release);
        assert!(!unowned_arm_retirement_registry_matches(
            &registry,
            Some(&context),
            &target,
            &proof
        ));
    }
    reply(
        &mut fixture.socket,
        json!({"id": fixture.census["id"], "result": {"targetInfos": []}}),
    )
    .await;
    fixture._commands.send(super::super::ChildCommand::Stop).await.unwrap();
    assert!(fixture.controller.await.unwrap());
}
