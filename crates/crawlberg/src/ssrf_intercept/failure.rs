use std::sync::atomic::Ordering;

use super::{Shared, StructuralTarget};

pub(super) fn child_fail(shared: &Shared, reason: &'static str) -> bool {
    // ~keep A requested stop and a failure cascade already end under the existing stopping guard.
    if !shared.stopping.load(Ordering::Acquire) {
        tracing::error!(
            target: "crawlberg::ssrf_intercept",
            phase = "child_target_controller",
            reason,
            browser_handler_task_id = ?shared.handler_abort.as_ref().map(tokio::task::AbortHandle::id),
            "browser child-target controller failed closed"
        );
    }
    false
}

pub(super) fn structural_fail(shared: &Shared, target: &StructuralTarget) -> bool {
    if !shared.stopping.load(Ordering::Acquire) {
        tracing::error!(
            target: "crawlberg::ssrf_intercept",
            phase = "child_target_controller",
            reason = "structural_attachment_deadline",
            browser_handler_task_id = ?shared.handler_abort.as_ref().map(tokio::task::AbortHandle::id),
            structural_authorized = target.authorized,
            structural_arm_acknowledged = target.arm_acknowledged,
            structural_child_boundary_acknowledged = target.child_boundary_acknowledged,
            structural_waiting_for_debugger = target.waiting_for_debugger,
            structural_context_present = target.context.is_some(),
            "browser child-target controller failed closed"
        );
    }
    false
}

pub(super) fn owned_transport_end(browser: &chromiumoxide::Browser) -> bool {
    browser.observed_owned_termination()
}

pub(super) fn owned_command_cancelled(
    browser: &chromiumoxide::Browser,
    error: &chromiumoxide::error::CdpError,
) -> bool {
    owned_transport_end(browser)
        && matches!(
            error,
            chromiumoxide::error::CdpError::ChannelSendError(_) | chromiumoxide::error::CdpError::NoResponse
        )
}

pub(super) async fn disable_finished(browser: &chromiumoxide::Browser) -> bool {
    match tokio::time::timeout(
        super::CONTROLLER_TEARDOWN_TIMEOUT,
        browser.execute(super::FetchDisableParams::default()),
    )
    .await
    {
        Ok(Ok(_)) => true,
        Ok(Err(error)) => owned_command_cancelled(browser, &error),
        Err(_) => false,
    }
}

pub(super) fn child_transport_end(
    browser: Option<&chromiumoxide::Browser>,
    shared: &Shared,
    error: Option<&chromiumoxide::error::CdpError>,
    reason: &'static str,
) -> bool {
    if browser.is_some_and(|browser| browser.observe_owned_transport_end(error)) {
        return true;
    }
    child_fail(shared, reason)
}

pub(super) fn child_ack_fail(shared: &Shared, pending_command: &'static str, cdp_error_code: Option<i64>) -> bool {
    if !shared.stopping.load(Ordering::Acquire) {
        tracing::error!(
            target: "crawlberg::ssrf_intercept",
            phase = "child_target_controller",
            reason = "boundary_or_close_ack",
            browser_handler_task_id = ?shared.handler_abort.as_ref().map(tokio::task::AbortHandle::id),
            pending_command,
            cdp_error_code,
            "browser child-target controller failed closed"
        );
    }
    false
}
