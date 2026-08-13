use super::actions::{
    MAX_ACTIONS, MAX_SCRIPT_LEN, MAX_SCROLL_AMOUNT, MAX_SELECTOR_LEN, MAX_SINGLE_WAIT_MS, MAX_TEXT_LEN,
    MAX_TOTAL_WAIT_SECS, PageAction,
};
use crate::error::CrawlError;

/// Validate a sequence of page actions before execution.
///
/// Checks:
/// 1. Action count does not exceed [`MAX_ACTIONS`].
/// 2. Total wait time across all `Wait` actions does not exceed [`MAX_TOTAL_WAIT_SECS`] seconds.
/// 3. `Click` and `Type` selectors are non-empty.
/// 4. `ExecuteJs` script is non-empty.
/// 5. `Press` key is non-empty.
pub fn validate_actions(actions: &[PageAction]) -> Result<(), CrawlError> {
    if actions.len() > MAX_ACTIONS {
        return Err(CrawlError::invalid_config(format!(
            "too many actions: {} exceeds maximum of {MAX_ACTIONS}",
            actions.len()
        )));
    }

    let mut total_wait_ms: u64 = 0;

    for (i, action) in actions.iter().enumerate() {
        match action {
            PageAction::Click { selector } => {
                if selector.is_empty() {
                    return Err(CrawlError::invalid_config(format!(
                        "action[{i}]: click selector must not be empty"
                    )));
                }
                if selector.len() > MAX_SELECTOR_LEN {
                    return Err(CrawlError::invalid_config(format!(
                        "action[{i}]: selector exceeds maximum length of {MAX_SELECTOR_LEN} bytes"
                    )));
                }
            }
            PageAction::TypeText { selector, text } => {
                if selector.is_empty() {
                    return Err(CrawlError::invalid_config(format!(
                        "action[{i}]: type selector must not be empty"
                    )));
                }
                if selector.len() > MAX_SELECTOR_LEN {
                    return Err(CrawlError::invalid_config(format!(
                        "action[{i}]: selector exceeds maximum length of {MAX_SELECTOR_LEN} bytes"
                    )));
                }
                if text.len() > MAX_TEXT_LEN {
                    return Err(CrawlError::invalid_config(format!(
                        "action[{i}]: text exceeds maximum length of {MAX_TEXT_LEN} bytes"
                    )));
                }
            }
            PageAction::Press { key } => {
                if key.is_empty() {
                    return Err(CrawlError::invalid_config(format!(
                        "action[{i}]: press key must not be empty"
                    )));
                }
            }
            PageAction::Wait { milliseconds, selector } => {
                if let Some(ms) = milliseconds {
                    if *ms < 0 {
                        return Err(CrawlError::invalid_config(format!(
                            "action[{i}]: wait time {ms}ms must not be negative"
                        )));
                    }
                    let ms = *ms as u64;
                    if ms > MAX_SINGLE_WAIT_MS {
                        return Err(CrawlError::invalid_config(format!(
                            "action[{i}]: wait time {ms}ms exceeds maximum of {MAX_SINGLE_WAIT_MS}ms"
                        )));
                    }
                    total_wait_ms = total_wait_ms.saturating_add(ms);
                }
                if let Some(selector) = selector {
                    validate_selector(i, "wait", selector)?;
                }
            }
            PageAction::ExecuteJs { script } => {
                if script.is_empty() {
                    return Err(CrawlError::invalid_config(format!(
                        "action[{i}]: executeJs script must not be empty"
                    )));
                }
                if script.len() > MAX_SCRIPT_LEN {
                    return Err(CrawlError::invalid_config(format!(
                        "action[{i}]: script exceeds maximum length of {MAX_SCRIPT_LEN} bytes"
                    )));
                }
            }
            PageAction::Scroll { selector, amount, .. } => {
                if let Some(selector) = selector {
                    validate_selector(i, "scroll", selector)?;
                }
                if let Some(a) = amount
                    && a.unsigned_abs() > MAX_SCROLL_AMOUNT as u64
                {
                    return Err(CrawlError::invalid_config(format!(
                        "action[{i}]: scroll amount {} exceeds maximum of {MAX_SCROLL_AMOUNT}",
                        a.unsigned_abs()
                    )));
                }
            }
            PageAction::Screenshot { .. } | PageAction::Scrape => {}
        }
    }

    let max_wait_ms = MAX_TOTAL_WAIT_SECS.saturating_mul(1000);
    if total_wait_ms > max_wait_ms {
        return Err(CrawlError::invalid_config(format!(
            "total wait time {total_wait_ms}ms exceeds maximum of {max_wait_ms}ms ({MAX_TOTAL_WAIT_SECS}s)"
        )));
    }

    Ok(())
}

fn validate_selector(action_index: usize, action_type: &str, selector: &str) -> Result<(), CrawlError> {
    if selector.is_empty() {
        return Err(CrawlError::invalid_config(format!(
            "action[{action_index}]: {action_type} selector must not be empty"
        )));
    }
    if selector.len() > MAX_SELECTOR_LEN {
        return Err(CrawlError::invalid_config(format!(
            "action[{action_index}]: selector exceeds maximum length of {MAX_SELECTOR_LEN} bytes"
        )));
    }
    Ok(())
}
