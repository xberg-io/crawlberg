use std::time::Duration;

use crawlberg_browser::adapter::{
    NativeActionResult, NativeBrowserConfig, NativeBrowserExecutor, NativeBrowserWait, NativeCookie,
    NativeInteractionResult, NativePageAction, NativeScrollDirection,
};

use super::{PageAction, ScrollDirection};
use crate::error::CrawlError;
use crate::types::{ActionResult, AuthConfig, BrowserWait, CrawlConfig, InteractionResult};

pub(super) async fn run(
    url: &str,
    actions: &[PageAction],
    config: &CrawlConfig,
    native_executor: &NativeBrowserExecutor,
) -> Result<InteractionResult, CrawlError> {
    if config.browser.endpoint.is_some() {
        return Err(CrawlError::InvalidConfig(
            "browser.endpoint is only supported by the chromiumoxide backend".into(),
        ));
    }

    let native_config = build_native_config(config);
    let native_actions = actions.iter().map(map_action).collect::<Vec<_>>();
    let post_navigation_wait = post_navigation_wait(config);
    let timeout = config.browser.timeout;

    let native_result = native_executor
        .interact_url(url, &native_config, &native_actions, post_navigation_wait)
        .await
        .map_err(|e| {
            let message = e.to_string();
            if message.contains("timed out") {
                CrawlError::BrowserTimeout(format!("browser timed out after {timeout:?}"))
            } else {
                CrawlError::BrowserError(format!("native browser interact failed: {message}"))
            }
        })?;

    Ok(map_result(native_result))
}

fn build_native_config(config: &CrawlConfig) -> NativeBrowserConfig {
    let mut extra_headers = config.custom_headers.clone();
    match config.auth {
        Some(AuthConfig::Bearer { ref token }) => {
            extra_headers.insert("Authorization".to_owned(), format!("Bearer {token}"));
        }
        Some(AuthConfig::Header { ref name, ref value }) => {
            extra_headers.insert(name.clone(), value.clone());
        }
        _ => {}
    }

    let wait_until = match config.browser.wait {
        BrowserWait::NetworkIdle => NativeBrowserWait::NetworkIdle,
        BrowserWait::Selector => NativeBrowserWait::Selector,
        BrowserWait::Fixed => NativeBrowserWait::Load,
    };

    NativeBrowserConfig {
        user_agent: config.user_agent.clone(),
        timeout: config.browser.timeout,
        wait_until,
        extra_headers,
        respect_robots_txt: config.respect_robots_txt,
        stealth: matches!(config.browser.mode, crate::types::BrowserMode::Stealth),
        proxy_url: resolved_proxy(config),
        prior_cookies: Vec::<NativeCookie>::new(),
        block_url_patterns: config.browser.block_url_patterns.clone(),
        eval_script: config.browser.eval_script.clone(),
        wait_selector: config.browser.wait_selector.clone(),
        robots_user_agent: config.browser.robots_user_agent.clone(),
        capture_network_events: config.browser.capture_network_events,
        ssrf: None,
        allow_file_access: false,
    }
}

fn resolved_proxy(config: &CrawlConfig) -> Option<String> {
    config.browser.proxy.as_ref().or(config.proxy.as_ref()).map(|proxy| {
        if proxy.username.is_some() || proxy.password.is_some() {
            let user = proxy.username.as_deref().unwrap_or("");
            let pass = proxy.password.as_deref().unwrap_or("");
            if let Some(rest) = proxy.url.strip_prefix("http://") {
                format!("http://{user}:{pass}@{rest}")
            } else if let Some(rest) = proxy.url.strip_prefix("https://") {
                format!("https://{user}:{pass}@{rest}")
            } else {
                proxy.url.clone()
            }
        } else {
            proxy.url.clone()
        }
    })
}

fn post_navigation_wait(config: &CrawlConfig) -> Option<Duration> {
    let fixed_wait = if config.browser.wait == BrowserWait::Fixed {
        Some(Duration::from_secs(2))
    } else {
        None
    };
    match (fixed_wait, config.browser.extra_wait) {
        (Some(base), Some(extra)) => Some(base + extra),
        (Some(base), None) => Some(base),
        (None, extra) => extra,
    }
}

fn map_action(action: &PageAction) -> NativePageAction {
    match action {
        PageAction::Click { selector } => NativePageAction::Click {
            selector: selector.clone(),
        },
        PageAction::TypeText { selector, text } => NativePageAction::TypeText {
            selector: selector.clone(),
            text: text.clone(),
        },
        PageAction::Press { key } => NativePageAction::Press { key: key.clone() },
        PageAction::Scroll {
            direction,
            selector,
            amount,
        } => NativePageAction::Scroll {
            direction: map_scroll_direction(*direction),
            selector: selector.clone(),
            amount: *amount,
        },
        PageAction::Wait { milliseconds, selector } => NativePageAction::Wait {
            milliseconds: *milliseconds,
            selector: selector.clone(),
        },
        PageAction::Screenshot { full_page } => NativePageAction::Screenshot { full_page: *full_page },
        PageAction::ExecuteJs { script } => NativePageAction::ExecuteJs { script: script.clone() },
        PageAction::Scrape => NativePageAction::Scrape,
    }
}

fn map_scroll_direction(direction: ScrollDirection) -> NativeScrollDirection {
    match direction {
        ScrollDirection::Up => NativeScrollDirection::Up,
        ScrollDirection::Down => NativeScrollDirection::Down,
    }
}

fn map_result(result: NativeInteractionResult) -> InteractionResult {
    InteractionResult {
        action_results: result.action_results.into_iter().map(map_action_result).collect(),
        final_html: result.final_html,
        final_url: result.final_url,
        screenshot: result.screenshot,
    }
}

fn map_action_result(result: NativeActionResult) -> ActionResult {
    ActionResult {
        action_index: result.action_index,
        action_type: result.action_type.into(),
        success: result.success,
        data: result.data,
        error: result.error,
    }
}
