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
