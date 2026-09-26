use std::time::Duration;

use crawlberg_browser::adapter::{
    NativeActionResult, NativeBrowserConfig, NativeBrowserExecutor, NativeBrowserWait, NativeCookie,
    NativeInteractionResult, NativePageAction, NativeScrollDirection,
};

use super::{DEFAULT_ACTION_TIMEOUT, PageAction, ScrollDirection, encode_screenshot_base64};
use crate::error::CrawlError;
use crate::types::{ActionResult, AuthConfig, BrowserWait, CrawlConfig, InteractionResult};

pub(super) async fn run(
    url: &str,
    actions: &[PageAction],
    config: &CrawlConfig,
    native_executor: &NativeBrowserExecutor,
) -> Result<InteractionResult, CrawlError> {
    if config.browser.endpoint.is_some() {
        return Err(CrawlError::invalid_config(
            "browser.endpoint is only supported by the chromiumoxide backend",
        ));
    }

    let native_config = build_native_config(config)?;
    let native_actions = actions.iter().map(map_action).collect::<Vec<_>>();
    let post_navigation_wait = post_navigation_wait(config);
    let timeout = config.browser.timeout;

    // ~keep The native worker executes navigation + all actions (including ExecuteJs, which has
    // ~keep no per-action timeout in the worker) as one job on a dedicated OS thread; a hung
    // ~keep script blocks that thread indefinitely. This wraps the whole call in a budget scaled
    // ~keep by action count so callers get a bounded error instead of hanging forever. It cannot
    // ~keep reclaim the stuck worker thread itself — see interact/native.rs task notes.
    let action_budget = actions
        .iter()
        .map(PageAction::timeout)
        .fold(DEFAULT_ACTION_TIMEOUT, |total, budget| total.saturating_add(budget));
    let overall_timeout = timeout.saturating_add(action_budget);

    let native_result = tokio::time::timeout(
        overall_timeout,
        native_executor.interact_url(url, &native_config, &native_actions, post_navigation_wait),
    )
    .await
    .map_err(|_| {
        CrawlError::browser_timeout(format!(
            "browser timed out after {overall_timeout:?} ({} actions)",
            actions.len()
        ))
    })?
    .map_err(|e| {
        let message = e.to_string();
        if message.contains("timed out") {
            CrawlError::browser_timeout(format!("browser timed out after {timeout:?}"))
        } else {
            CrawlError::browser_error(format!("native browser interact failed: {message}"))
        }
    })?;

    Ok(map_result(native_result))
}

fn build_native_config(config: &CrawlConfig) -> Result<NativeBrowserConfig, CrawlError> {
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

    Ok(NativeBrowserConfig {
        user_agent: config.user_agent.clone(),
        timeout: config.browser.timeout,
        wait_until,
        extra_headers,
        respect_robots_txt: config.respect_robots_txt,
        stealth: matches!(config.browser.mode, crate::types::BrowserMode::Stealth),
        proxy_url: resolved_proxy(config)?,
        prior_cookies: Vec::<NativeCookie>::new(),
        block_url_patterns: config.browser.block_url_patterns.clone(),
        eval_script: config.browser.eval_script.clone(),
        wait_selector: config.browser.wait_selector.clone(),
        robots_user_agent: config.browser.robots_user_agent.clone(),
        capture_network_events: config.browser.capture_network_events,
        ssrf: Some(crate::net::browser_policy::validator_for(&config.ssrf)),
        allow_file_access: false,
    })
}

/// Resolve the proxy URL string handed to the native browser worker: the
/// browser-specific proxy if set, else the crawl-wide one, with any
/// configured credentials embedded by
/// `net::proxy_credentials::embed_proxy_credentials` (shared with the
/// crawl/scrape path in `crate::native_browser`).
fn resolved_proxy(config: &CrawlConfig) -> Result<Option<String>, CrawlError> {
    config
        .browser
        .proxy
        .as_ref()
        .or(config.proxy.as_ref())
        .map(crate::net::proxy_credentials::embed_proxy_credentials)
        .transpose()
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
    let screenshot_base64 = result.screenshot.as_deref().map(encode_screenshot_base64);
    InteractionResult {
        action_results: result.action_results.into_iter().map(map_action_result).collect(),
        final_html: result.final_html,
        final_url: result.final_url,
        screenshot: result.screenshot,
        screenshot_base64,
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

// Credential embedding itself (scheme case, every scheme with an authority component,
// percent-encoding including a literal '%', and the unparseable-URL error) is exercised
// once by `net::proxy_credentials`'s own tests, which this module now delegates to.

#[cfg(test)]
mod native_worker_hang_tests {
    use std::time::Duration;

    use crawlberg_browser::adapter::{NativeBrowserExecutor, NativeBrowserExecutorConfig};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::run;
    use crate::interact::actions::PageAction;
    use crate::types::{BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig};

    // ~keep Reaches wiremock's 127.0.0.1 server through the `allow_private_networks` config
    // seam rather than the `CRAWLBERG_ALLOW_PRIVATE_NETWORK` variable. Writing that variable
    // here was a process-global mutation racing the `std::env::var` reads that every
    // concurrent non-serial test in this binary performs via `CrawlConfig::default` ->
    // `SsrfPolicy::from_env`; on glibc that can realloc `environ` under a reader and abort
    // the process with no failing test name.
    fn native_test_config() -> CrawlConfig {
        CrawlConfig {
            browser: BrowserConfig {
                backend: BrowserBackend::Native,
                mode: BrowserMode::Always,
                timeout: Duration::from_secs(10),
                ..BrowserConfig::default()
            },
            ..CrawlConfig::builder().allow_private_networks(true).build()
        }
    }

    /// Proves both halves of the task-1 diagnosis with one deterministic, non-Chrome scenario — the
    /// native backend never launches a real browser subprocess; scripts run through an in-process
    /// `deno_core` V8 isolate, so no `/Applications/Google Chrome.app` dependency is needed here:
    ///
    /// 1. The caller of `native::run` does not hang forever on a non-terminating `ExecuteJs` — the
    ///    short external timeout below fires promptly, because it is polled on the *calling* task's
    ///    OS thread, which is distinct from the dedicated worker thread that actually runs the script.
    /// 2. The worker's dedicated OS thread is genuinely NOT reclaimed. A trivial follow-up job
    ///    submitted to the same single-worker executor also never completes, because that worker
    ///    stays permanently blocked inside the synchronous `JsRuntime::execute_script` call the first
    ///    script triggers. `execute_action`'s own `tokio::time::timeout(ACTION_TIMEOUT, ...)` wrap in
    ///    `crawlberg-browser/src/adapter.rs` never gets a chance to act: `NativePageAction::ExecuteJs`
    ///    resolves via `page.evaluate_result(script)`, a plain synchronous call with no `.await` point,
    ///    so polling that branch never returns control to the worker's executor to check the timer. ~keep
    #[tokio::test]
    async fn hung_execute_js_permanently_pins_the_native_worker_thread() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("<html><body>hang test</body></html>")
                    .append_header("content-type", "text/html"),
            )
            .mount(&mock)
            .await;

        let executor = NativeBrowserExecutor::new(NativeBrowserExecutorConfig::with_workers(1))
            .expect("single-worker executor should start");
        let config = native_test_config();
        let url = mock.uri();

        let hang_actions = vec![PageAction::ExecuteJs {
            script: "while (true) {}".to_owned(),
        }];
        let hang_outcome =
            tokio::time::timeout(Duration::from_secs(5), run(&url, &hang_actions, &config, &executor)).await;
        assert!(
            hang_outcome.is_err(),
            "caller must not hang forever awaiting a non-terminating ExecuteJs action, got {hang_outcome:?}"
        );

        let followup_actions = vec![PageAction::Scrape];
        let followup_outcome =
            tokio::time::timeout(Duration::from_secs(5), run(&url, &followup_actions, &config, &executor)).await;
        assert!(
            followup_outcome.is_err(),
            "a trivial follow-up job on the same single-worker executor must also fail to complete: the sole \
             worker OS thread should still be pinned inside the hung ExecuteJs script if ACTION_TIMEOUT failed \
             to reclaim it, got {followup_outcome:?}"
        );

        // ~keep The worker OS thread never returns from the hung V8 call, so `NativeBrowserExecutor`'s
        // ~keep `Drop` (which joins every worker thread) would block this test process forever. Leak the
        // ~keep executor deliberately instead of letting it drop — this mirrors the real leak the test
        // ~keep demonstrates and keeps the test binary from hanging on exit.
        std::mem::forget(executor);
    }
}
