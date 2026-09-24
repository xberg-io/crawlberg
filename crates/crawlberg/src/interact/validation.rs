use super::actions::{
    MAX_ACTIONS, MAX_SCRIPT_LEN, MAX_SCROLL_AMOUNT, MAX_SELECTOR_LEN, MAX_SINGLE_WAIT_MS, MAX_TEXT_LEN,
    MAX_TOTAL_WAIT_SECS, PageAction,
};
use crate::error::CrawlError;

/// Milliseconds per second, used to turn [`MAX_TOTAL_WAIT_SECS`] into a millisecond budget.
const MILLIS_PER_SEC: u64 = 1000;

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

    for (index, action) in actions.iter().enumerate() {
        match action {
            PageAction::Click { selector } => validate_selector(index, "click", selector)?,
            PageAction::TypeText { selector, text } => validate_type_text(index, selector, text)?,
            PageAction::Press { key } => validate_press(index, key)?,
            PageAction::Wait { milliseconds, selector } => {
                total_wait_ms = total_wait_ms.saturating_add(validate_wait(index, *milliseconds, selector.as_deref())?);
            }
            PageAction::ExecuteJs { script } => validate_execute_js(index, script)?,
            PageAction::Scroll { selector, amount, .. } => validate_scroll(index, selector.as_deref(), *amount)?,
            PageAction::Screenshot { .. } | PageAction::Scrape => {}
        }
    }

    let max_wait_ms = MAX_TOTAL_WAIT_SECS.saturating_mul(MILLIS_PER_SEC);
    if total_wait_ms > max_wait_ms {
        return Err(CrawlError::invalid_config(format!(
            "total wait time {total_wait_ms}ms exceeds maximum of {max_wait_ms}ms ({MAX_TOTAL_WAIT_SECS}s)"
        )));
    }

    Ok(())
}

fn validate_type_text(action_index: usize, selector: &str, text: &str) -> Result<(), CrawlError> {
    validate_selector(action_index, "type", selector)?;
    if text.len() > MAX_TEXT_LEN {
        return Err(CrawlError::invalid_config(format!(
            "action[{action_index}]: text exceeds maximum length of {MAX_TEXT_LEN} bytes"
        )));
    }
    Ok(())
}

fn validate_press(action_index: usize, key: &str) -> Result<(), CrawlError> {
    if key.is_empty() {
        return Err(CrawlError::invalid_config(format!(
            "action[{action_index}]: press key must not be empty"
        )));
    }
    Ok(())
}

/// Validate one `Wait` action and return the milliseconds it contributes to the total budget.
fn validate_wait(action_index: usize, milliseconds: Option<i64>, selector: Option<&str>) -> Result<u64, CrawlError> {
    let mut contributed = 0;
    if let Some(ms) = milliseconds {
        if ms < 0 {
            return Err(CrawlError::invalid_config(format!(
                "action[{action_index}]: wait time {ms}ms must not be negative"
            )));
        }
        let ms = ms as u64;
        if ms > MAX_SINGLE_WAIT_MS {
            return Err(CrawlError::invalid_config(format!(
                "action[{action_index}]: wait time {ms}ms exceeds maximum of {MAX_SINGLE_WAIT_MS}ms"
            )));
        }
        contributed = ms;
    }
    if let Some(selector) = selector {
        validate_selector(action_index, "wait", selector)?;
    }
    Ok(contributed)
}

fn validate_execute_js(action_index: usize, script: &str) -> Result<(), CrawlError> {
    if script.is_empty() {
        return Err(CrawlError::invalid_config(format!(
            "action[{action_index}]: executeJs script must not be empty"
        )));
    }
    if script.len() > MAX_SCRIPT_LEN {
        return Err(CrawlError::invalid_config(format!(
            "action[{action_index}]: script exceeds maximum length of {MAX_SCRIPT_LEN} bytes"
        )));
    }
    Ok(())
}

fn validate_scroll(action_index: usize, selector: Option<&str>, amount: Option<i64>) -> Result<(), CrawlError> {
    if let Some(selector) = selector {
        validate_selector(action_index, "scroll", selector)?;
    }
    if let Some(amount) = amount
        && amount.unsigned_abs() > MAX_SCROLL_AMOUNT as u64
    {
        return Err(CrawlError::invalid_config(format!(
            "action[{action_index}]: scroll amount {} exceeds maximum of {MAX_SCROLL_AMOUNT}",
            amount.unsigned_abs()
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

#[cfg(test)]
mod tests {
    use super::super::ScrollDirection;
    use super::*;

    fn message(result: Result<(), CrawlError>) -> String {
        match result {
            Ok(()) => panic!("expected a validation error, got Ok"),
            Err(CrawlError::InvalidConfig { message, .. }) => message,
            Err(other) => panic!("expected InvalidConfig, got {other:?}"),
        }
    }

    fn click(selector: &str) -> PageAction {
        PageAction::Click {
            selector: selector.to_owned(),
        }
    }

    fn wait_ms(milliseconds: i64) -> PageAction {
        PageAction::Wait {
            milliseconds: Some(milliseconds),
            selector: None,
        }
    }

    #[test]
    fn an_empty_action_list_and_a_list_at_the_limit_are_accepted() {
        assert!(validate_actions(&[]).is_ok());
        let at_limit = vec![PageAction::Scrape; MAX_ACTIONS];
        assert!(validate_actions(&at_limit).is_ok());
    }

    #[test]
    fn one_action_past_the_limit_is_rejected() {
        let over = vec![PageAction::Scrape; MAX_ACTIONS + 1];
        assert_eq!(
            message(validate_actions(&over)),
            format!("too many actions: {} exceeds maximum of {MAX_ACTIONS}", MAX_ACTIONS + 1)
        );
    }

    #[test]
    fn click_selector_must_be_non_empty_and_within_the_length_limit() {
        assert_eq!(
            message(validate_actions(&[click("")])),
            "action[0]: click selector must not be empty"
        );
        assert!(validate_actions(&[click(&"a".repeat(MAX_SELECTOR_LEN))]).is_ok());
        assert_eq!(
            message(validate_actions(&[click(&"a".repeat(MAX_SELECTOR_LEN + 1))])),
            format!("action[0]: selector exceeds maximum length of {MAX_SELECTOR_LEN} bytes")
        );
    }

    #[test]
    fn type_text_validates_selector_then_text_length() {
        let empty_selector = PageAction::TypeText {
            selector: String::new(),
            text: "x".to_owned(),
        };
        assert_eq!(
            message(validate_actions(&[empty_selector])),
            "action[0]: type selector must not be empty"
        );

        let long_selector = PageAction::TypeText {
            selector: "a".repeat(MAX_SELECTOR_LEN + 1),
            text: "x".repeat(MAX_TEXT_LEN + 1),
        };
        assert_eq!(
            message(validate_actions(&[long_selector])),
            format!("action[0]: selector exceeds maximum length of {MAX_SELECTOR_LEN} bytes"),
            "the selector check runs before the text check"
        );

        let long_text = PageAction::TypeText {
            selector: "#a".to_owned(),
            text: "x".repeat(MAX_TEXT_LEN + 1),
        };
        assert_eq!(
            message(validate_actions(&[long_text])),
            format!("action[0]: text exceeds maximum length of {MAX_TEXT_LEN} bytes")
        );
    }

    #[test]
    fn press_key_must_not_be_empty() {
        assert_eq!(
            message(validate_actions(&[PageAction::Press { key: String::new() }])),
            "action[0]: press key must not be empty"
        );
        assert!(
            validate_actions(&[PageAction::Press {
                key: "Enter".to_owned()
            }])
            .is_ok()
        );
    }

    #[test]
    fn wait_rejects_negative_and_oversized_single_waits() {
        assert_eq!(
            message(validate_actions(&[wait_ms(-1)])),
            "action[0]: wait time -1ms must not be negative"
        );
        let over = MAX_SINGLE_WAIT_MS as i64 + 1;
        assert_eq!(
            message(validate_actions(&[wait_ms(over)])),
            format!("action[0]: wait time {over}ms exceeds maximum of {MAX_SINGLE_WAIT_MS}ms")
        );
        assert!(validate_actions(&[wait_ms(MAX_SINGLE_WAIT_MS as i64)]).is_ok());
    }

    #[test]
    fn total_wait_is_summed_across_actions_and_capped() {
        let max_total_ms = MAX_TOTAL_WAIT_SECS.saturating_mul(1000);
        let single = MAX_SINGLE_WAIT_MS as i64;
        let at_limit: Vec<PageAction> = (0..(max_total_ms / MAX_SINGLE_WAIT_MS))
            .map(|_| wait_ms(single))
            .collect();
        assert!(validate_actions(&at_limit).is_ok(), "the total cap itself is allowed");

        let mut over = at_limit;
        over.push(wait_ms(1));
        assert_eq!(
            message(validate_actions(&over)),
            format!(
                "total wait time {}ms exceeds maximum of {max_total_ms}ms ({MAX_TOTAL_WAIT_SECS}s)",
                max_total_ms + 1
            )
        );
    }

    #[test]
    fn wait_selector_is_validated_and_the_action_index_is_reported() {
        let actions = vec![
            PageAction::Scrape,
            PageAction::Wait {
                milliseconds: None,
                selector: Some(String::new()),
            },
        ];
        assert_eq!(
            message(validate_actions(&actions)),
            "action[1]: wait selector must not be empty"
        );
    }

    #[test]
    fn scroll_validates_its_selector_and_absolute_amount() {
        let empty = PageAction::Scroll {
            direction: ScrollDirection::Down,
            selector: Some(String::new()),
            amount: None,
        };
        assert_eq!(
            message(validate_actions(&[empty])),
            "action[0]: scroll selector must not be empty"
        );

        let negative_over = PageAction::Scroll {
            direction: ScrollDirection::Up,
            selector: None,
            amount: Some(-(MAX_SCROLL_AMOUNT + 1)),
        };
        assert_eq!(
            message(validate_actions(&[negative_over])),
            format!(
                "action[0]: scroll amount {} exceeds maximum of {MAX_SCROLL_AMOUNT}",
                MAX_SCROLL_AMOUNT + 1
            ),
            "the magnitude is checked, so a negative amount is rejected too"
        );

        let at_limit = PageAction::Scroll {
            direction: ScrollDirection::Down,
            selector: None,
            amount: Some(MAX_SCROLL_AMOUNT),
        };
        assert!(validate_actions(&[at_limit]).is_ok());
    }

    #[test]
    fn execute_js_script_must_be_non_empty_and_within_the_length_limit() {
        assert_eq!(
            message(validate_actions(&[PageAction::ExecuteJs { script: String::new() }])),
            "action[0]: executeJs script must not be empty"
        );
        assert_eq!(
            message(validate_actions(&[PageAction::ExecuteJs {
                script: "x".repeat(MAX_SCRIPT_LEN + 1),
            }])),
            format!("action[0]: script exceeds maximum length of {MAX_SCRIPT_LEN} bytes")
        );
    }

    #[test]
    fn screenshot_and_scrape_are_always_accepted() {
        let actions = vec![PageAction::Screenshot { full_page: Some(true) }, PageAction::Scrape];
        assert!(validate_actions(&actions).is_ok());
    }

    #[test]
    fn the_first_failing_action_decides_the_error() {
        let actions = vec![click("#ok"), click(""), PageAction::Press { key: String::new() }];
        assert_eq!(
            message(validate_actions(&actions)),
            "action[1]: click selector must not be empty"
        );
    }
}
