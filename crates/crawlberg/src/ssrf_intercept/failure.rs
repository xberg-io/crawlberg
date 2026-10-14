use std::sync::atomic::Ordering;

use super::Shared;

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
