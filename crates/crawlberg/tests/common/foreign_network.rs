use chromiumoxide::cdp::browser_protocol::network::{
    BlockPattern, BlockedReason, EventLoadingFailed, ResourceType, SetBlockedUrLsParams,
};
use futures::StreamExt as _;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, PartialEq)]
pub struct Failure {
    error: &'static str,
    blocked: Option<BlockedReason>,
    canceled: Option<bool>,
    resource: ResourceType,
}

fn categorize(event: &EventLoadingFailed) -> Failure {
    let error = match event.error_text.as_str() {
        "net::ERR_BLOCKED_BY_CLIENT" => "blocked_by_client",
        "" => "empty",
        "net::ERR_ABORTED" => "aborted",
        "net::ERR_CONNECTION_REFUSED" => "connection_refused",
        "net::ERR_CONNECTION_RESET" => "connection_reset",
        "net::ERR_CONNECTION_CLOSED" => "connection_closed",
        "net::ERR_NAME_NOT_RESOLVED" => "name_not_resolved",
        "net::ERR_FAILED" => "failed",
        "net::ERR_TIMED_OUT" => "timed_out",
        _ => "other",
    };
    Failure {
        error,
        blocked: event.blocked_reason.clone(),
        canceled: event.canceled,
        resource: event.r#type.clone(),
    }
}

#[test]
fn should_record_static_network_failure_categories_without_private_payload() {
    let event: EventLoadingFailed = serde_json::from_value(serde_json::json!({
        "requestId": "private-request-marker", "timestamp": 1.0, "type": "Fetch",
        "errorText": "net::ERR_BLOCKED_BY_CLIENT", "blockedReason": "inspector", "canceled": false
    }))
    .expect("typed network event");
    let summary = categorize(&event);
    assert_eq!(
        summary,
        Failure {
            error: "blocked_by_client",
            blocked: Some(BlockedReason::Inspector),
            canceled: Some(false),
            resource: ResourceType::Fetch
        }
    );
    assert!(!format!("{summary:?}").contains("private-request-marker"));
}

#[test]
fn should_collapse_unknown_error_text_without_exposing_private_payload() {
    let event: EventLoadingFailed = serde_json::from_value(serde_json::json!({
        "requestId": "private-request-marker", "timestamp": 1.0, "type": "Document",
        "errorText": "https://private-url-marker/?token=private-token-marker"
    }))
    .expect("typed network event");
    let summary = categorize(&event);
    assert_eq!(
        summary,
        Failure {
            error: "other",
            blocked: None,
            canceled: None,
            resource: ResourceType::Document
        }
    );
    for secret in ["private-request-marker", "private-url-marker", "private-token-marker"] {
        assert!(!format!("{summary:?}").contains(secret));
    }
}

const MAX_FAILURES: usize = 16;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub failures: Vec<Failure>,
    pub omitted: usize,
}

impl Snapshot {
    fn record(&mut self, event: &EventLoadingFailed) {
        if self.failures.len() < MAX_FAILURES {
            self.failures.push(categorize(event));
        } else {
            self.omitted = self.omitted.saturating_add(1);
        }
    }
}

#[test]
fn should_bound_retained_network_failures_and_count_omitted_events() {
    let event: EventLoadingFailed = serde_json::from_value(serde_json::json!({
        "requestId": "private-request-marker", "timestamp": 1.0, "type": "Fetch",
        "errorText": "net::ERR_BLOCKED_BY_CLIENT"
    }))
    .expect("typed network event");
    let mut snapshot = Snapshot::default();
    for _ in 0..MAX_FAILURES + 3 {
        snapshot.record(&event);
    }
    assert_eq!(snapshot.failures.len(), MAX_FAILURES);
    assert_eq!(snapshot.omitted, 3);
}

pub struct Observer {
    failures: Arc<Mutex<Snapshot>>,
    task: tokio::task::JoinHandle<()>,
}

impl Observer {
    pub async fn start(page: &chromiumoxide::Page) -> Result<Self, chromiumoxide::error::CdpError> {
        let mut events = page.event_listener::<EventLoadingFailed>().await?;
        page.execute(chromiumoxide::cdp::browser_protocol::network::EnableParams::default())
            .await?;
        let failures = Arc::new(Mutex::new(Snapshot::default()));
        let recorded = Arc::clone(&failures);
        let task = tokio::spawn(async move {
            while let Some(event) = events.next().await {
                recorded.lock().expect("observer lock").record(&event);
            }
        });
        Ok(Self { failures, task })
    }

    pub fn snapshot(&self) -> Snapshot {
        self.failures.lock().expect("observer lock").clone()
    }
}

impl Drop for Observer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Debug, PartialEq)]
pub enum ShutdownOutcome {
    Closed,
    Killed,
}

pub async fn shutdown_owned_browser(
    browser: &mut chromiumoxide::Browser,
    handler: tokio::task::JoinHandle<()>,
) -> Result<ShutdownOutcome, &'static str> {
    let deadline = std::time::Duration::from_secs(2);
    let closed = matches!(tokio::time::timeout(deadline, browser.close()).await, Ok(Ok(_)));
    let exited = closed && matches!(tokio::time::timeout(deadline, browser.wait()).await, Ok(Ok(_)));
    let result = if exited {
        Ok(ShutdownOutcome::Closed)
    } else if matches!(tokio::time::timeout(deadline, browser.kill()).await, Ok(Some(Ok(())))) {
        Ok(ShutdownOutcome::Killed)
    } else {
        Err("owned browser shutdown did not complete")
    };
    handler.abort();
    result
}

#[tokio::test]
async fn should_capture_an_actual_chrome_blocked_request() {
    let test_name = "should_capture_an_actual_chrome_blocked_request";
    let profile = std::env::temp_dir().join(format!("crawlberg-{test_name}-{}", std::process::id()));
    let Some(config) = super::common::expect_chrome_or_skip(
        test_name,
        chromiumoxide::browser::BrowserConfig::builder()
            .no_sandbox()
            .new_headless_mode()
            .user_data_dir(&profile)
            .build(),
    ) else {
        return;
    };
    let Some((mut browser, handler)) =
        super::common::expect_chrome_or_skip(test_name, chromiumoxide::Browser::launch(config).await)
    else {
        return;
    };
    let handler_task = super::common::spawn_handler(handler);
    let page = browser.new_page("about:blank").await.expect("controlled page");
    let observer = Observer::start(&page).await.expect("network observer");
    let url = "https://network-observer.invalid/controlled";
    page.execute(
        SetBlockedUrLsParams::builder()
            .url_pattern(BlockPattern::new(url, true))
            .build(),
    )
    .await
    .expect("block owned control request");
    let result = page
        .evaluate(format!(
            "fetch({url:?}, {{mode:'no-cors'}}).then(() => 'resolved', () => 'rejected')"
        ))
        .await;
    for _ in 0..50 {
        if !observer.snapshot().failures.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let failures = observer.snapshot().failures;
    drop(observer);
    // ~keep Exercise owned-process kill fallback when the CDP driver is no longer available.
    handler_task.abort();
    let shutdown = shutdown_owned_browser(&mut browser, handler_task).await;
    let _ = std::fs::remove_dir_all(profile);
    assert_eq!(shutdown.expect("owned browser cleanup"), ShutdownOutcome::Killed);
    assert_eq!(
        result
            .expect("evaluate fetch")
            .into_value::<String>()
            .expect("fetch outcome"),
        "rejected"
    );
    assert_eq!(
        failures,
        vec![Failure {
            error: "empty",
            blocked: Some(BlockedReason::Inspector),
            canceled: Some(false),
            resource: ResourceType::Fetch
        }]
    );
}
