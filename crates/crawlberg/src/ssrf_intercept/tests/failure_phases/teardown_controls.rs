use super::*;

async fn reject_external_disable(socket: &mut TestChildSocket, mode: &str) -> Vec<String> {
    let mut seen = Vec::new();
    for expected in [
        "Target.setDiscoverTargets",
        "Target.getBrowserContexts",
        "Target.getTargets",
        "Fetch.disable",
    ] {
        let command = next_child_message(socket).await;
        assert_eq!(command["method"], expected);
        seen.push(expected.to_owned());
        let result = match expected {
            "Target.getBrowserContexts" => serde_json::json!({"browserContextIds": []}),
            "Target.getTargets" => serde_json::json!({"targetInfos": []}),
            _ => serde_json::json!({}),
        };
        if expected == "Fetch.disable" {
            match mode {
                "close" => {
                    socket.close(None).await.expect("close external transport");
                    return seen;
                }
                "stall" => {
                    tokio::time::sleep(super::super::super::CONTROLLER_TEARDOWN_TIMEOUT * 2).await;
                    return seen;
                }
                "reject" => {}
                _ => panic!("unknown control"),
            }
        }
        let reply = if expected == "Fetch.disable" {
            serde_json::json!({"id": command["id"], "error": {"code": -32000, "message": "controlled rejection"}})
        } else {
            serde_json::json!({"id": command["id"], "result": result})
        };
        socket
            .send(async_tungstenite::tungstenite::Message::Text(reply.to_string().into()))
            .await
            .expect("reply to controlled command");
    }
    seen
}

async fn external_stop_failure(mode: &'static str) -> (bool, bool, Vec<String>) {
    use super::super::super::{Browser, Command, EventTargetDestroyed, Events, fail_controller, serve};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("CDP listener");
    let address = listener.local_addr().expect("CDP address");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("CDP accept");
        let mut socket = async_tungstenite::tokio::accept_async(stream)
            .await
            .expect("WebSocket accept");
        let seen = reject_external_disable(&mut socket, mode).await;
        if mode == "reject" {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        seen
    });
    let (browser, handler) = Browser::connect(format!("ws://{address}"))
        .await
        .expect("external connect");
    assert!(
        !browser.begin_owned_termination(),
        "borrowed transport cannot arm owned intent"
    );
    let handler = crate::browser_pool::spawn_handler(handler);
    let events = Events {
        paused: browser
            .event_listener::<EventRequestPaused>()
            .await
            .expect("paused listener"),
        created: browser
            .event_listener::<EventTargetCreated>()
            .await
            .expect("created listener"),
        destroyed: browser
            .event_listener::<EventTargetDestroyed>()
            .await
            .expect("destroyed listener"),
    };
    let shared = Arc::new(shared_with(&watched("ROOT"), "OTHER"));
    let (commands, receiver) = mpsc::unbounded_channel();
    commands.send(Command::Stop(None)).expect("request stop");
    shared.stopping.store(true, Ordering::Release);
    let browser = Arc::new(browser);
    let ended = tokio::time::timeout(
        Duration::from_secs(5),
        serve(Arc::clone(&browser), Arc::clone(&shared), events, receiver),
    )
    .await
    .expect("bounded Stop flow");
    if !ended {
        fail_controller(&browser, &shared, "request-interception controller").await;
    }
    handler.abort();
    let seen = server.await.expect("controlled server");
    (ended, shared.failed.load(Ordering::Acquire), seen)
}

#[tokio::test]
async fn external_stop_keeps_disable_rejection_transport_loss_and_timeout_observable() {
    let _other = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    for mode in ["reject", "close", "stall"] {
        let ((ended, failed, seen), events) = captured_child_events(external_stop_failure(mode)).await;
        assert!(!ended, "{mode}: Stop must report failure");
        assert!(failed, "{mode}: security controller must remain failed closed");
        assert_eq!(
            seen,
            [
                "Target.setDiscoverTargets",
                "Target.getBrowserContexts",
                "Target.getTargets",
                "Fetch.disable"
            ]
        );
        assert_eq!(
            events
                .iter()
                .filter(
                    |event| event["fields"]["message"] == "browser security controller ended unexpectedly"
                        && event["level"] == "ERROR"
                )
                .count(),
            1,
            "{mode}: exact failure event"
        );
    }
}
