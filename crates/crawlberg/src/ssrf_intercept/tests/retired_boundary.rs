use super::super::ChildCommand;
use super::*;
use async_tungstenite::tungstenite::Message;
use serde_json::{Value, json};

struct Boundary {
    socket: TestChildSocket,
    commands: mpsc::Sender<ChildCommand>,
    controller: tokio::task::JoinHandle<bool>,
    shared: Arc<Shared>,
    page: Arc<WatchedPage>,
    context: BrowserContextId,
    resume: Value,
}

impl Boundary {
    async fn new(origin: BrowserOrigin) -> Self {
        let page = watched("ROOT");
        let mut state = shared_with(&page, "OTHER");
        state.origin = origin;
        let shared = Arc::new(state);
        let context = register_owned_context(&shared, &page);
        let (mut socket, commands, controller) =
            test_child_controller(Arc::clone(&shared), Duration::from_secs(1)).await;
        let (done, armed) = oneshot::channel();
        commands
            .send(ChildCommand::Arm {
                root: page.root.clone(),
                done,
            })
            .await
            .expect("arm root");
        let root_arm = begin_root_arm(&mut socket, &context).await;
        finish_root_arm(&mut socket, &root_arm, armed).await;
        send_attached_target(&mut socket, "S-CHILD", "CHILD", Some("ROOT"), &context).await;
        let arm = next_child_message(&mut socket).await;
        assert_eq!(arm["method"], "Target.setAutoAttach");
        answer_child_command(&mut socket, &arm).await;
        let resume = next_child_message(&mut socket).await;
        assert_eq!(resume["method"], "Runtime.runIfWaitingForDebugger");
        Self {
            socket,
            commands,
            controller,
            shared,
            page,
            context,
            resume,
        }
    }

    fn retire(&self) {
        begin_ending(&self.page);
        retire_child_test_context(&self.shared, &self.context);
    }

    async fn send(&mut self, value: Value) {
        self.socket
            .send(Message::Text(value.to_string().into()))
            .await
            .expect("send controlled CDP response");
    }

    async fn reject(&mut self, code: i64) {
        self.send(json!({"id": self.resume["id"], "error": {"code": code, "message": "controlled session error"}}))
            .await;
    }

    async fn drain(&mut self) -> (Value, oneshot::Receiver<()>) {
        let (done, drained) = oneshot::channel();
        self.commands
            .send(ChildCommand::Drain {
                context: self.context.clone(),
                done,
            })
            .await
            .expect("request drain");
        let census = next_child_message(&mut self.socket).await;
        assert_eq!(census["method"], "Target.getTargets");
        assert_eq!(census["params"]["filter"], json!([{}]));
        (census, drained)
    }

    async fn round_trip(&mut self) {
        self.socket
            .send(Message::Ping(vec![7, 6, 5].into()))
            .await
            .expect("send ordering barrier");
        let answer = tokio::time::timeout(Duration::from_secs(1), self.socket.next())
            .await
            .expect("controller must process the barrier")
            .expect("controller remains open")
            .expect("valid barrier response");
        assert_eq!(answer, Message::Pong(vec![7, 6, 5].into()));
        assert!(!self.controller.is_finished());
    }

    async fn assert_failed(self) {
        assert!(
            !tokio::time::timeout(Duration::from_secs(1), self.controller)
                .await
                .expect("unsafe response must end the controller")
                .expect("controller task")
        );
    }
}

#[tokio::test]
async fn disposed_context_boundary_error_waits_for_authoritative_drain_census() {
    let mut cases = 0;
    for origin in [BrowserOrigin::Launched, BrowserOrigin::Killed, BrowserOrigin::External] {
        let mut fixture = Boundary::new(origin).await;
        fixture.retire();
        let (census, mut drained) = fixture.drain().await;
        fixture.reject(-32001).await;
        fixture.round_trip().await;
        assert!(
            tokio::time::timeout(Duration::from_millis(25), &mut drained)
                .await
                .is_err(),
            "a session error must not acknowledge an unproved drain"
        );
        fixture
            .send(json!({"id": census["id"], "result": {"targetInfos": []}}))
            .await;
        tokio::time::timeout(Duration::from_secs(1), drained)
            .await
            .expect("proved drain must finish")
            .expect("drain acknowledged");
        fixture
            .commands
            .send(ChildCommand::Stop)
            .await
            .expect("stop healthy controller");
        assert!(fixture.controller.await.expect("controller task"));
        cases += 1;
    }
    assert_eq!(cases, 3);
}

#[tokio::test]
async fn acknowledged_disposal_boundary_error_waits_for_primary_retirement_and_child_census() {
    let mut cases = 0;
    for origin in [BrowserOrigin::Launched, BrowserOrigin::Killed, BrowserOrigin::External] {
        let mut fixture = Boundary::new(origin).await;
        begin_ending(&fixture.page);
        lock(&fixture.shared.registry)
            .contexts
            .get_mut(&fixture.context)
            .expect("registered owned context")
            .disposal_acknowledged = true;
        assert!(lock(&fixture.shared.registry).contexts.contains_key(&fixture.context));
        fixture.reject(-32001).await;
        fixture.round_trip().await;
        fixture.retire();
        let (census, mut drained) = fixture.drain().await;
        assert!(
            tokio::time::timeout(Duration::from_millis(25), &mut drained)
                .await
                .is_err(),
            "primary retirement cannot replace the independent child-target census"
        );
        fixture
            .send(json!({"id": census["id"], "result": {"targetInfos": []}}))
            .await;
        tokio::time::timeout(Duration::from_secs(1), drained)
            .await
            .expect("proved drain must finish")
            .expect("drain acknowledged");
        fixture
            .commands
            .send(ChildCommand::Stop)
            .await
            .expect("stop healthy controller");
        assert!(fixture.controller.await.expect("controller task"));
        cases += 1;
    }
    assert_eq!(cases, 3);
}

#[tokio::test]
async fn a_disposal_ack_without_primary_retirement_cannot_authorize_a_drain() {
    let mut fixture = Boundary::new(BrowserOrigin::Launched).await;
    begin_ending(&fixture.page);
    lock(&fixture.shared.registry)
        .contexts
        .get_mut(&fixture.context)
        .expect("registered owned context")
        .disposal_acknowledged = true;
    fixture.reject(-32001).await;
    let (done, drained) = oneshot::channel();
    let sent = fixture
        .commands
        .send(ChildCommand::Drain {
            context: fixture.context.clone(),
            done,
        })
        .await;
    fixture.assert_failed().await;
    assert!(
        sent.is_err() || drained.await.is_err(),
        "an unretired context cannot finish a drain"
    );
}

#[tokio::test]
async fn active_ending_unknown_contexts_and_other_errors_still_fail_closed() {
    let mut cases = 0;
    for state in ["active", "ending", "unknown", "other-error"] {
        let mut fixture = Boundary::new(BrowserOrigin::Launched).await;
        match state {
            "active" => {}
            "ending" => begin_ending(&fixture.page),
            "unknown" => {
                fixture.retire();
                lock(&fixture.shared.registry)
                    .retired_contexts
                    .retain(|context| context != &fixture.context);
            }
            "other-error" => fixture.retire(),
            _ => unreachable!(),
        }
        fixture
            .reject(if state == "other-error" { -32000 } else { -32001 })
            .await;
        fixture.assert_failed().await;
        cases += 1;
    }
    assert_eq!(cases, 4);
}

#[tokio::test]
async fn retired_boundary_errors_cannot_bypass_live_rejected_or_malformed_censuses() {
    let mut cases = 0;
    for mode in ["live", "rejected", "malformed"] {
        let mut fixture = Boundary::new(BrowserOrigin::Launched).await;
        fixture.retire();
        let (census, drained) = fixture.drain().await;
        fixture.reject(-32001).await;
        fixture.round_trip().await;
        let reply = match mode {
            "live" => {
                json!({"id": census["id"], "result": {"targetInfos": [{"targetId": "STILL-LIVE", "type": "worker",
                "title": "", "url": "about:blank", "attached": true, "canAccessOpener": false,
                "browserContextId": fixture.context}]}})
            }
            "rejected" => {
                json!({"id": census["id"], "error": {"code": -32000, "message": "controlled census rejection"}})
            }
            "malformed" => json!({"id": census["id"], "result": {"targetInfos": [{}]}}),
            _ => unreachable!(),
        };
        fixture.send(reply).await;
        fixture.assert_failed().await;
        assert!(drained.await.is_err(), "failed census cannot acknowledge disposal");
        cases += 1;
    }
    assert_eq!(cases, 3);
}

#[tokio::test]
async fn a_missing_census_stays_unacknowledged_and_socket_loss_still_fails_closed() {
    let mut fixture = Boundary::new(BrowserOrigin::Launched).await;
    fixture.retire();
    let (_census, mut drained) = fixture.drain().await;
    fixture.reject(-32001).await;
    fixture.round_trip().await;
    assert!(
        tokio::time::timeout(Duration::from_millis(25), &mut drained)
            .await
            .is_err()
    );
    fixture
        .socket
        .close(None)
        .await
        .expect("end controlled socket without census");
    fixture.assert_failed().await;
    assert!(
        drained.await.is_err(),
        "socket loss cannot acknowledge the pending drain"
    );
}
