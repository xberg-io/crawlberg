use serde::{Deserialize, Serialize};

/// Maximum number of actions allowed in a single interaction sequence.
pub const MAX_ACTIONS: usize = 100;

/// Maximum total wait time across all Wait actions, in seconds.
pub const MAX_TOTAL_WAIT_SECS: u64 = 300;

/// Maximum wait time for a single Wait action, in milliseconds (5 minutes).
pub const MAX_SINGLE_WAIT_MS: u64 = 300_000;

/// Maximum length of a CSS selector, in bytes.
pub const MAX_SELECTOR_LEN: usize = 4096;

/// Maximum length of a JavaScript script, in bytes (1 MB).
pub const MAX_SCRIPT_LEN: usize = 1_048_576;

/// Maximum length of text to type, in bytes (1 MB).
pub const MAX_TEXT_LEN: usize = 1_048_576;

/// Maximum absolute scroll amount in pixels.
pub const MAX_SCROLL_AMOUNT: i64 = 100_000;

/// A single page interaction action.
///
/// Actions are serialized with a `type` tag using camelCase naming,
/// except `ExecuteJs` which is explicitly renamed to `"executeJs"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
#[derive(Default)]
pub enum PageAction {
    /// Click on an element matching the given CSS selector.
    Click {
        /// CSS selector for the element to click.
        selector: String,
    },
    /// Type text into an element matching the given CSS selector.
    #[serde(rename = "type")]
    TypeText {
        /// CSS selector for the input element.
        selector: String,
        /// Text to type into the element.
        text: String,
    },
    /// Press a keyboard key (e.g. "Enter", "Tab", "Escape").
    Press {
        /// Key name to press.
        key: String,
    },
    /// Scroll the page or a specific element.
    Scroll {
        /// Direction to scroll.
        direction: ScrollDirection,
        /// Optional CSS selector for a scrollable element. Scrolls the page if absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
        /// Optional pixel amount to scroll. Uses a default if absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        amount: Option<i64>,
    },
    /// Wait for a duration or for an element to appear.
    Wait {
        /// Milliseconds to wait. Ignored if `selector` is provided.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        milliseconds: Option<i64>,
        /// CSS selector to wait for.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
    },
    /// Take a screenshot of the current page.
    Screenshot {
        /// Whether to capture the full scrollable page. Defaults to viewport only.
        ///
        /// Accepts both the canonical `fullPage` (camelCase) form and the
        /// `full_page` (snake_case) alias so language bindings and fixtures can
        /// use either convention without error.
        #[serde(
            default,
            rename = "fullPage",
            alias = "full_page",
            skip_serializing_if = "Option::is_none"
        )]
        full_page: Option<bool>,
    },
    /// Execute arbitrary JavaScript in the page context.
    ///
    /// # Safety
    ///
    /// The script runs with full page privileges in the browser context.
    /// Only execute scripts from trusted sources.
    #[serde(rename = "executeJs")]
    ExecuteJs {
        /// JavaScript source code to execute. Max 1 MB.
        script: String,
    },
    /// Scrape the current page HTML.
    #[default]
    Scrape,
}

/// Direction for a scroll action.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScrollDirection {
    /// Scroll upward.
    Up,
    /// Scroll downward.
    #[default]
    Down,
}

impl From<ScrollDirection> for String {
    fn from(direction: ScrollDirection) -> Self {
        match direction {
            ScrollDirection::Up => "up".to_owned(),
            ScrollDirection::Down => "down".to_owned(),
        }
    }
}

impl From<String> for ScrollDirection {
    fn from(value: String) -> Self {
        value.as_str().into()
    }
}

impl From<&str> for ScrollDirection {
    fn from(value: &str) -> Self {
        match value {
            "up" | "Up" | "UP" => Self::Up,
            _ => Self::Down,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_wait_with_only_required_fields() {
        let json = r#"{"type":"wait"}"#;
        let action: PageAction = serde_json::from_str(json).unwrap();
        assert_eq!(
            action,
            PageAction::Wait {
                milliseconds: None,
                selector: None
            }
        );
    }

    #[test]
    fn deserialize_wait_selector_only() {
        let json = r##"{"type":"wait","selector":"#content"}"##;
        let action: PageAction = serde_json::from_str(json).unwrap();
        assert_eq!(
            action,
            PageAction::Wait {
                milliseconds: None,
                selector: Some("#content".to_owned())
            }
        );
    }

    #[test]
    fn deserialize_wait_milliseconds_only() {
        let json = r#"{"type":"wait","milliseconds":500}"#;
        let action: PageAction = serde_json::from_str(json).unwrap();
        assert_eq!(
            action,
            PageAction::Wait {
                milliseconds: Some(500),
                selector: None
            }
        );
    }

    #[test]
    fn deserialize_scroll_direction_only() {
        let json = r#"{"type":"scroll","direction":"down"}"#;
        let action: PageAction = serde_json::from_str(json).unwrap();
        assert_eq!(
            action,
            PageAction::Scroll {
                direction: ScrollDirection::Down,
                selector: None,
                amount: None
            }
        );
    }

    #[test]
    fn deserialize_scroll_with_selector_no_amount() {
        let json = r##"{"type":"scroll","direction":"up","selector":"#list"}"##;
        let action: PageAction = serde_json::from_str(json).unwrap();
        assert_eq!(
            action,
            PageAction::Scroll {
                direction: ScrollDirection::Up,
                selector: Some("#list".to_owned()),
                amount: None,
            }
        );
    }

    #[test]
    fn deserialize_screenshot_no_fields() {
        let json = r#"{"type":"screenshot"}"#;
        let action: PageAction = serde_json::from_str(json).unwrap();
        assert_eq!(action, PageAction::Screenshot { full_page: None });
    }

    #[test]
    fn deserialize_screenshot_full_page_true() {
        let json = r#"{"type":"screenshot","fullPage":true}"#;
        let action: PageAction = serde_json::from_str(json).unwrap();
        assert_eq!(action, PageAction::Screenshot { full_page: Some(true) });
    }

    #[test]
    fn deserialize_screenshot_full_page_alias() {
        let json = r#"{"type":"screenshot","full_page":false}"#;
        let action: PageAction = serde_json::from_str(json).unwrap();
        assert_eq!(action, PageAction::Screenshot { full_page: Some(false) });
    }
}
