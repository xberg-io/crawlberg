use super::super::ChildCommand;
use super::*;

struct RetirementFixture {
    shared: Arc<Shared>,
    socket: TestChildSocket,
    parent_arm: Option<serde_json::Value>,
    child_arm: serde_json::Value,
    // ~keep Dropping the command sender would stop the controller before the CDP control runs.
    commands: mpsc::Sender<ChildCommand>,
    controller: tokio::task::JoinHandle<bool>,
}

async fn retirement_fixture(structural: bool) -> RetirementFixture {
    let page = watched("ROOT");
    let shared = Arc::new(shared_with(&page, "OTHER"));
    let foreign = BrowserContextId::new("FOREIGN");
    let (mut socket, commands, controller) =
        test_child_controller(Arc::clone(&shared), Duration::from_millis(250)).await;
    let parent_arm = if structural {
        send_attached_target_with_waiting(&mut socket, "S-TAB", "TAB", "tab", None, &foreign, false).await;
        Some(next_child_message(&mut socket).await)
    } else {
        None
    };
    send_attached_target_from_parent(
        &mut socket,
        "S-CHILD",
        "CHILD",
        "browser_ui",
        None,
        &foreign,
        false,
        structural.then_some("S-TAB"),
    )
    .await;
    let child_arm = next_child_message(&mut socket).await;
    assert_eq!(child_arm["method"], "Target.setAutoAttach");
    assert_eq!(child_arm["sessionId"], "S-CHILD");
    lock(&shared.registry).register_watched(TargetId::new("CHILD"), page);
    RetirementFixture {
        shared,
        socket,
        parent_arm,
        child_arm,
        commands,
        controller,
    }
}

async fn reject_child_arm(fixture: &mut RetirementFixture) {
    let error = serde_json::json!({ "id": fixture.child_arm["id"], "error": {
        "code": -32001, "message": "Session with given id not found." } });
    fixture
        .socket
        .send(async_tungstenite::tungstenite::Message::Text(error.to_string().into()))
        .await
        .expect("send owned child arm error");
}

async fn expect_rejection_without_census(mut fixture: RetirementFixture) {
    let next = tokio::time::timeout(Duration::from_secs(1), fixture.socket.next())
        .await
        .expect("the protected arm must close its CDP socket");
    assert!(
        !matches!(next, Some(Ok(async_tungstenite::tungstenite::Message::Text(_)))),
        "a protected arm must not issue a foreign census: {next:?}"
    );
    assert!(!fixture.controller.await.expect("protected child controller"));
}

#[tokio::test]
async fn a_promoted_arm_error_is_rejected_before_a_foreign_census() {
    let mut fixture = retirement_fixture(false).await;
    reject_child_arm(&mut fixture).await;
    expect_rejection_without_census(fixture).await;
}

enum ParentRetirement {
    Census,
    DestroyTab,
    DestroyChild,
    DetachTab,
}

async fn a_promoted_child_remains_pending_during_retirement(retirement: ParentRetirement) {
    let mut fixture = retirement_fixture(!matches!(retirement, ParentRetirement::DestroyChild)).await;
    let event = match retirement {
        ParentRetirement::Census => {
            let error = serde_json::json!({ "id": fixture.parent_arm.as_ref().expect("parent arm")["id"],
                "error": { "code": -32001, "message": "Session with given id not found." } });
            fixture
                .socket
                .send(async_tungstenite::tungstenite::Message::Text(error.to_string().into()))
                .await
                .expect("parent session error");
            let census = next_child_message(&mut fixture.socket).await;
            assert_eq!(census["method"], "Target.getTargets");
            serde_json::json!({ "id": census["id"], "result": { "targetInfos": [{
                "targetId": "CHILD", "type": "browser_ui", "title": "", "url": "chrome://privacy-sandbox-dialog/",
                "attached": true, "canAccessOpener": false, "browserContextId": "FOREIGN" }] } })
        }
        ParentRetirement::DestroyTab => serde_json::json!({
            "method": "Target.targetDestroyed", "params": { "targetId": "TAB" } }),
        ParentRetirement::DestroyChild => serde_json::json!({
            "method": "Target.targetDestroyed", "params": { "targetId": "CHILD" } }),
        ParentRetirement::DetachTab => serde_json::json!({
            "method": "Target.detachedFromTarget", "params": { "sessionId": "S-TAB", "targetId": "TAB" } }),
    };
    fixture
        .socket
        .send(async_tungstenite::tungstenite::Message::Text(event.to_string().into()))
        .await
        .expect("retire parent state");
    reject_child_arm(&mut fixture).await;
    expect_rejection_without_census(fixture).await;
}

#[tokio::test]
async fn a_foreign_census_keeps_a_promoted_childs_pending_arm() {
    a_promoted_child_remains_pending_during_retirement(ParentRetirement::Census).await;
}

#[tokio::test]
async fn destroying_a_foreign_tab_keeps_a_promoted_childs_pending_arm() {
    a_promoted_child_remains_pending_during_retirement(ParentRetirement::DestroyTab).await;
}

#[tokio::test]
async fn destroying_a_promoted_target_keeps_its_pending_arm() {
    a_promoted_child_remains_pending_during_retirement(ParentRetirement::DestroyChild).await;
}

#[tokio::test]
async fn detaching_a_foreign_tab_keeps_a_promoted_childs_pending_arm() {
    a_promoted_child_remains_pending_during_retirement(ParentRetirement::DetachTab).await;
}

#[tokio::test]
async fn a_root_waiter_keeps_its_child_arm_pending_after_parent_destruction() {
    let mut fixture = retirement_fixture(true).await;
    let (done, armed) = oneshot::channel();
    fixture
        .commands
        .send(ChildCommand::Arm {
            root: TargetId::new("CHILD"),
            done,
        })
        .await
        .expect("queue root waiter");
    let (done, enabled) = oneshot::channel();
    fixture
        .commands
        .send(ChildCommand::Enable { done })
        .await
        .expect("queue command barrier");
    enabled
        .await
        .expect("barrier sender")
        .expect("earlier root command applied");
    {
        let mut registry = lock(&fixture.shared.registry);
        registry.remove_target(&TargetId::new("CHILD"));
        assert!(!registry.ownership.contains_key(&TargetId::new("CHILD")));
    }
    let destroyed = serde_json::json!({
        "method": "Target.targetDestroyed", "params": { "targetId": "TAB" }
    });
    fixture
        .socket
        .send(async_tungstenite::tungstenite::Message::Text(
            destroyed.to_string().into(),
        ))
        .await
        .expect("destroy structural parent while its child's root waiter is pending");
    reject_child_arm(&mut fixture).await;
    let error = tokio::time::timeout(Duration::from_secs(1), armed)
        .await
        .expect("root waiter must receive its arm error")
        .expect("root waiter sender")
        .expect_err("a root arm error must fail its waiter");
    assert!(
        error.starts_with("failed to arm child-target interception for CHILD:"),
        "{error}"
    );
    expect_rejection_without_census(fixture).await;
}
