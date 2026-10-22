use super::*;
use async_tungstenite::tungstenite::Message;
use serde_json::{Value, json};

async fn reply(socket: &mut TestChildSocket, value: Value) {
    socket.send(Message::Text(value.to_string().into())).await.unwrap();
}

async fn nested(socket: &mut TestChildSocket, parent: &str, session: &str, target: &str, waiting: bool) {
    reply(
        socket,
        json!({"sessionId": parent, "method": "Target.attachedToTarget", "params": {
            "sessionId": session, "waitingForDebugger": waiting,
            "targetInfo": {"targetId": target, "type": "worker", "title": "", "url": "",
                "attached": true, "canAccessOpener": false, "browserContextId": "DEFAULT"}
        }}),
    )
    .await;
}

async fn opener_linked(socket: &mut TestChildSocket, waiting: bool) {
    reply(
        socket,
        json!({"method": "Target.attachedToTarget", "params": {
            "sessionId": "S-DESCENDANT", "waitingForDebugger": waiting,
            "targetInfo": {"targetId": "DESCENDANT", "type": "worker", "title": "", "url": "",
                "attached": true, "canAccessOpener": false, "browserContextId": "DEFAULT", "openerId": "CHILD"}
        }}),
    )
    .await;
}

struct Fixture {
    shared: Arc<Shared>,
    page: Arc<WatchedPage>,
    socket: TestChildSocket,
    commands: mpsc::Sender<super::super::ChildCommand>,
    controller: tokio::task::JoinHandle<bool>,
}

impl Fixture {
    async fn pending() -> Self {
        let page = watched("ROOT");
        let mut state = shared_with(&page, "OTHER");
        state.origin = BrowserOrigin::Launched;
        let shared = Arc::new(state);
        register_owned_context(&shared, &page);
        let (mut socket, commands, controller) =
            test_child_controller(Arc::clone(&shared), Duration::from_secs(1)).await;
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
        let parent = next_child_message(&mut socket).await;
        assert_eq!(parent["method"], "Target.setAutoAttach");
        assert_eq!(parent["sessionId"], "S-TAB");
        answer_child_command(&mut socket, &parent).await;
        nested(&mut socket, "S-TAB", "S-CHILD", "CHILD", false).await;
        let child = next_child_message(&mut socket).await;
        assert_eq!(child["method"], "Target.setAutoAttach");
        assert_eq!(child["sessionId"], "S-CHILD");
        assert!(matches!(
            lock(&shared.registry).ownership.get(&TargetId::new("CHILD")),
            Some(TargetOwnership::Other)
        ));
        Self {
            shared,
            page,
            socket,
            commands,
            controller,
        }
    }

    async fn detach(&mut self) {
        reply(
            &mut self.socket,
            json!({"method": "Target.detachedFromTarget", "params": {
                "sessionId": "S-TAB", "targetId": "TAB"
            }}),
        )
        .await;
    }

    async fn census(&mut self) -> Value {
        let census = next_child_message(&mut self.socket).await;
        assert_eq!(census["method"], "Target.getTargets");
        assert_eq!(census["params"], json!({"filter": [{}]}));
        census
    }

    async fn refused(mut self, case: &str) {
        let message = tokio::time::timeout(Duration::from_secs(3), self.socket.next())
            .await
            .unwrap_or_else(|error| panic!("{case}: rejection must close socket: {error}"));
        if let Some(Ok(message)) = message {
            assert!(
                !message.is_text(),
                "{case}: no command or resume may escape: {message:?}"
            );
        }
        assert!(
            !tokio::time::timeout(Duration::from_secs(3), self.controller)
                .await
                .unwrap_or_else(|error| panic!("{case}: controller must fail closed: {error}"))
                .unwrap()
        );
    }
}

fn mutate(fixture: &Fixture, case: &str) -> Option<Arc<()>> {
    let mut registry = lock(&fixture.shared.registry);
    let child = TargetId::new("CHILD");
    match case {
        "context_reuse" => {
            let context = BrowserContextId::new("DEFAULT");
            let original = registry.unowned_context_observers[&context].upgrade().unwrap();
            registry.register_context(context.clone(), None);
            registry.contexts.remove(&context);
            let replacement = registry.observe_unowned_context(&context).unwrap();
            assert!(!Arc::ptr_eq(&original, &replacement));
            assert!(!registry.unowned_context_matches(&context, &original));
            return Some(replacement);
        }
        "owned_child" | "owned_root" => registry.register_watched(child, Arc::clone(&fixture.page)),
        "child_replace" => {
            registry.register_watched(child.clone(), Arc::clone(&fixture.page));
            registry.remove_target(&child);
            registry.register_other(child.clone());
            assert!(matches!(registry.ownership.get(&child), Some(TargetOwnership::Other)));
        }
        "parent_claim_remove" => {
            let parent = TargetId::new("TAB");
            registry.register_other(parent.clone());
            registry.remove_target(&parent);
            assert!(!registry.ownership.contains_key(&parent));
            assert!(!registry.target_generations.contains_key(&parent));
        }
        _ => unreachable!("mutation {case}"),
    }
    None
}

async fn owned_waiter(fixture: &Fixture) -> oneshot::Receiver<Result<(), String>> {
    let (done, mut ready) = oneshot::channel();
    fixture
        .commands
        .send(super::super::ChildCommand::Arm {
            root: TargetId::new("CHILD"),
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
    ready
}

async fn settled_descendant(fixture: &mut Fixture, opener: bool) {
    if opener {
        opener_linked(&mut fixture.socket, true).await;
    } else {
        nested(&mut fixture.socket, "S-CHILD", "S-DESCENDANT", "DESCENDANT", true).await;
    }
    let arm = next_child_message(&mut fixture.socket).await;
    assert_eq!(arm["sessionId"], "S-DESCENDANT");
    answer_child_command(&mut fixture.socket, &arm).await;
    let resume = next_child_message(&mut fixture.socket).await;
    assert_eq!(resume["method"], "Runtime.runIfWaitingForDebugger");
    assert_eq!(resume["sessionId"], "S-DESCENDANT");
    answer_child_command(&mut fixture.socket, &resume).await;
}

#[tokio::test]
async fn an_authorized_tab_drain_rejects_changed_identity_owned_work_and_ambiguous_descendants() {
    let cases = [
        (true, "context_reuse"),
        (true, "owned_child"),
        (true, "owned_root"),
        (true, "settled_descendant"),
        (false, "context_reuse"),
        (false, "child_replace"),
        (false, "parent_claim_remove"),
        (false, "new_descendant"),
        (true, "settled_opener_descendant"),
        (false, "new_opener_descendant"),
        (false, "duplicate_child"),
    ];
    assert_eq!(cases.len(), 11);
    for (before_detach, case) in cases {
        let mut fixture = Fixture::pending().await;
        let census = if before_detach {
            None
        } else {
            fixture.detach().await;
            Some(fixture.census().await)
        };
        let mut retained_context = None;
        let mut retained_waiter = None;
        match case {
            "settled_descendant" => settled_descendant(&mut fixture, false).await,
            "settled_opener_descendant" => settled_descendant(&mut fixture, true).await,
            "new_descendant" => nested(&mut fixture.socket, "UNKNOWN-PARENT", "S-NEW", "NEW", false).await,
            "new_opener_descendant" => opener_linked(&mut fixture.socket, false).await,
            "duplicate_child" => {
                send_attached_target_with_waiting(
                    &mut fixture.socket,
                    "S-CHILD",
                    "CHILD",
                    "worker",
                    None,
                    &BrowserContextId::new("DEFAULT"),
                    true,
                )
                .await;
            }
            _ => {
                retained_context = mutate(&fixture, case);
                if case == "owned_root" {
                    retained_waiter = Some(owned_waiter(&fixture).await);
                }
            }
        }
        if before_detach {
            fixture.detach().await;
        } else if !matches!(case, "new_descendant" | "new_opener_descendant" | "duplicate_child") {
            reply(
                &mut fixture.socket,
                json!({"id": census.unwrap()["id"], "result": {"targetInfos": []}}),
            )
            .await;
        }
        fixture.refused(case).await;
        drop(retained_context);
        drop(retained_waiter);
    }
}
