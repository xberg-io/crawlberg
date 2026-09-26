use std::sync::Arc;
use std::time::Duration;

use chromiumoxide::Handler;
use chromiumoxide::browser::{Browser, BrowserConfig as ChromeBrowserConfig};
use chromiumoxide::cdp::browser_protocol::network::{Headers, SetExtraHttpHeadersParams};
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::page::ScreenshotParams;
use serde_json::json;
use tokio_stream::StreamExt;

use super::{PageAction, ScrollDirection, encode_screenshot_base64};
use crate::error::CrawlError;
use crate::ssrf_intercept::{BrowserFirewall, StoppedResponse, Watch};
use crate::types::{ActionResult, AuthConfig, BrowserWait, CrawlConfig, InteractionResult};

pub(super) async fn run(
    url: &str,
    actions: &[PageAction],
    config: &CrawlConfig,
) -> Result<InteractionResult, CrawlError> {
    let (browser, mut handler, data_dir) = launch_or_connect(config).await?;
    let handler_handle = tokio::spawn(async move { while handler.next().await.is_some() {} });

    let browser = Arc::new(browser);
    let result = match BrowserFirewall::start(Arc::clone(&browser)).await {
        Ok(firewall) => {
            let result = run_with_browser(&browser, &firewall, url, actions, config).await;
            firewall.stop().await;
            result
        }
        Err(error) => Err(error),
    };

    // ~keep The stopped firewall held the only other reference, so this is the browser itself.
    if let Some(mut browser) = Arc::into_inner(browser) {
        let _ = browser.close().await;
        let _ = browser.wait().await;
    }
    let _ = tokio::time::timeout(Duration::from_secs(5), handler_handle).await;
    if let Some(dir) = data_dir {
        let _ = std::fs::remove_dir_all(dir);
    }

    result
}

/// Run every action in order, collecting one [`ActionResult`] each and the last screenshot taken.
///
/// A failing action is recorded and the run continues, so the caller always gets one result per
/// requested action.
async fn run_actions(page: &chromiumoxide::Page, actions: &[PageAction]) -> (Vec<ActionResult>, Option<Vec<u8>>) {
    let mut action_results = Vec::with_capacity(actions.len());
    let mut screenshot = None;

    for (index, action) in actions.iter().enumerate() {
        match run_action_with_timeout(page, action, index).await {
            Ok(action_data) => {
                if let Some(bytes) = action_data.screenshot {
                    screenshot = Some(bytes);
                }
                action_results.push(ActionResult {
                    action_index: index,
                    action_type: action_type(action).into(),
                    success: true,
                    data: action_data.data,
                    error: None,
                });
            }
            Err(error) => {
                action_results.push(ActionResult {
                    action_index: index,
                    action_type: action_type(action).into(),
                    success: false,
                    data: None,
                    error: Some(error.to_string()),
                });
            }
        }
    }

    (action_results, screenshot)
}

/// Execute one action under its own timeout budget.
///
/// ~keep A non-terminating ExecuteJs script otherwise hangs `page.evaluate` forever,
/// ~keep leaking the Chrome subprocess the caller never gets a chance to close.
async fn run_action_with_timeout(
    page: &chromiumoxide::Page,
    action: &PageAction,
    index: usize,
) -> Result<ActionData, CrawlError> {
    let budget = action.timeout();
    match tokio::time::timeout(budget, execute_action(page, action)).await {
        Ok(result) => result,
        Err(_) => Err(CrawlError::browser_timeout(format!(
            "action[{index}] ({}) timed out after {budget:?}",
            action_type(action)
        ))),
    }
}

async fn run_with_browser(
    browser: &Browser,
    firewall: &BrowserFirewall,
    url: &str,
    actions: &[PageAction],
    config: &CrawlConfig,
) -> Result<InteractionResult, CrawlError> {
    let page = browser
        .new_page("about:blank")
        .await
        .map_err(|e| CrawlError::browser_error(format!("failed to create page: {e}")))?;

    // ~keep The SSRF check holds for the whole session, not just the first navigation: the
    // ~keep actions click, submit forms and run scripts, and each can send the page, a frame,
    // ~keep a worker or a popup to an address the policy refuses (xberg-io/crawlberg#153).
    // ~keep Closing the watch closes the popups, children first, then the page, and stops
    // ~keep watching only once Chrome has destroyed them, so the check answers until then.
    match firewall.handle().watch(&page, &config.ssrf, config.max_redirects).await {
        Ok(watch) => {
            let result = async {
                prepare_page(&page, config).await?;
                run_session(&page, &watch, url, actions, config).await
            }
            .await;
            watch.close().await;
            result
        }
        Err(error) => {
            let _ = page.close().await;
            Err(error)
        }
    }
}

/// Navigate to `url`, then run the actions and read the final page.
async fn run_session(
    page: &chromiumoxide::Page,
    watch: &Watch,
    url: &str,
    actions: &[PageAction],
    config: &CrawlConfig,
) -> Result<InteractionResult, CrawlError> {
    if let Some(stop) = navigate_and_wait(page, watch, url, config).await? {
        return Ok(no_document_result(&stop, actions));
    }
    if let Some(ref script) = config.browser.eval_script {
        evaluate_json(page, script).await.map_err(|e| {
            CrawlError::browser_error(format!(
                "post-navigation eval_script failed before interaction actions: {e}"
            ))
        })?;
    }

    let (action_results, screenshot) = run_actions(page, actions).await;

    let final_html = page
        .content()
        .await
        .map_err(|e| CrawlError::browser_error(format!("failed to extract final HTML: {e}")))?;
    let final_url = evaluate_json(page, "location.href")
        .await
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| url.to_owned());

    let screenshot_base64 = screenshot.as_deref().map(encode_screenshot_base64);

    Ok(InteractionResult {
        action_results,
        final_html,
        final_url,
        screenshot,
        screenshot_base64,
    })
}

/// The result of a navigation that ended on a response without a document: the URL that
/// answered, no HTML, and a failed result per action, since there is no page to act on.
///
/// ~keep `scrape` reports the same response as a page with its status and an empty body.
/// ~keep `InteractionResult` has no status, so the action errors carry it.
fn no_document_result(stop: &StoppedResponse, actions: &[PageAction]) -> InteractionResult {
    let error = format!(
        "no page to act on: {} answered {} with no document",
        stop.url, stop.status
    );
    InteractionResult {
        action_results: actions
            .iter()
            .enumerate()
            .map(|(index, action)| ActionResult {
                action_index: index,
                action_type: action_type(action).into(),
                success: false,
                data: None,
                error: Some(error.clone()),
            })
            .collect(),
        final_html: String::new(),
        final_url: stop.url.clone(),
        screenshot: None,
        screenshot_base64: None,
    }
}

async fn prepare_page(page: &chromiumoxide::Page, config: &CrawlConfig) -> Result<(), CrawlError> {
    if matches!(config.browser.mode, crate::types::BrowserMode::Stealth) {
        crate::stealth::apply_stealth_patches(page).await;
    }

    if let Some(ref ua) = config.user_agent {
        page.set_user_agent(ua)
            .await
            .map_err(|e| CrawlError::browser_error(format!("failed to set user agent: {e}")))?;
    }

    let mut extra_headers = serde_json::Map::new();
    for (key, value) in &config.custom_headers {
        extra_headers.insert(key.clone(), serde_json::Value::String(value.clone()));
    }
    match config.auth {
        Some(AuthConfig::Bearer { ref token }) => {
            extra_headers.insert(
                "Authorization".to_owned(),
                serde_json::Value::String(format!("Bearer {token}")),
            );
        }
        Some(AuthConfig::Header { ref name, ref value }) => {
            extra_headers.insert(name.clone(), serde_json::Value::String(value.clone()));
        }
        _ => {}
    }

    if !extra_headers.is_empty() {
        let params = SetExtraHttpHeadersParams::new(Headers::new(serde_json::Value::Object(extra_headers)));
        page.execute(params)
            .await
            .map_err(|e| CrawlError::browser_error(format!("failed to set headers: {e}")))?;
    }

    Ok(())
}

/// Navigate to `url` and wait for the page. Returns the response the navigation stopped on
/// when it has no document: the redirect past `max_redirects`, or a 204, 205 or 304.
// ~keep The pre-flight check in `interact::run` only covers the seed URL, and a browser follows
// ~keep redirects/client-side navigations internally, so `watch` checks every request the
// ~keep navigation makes, the same way the scrape/crawl path does (xberg-io/crawlberg#74).
async fn navigate_and_wait(
    page: &chromiumoxide::Page,
    watch: &Watch,
    url: &str,
    config: &CrawlConfig,
) -> Result<Option<StoppedResponse>, CrawlError> {
    let timeout = config.browser.timeout;

    let navigation = tokio::time::timeout(timeout, async {
        page.goto(url)
            .await
            .map_err(|e| CrawlError::browser_error(format!("navigation failed: {e}")))?;
        wait_for_ready(page, config)
            .await
            .map_err(|e| CrawlError::browser_error(format!("wait failed: {e}")))?;
        Ok::<(), CrawlError>(())
    })
    .await;

    let intercepted = watch.take_outcome();
    if intercepted.blocked.is_none()
        && let Some(stop) = intercepted.stopped_response
    {
        return Ok(Some(stop));
    }
    resolve_navigation_outcome(navigation, intercepted.blocked, timeout)?;

    if let Some(extra) = config.browser.extra_wait {
        tokio::time::sleep(extra).await;
    }

    Ok(None)
}

/// Resolve navigation's timeout/error/SSRF-block outcome into a single result.
///
/// ~keep Mirrors `browser::navigation::resolve_navigation_outcome`: a request blocked by
/// ~keep interception surfaces as `CrawlError::SsrfPolicyViolation`, taking priority over the
/// ~keep generic navigation error Chrome reports for the same failed request (CDP's
/// ~keep `BlockedByClient` typically surfaces to `page.goto` as an ordinary `net::ERR_FAILED`).
fn resolve_navigation_outcome(
    navigation: Result<Result<(), CrawlError>, tokio::time::error::Elapsed>,
    blocked: Option<(String, String)>,
    timeout: Duration,
) -> Result<(), CrawlError> {
    let navigation_error = match navigation {
        Ok(Ok(())) => return Ok(()),
        Ok(Err(error)) => error,
        Err(_) => CrawlError::browser_timeout(format!("browser timed out after {timeout:?}")),
    };
    if let Some((blocked_url, reason)) = blocked {
        return Err(CrawlError::SsrfPolicyViolation {
            url: blocked_url,
            reason,
            source: None,
        });
    }
    Err(navigation_error)
}

async fn wait_for_ready(
    page: &chromiumoxide::Page,
    config: &CrawlConfig,
) -> Result<(), chromiumoxide::error::CdpError> {
    match config.browser.wait {
        BrowserWait::NetworkIdle => tokio::time::sleep(Duration::from_millis(500)).await,
        BrowserWait::Selector => {
            if let Some(ref selector) = config.browser.wait_selector {
                page.find_element(selector).await?;
            } else {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
        BrowserWait::Fixed => tokio::time::sleep(Duration::from_secs(2)).await,
    }
    Ok(())
}

struct ActionData {
    data: Option<serde_json::Value>,
    screenshot: Option<Vec<u8>>,
}

impl ActionData {
    fn empty() -> Self {
        Self {
            data: None,
            screenshot: None,
        }
    }

    fn data(data: serde_json::Value) -> Self {
        Self {
            data: Some(data),
            screenshot: None,
        }
    }
}

async fn execute_action(page: &chromiumoxide::Page, action: &PageAction) -> Result<ActionData, CrawlError> {
    match action {
        PageAction::Click { selector } => {
            page.find_element(selector)
                .await
                .map_err(|e| CrawlError::browser_error(format!("failed to find click target {selector:?}: {e}")))?
                .click()
                .await
                .map_err(|e| CrawlError::browser_error(format!("failed to click {selector:?}: {e}")))?;
            Ok(ActionData::empty())
        }
        PageAction::TypeText { selector, text } => {
            let element = page
                .find_element(selector)
                .await
                .map_err(|e| CrawlError::browser_error(format!("failed to find type target {selector:?}: {e}")))?;
            element
                .click()
                .await
                .map_err(|e| CrawlError::browser_error(format!("failed to focus {selector:?}: {e}")))?;
            element
                .type_str(text)
                .await
                .map_err(|e| CrawlError::browser_error(format!("failed to type into {selector:?}: {e}")))?;
            Ok(ActionData::empty())
        }
        PageAction::Press { key } => {
            dispatch_key_event(page, key).await?;
            Ok(ActionData::empty())
        }
        PageAction::Scroll {
            direction,
            selector,
            amount,
        } => {
            scroll(page, *direction, selector.as_deref(), *amount).await?;
            Ok(ActionData::empty())
        }
        PageAction::Wait { milliseconds, selector } => {
            if let Some(selector) = selector {
                page.find_element(selector)
                    .await
                    .map_err(|e| CrawlError::browser_error(format!("failed waiting for selector {selector:?}: {e}")))?;
            } else if let Some(ms) = milliseconds {
                tokio::time::sleep(Duration::from_millis(*ms as u64)).await;
            }
            Ok(ActionData::empty())
        }
        PageAction::Screenshot { full_page } => {
            let params = ScreenshotParams::builder()
                .format(CaptureScreenshotFormat::Png)
                .full_page(full_page.unwrap_or(false))
                .build();
            let bytes = page
                .screenshot(params)
                .await
                .map_err(|e| CrawlError::browser_error(format!("failed to capture screenshot: {e}")))?;
            let len = bytes.len();
            Ok(ActionData {
                data: Some(json!({ "bytes": len, "format": "png" })),
                screenshot: Some(bytes),
            })
        }
        PageAction::ExecuteJs { script } => {
            let value = evaluate_json(page, script).await?;
            Ok(ActionData::data(value))
        }
        PageAction::Scrape => {
            let html = page
                .content()
                .await
                .map_err(|e| CrawlError::browser_error(format!("failed to scrape current page: {e}")))?;
            Ok(ActionData::data(json!({ "html": html })))
        }
    }
}

async fn evaluate_json(page: &chromiumoxide::Page, script: &str) -> Result<serde_json::Value, CrawlError> {
    // ~keep Chrome Runtime.evaluate rejects top-level `return`; wrap function-body snippets in an IIFE.
    let wrapped;
    let effective_script = if script.contains("return ") || script.trim_start().starts_with("return") {
        wrapped = format!("(function() {{ {script} }})()");
        wrapped.as_str()
    } else {
        script
    };
    let result = page
        .evaluate(effective_script)
        .await
        .map_err(|e| CrawlError::browser_error(format!("failed to evaluate JavaScript: {e}")))?;
    Ok(result.value().cloned().unwrap_or(serde_json::Value::Null))
}

async fn dispatch_key_event(page: &chromiumoxide::Page, key: &str) -> Result<(), CrawlError> {
    let key_json = serde_json::to_string(key).map_err(|e| CrawlError::other(format!("failed to encode key: {e}")))?;
    let script = format!(
        r#"
        (() => {{
            const key = {key_json};
            const target = document.activeElement || document.body || document;
            for (const type of ["keydown", "keyup"]) {{
                target.dispatchEvent(new KeyboardEvent(type, {{ key, bubbles: true, cancelable: true }}));
            }}
            return true;
        }})()
        "#
    );
    evaluate_json(page, &script).await?;
    Ok(())
}

async fn scroll(
    page: &chromiumoxide::Page,
    direction: ScrollDirection,
    selector: Option<&str>,
    amount: Option<i64>,
) -> Result<(), CrawlError> {
    let amount = amount.unwrap_or(800).unsigned_abs();
    let signed_amount = match direction {
        ScrollDirection::Up => format!("-{amount}"),
        ScrollDirection::Down => amount.to_string(),
    };
    let selector_json =
        serde_json::to_string(&selector).map_err(|e| CrawlError::other(format!("failed to encode selector: {e}")))?;
    let script = format!(
        r#"
        (() => {{
            const selector = {selector_json};
            const target = selector ? document.querySelector(selector) : window;
            if (!target) {{
                throw new Error(`scroll target not found: ${{selector}}`);
            }}
            if (target === window) {{
                window.scrollBy(0, {signed_amount});
            }} else {{
                target.scrollTop += {signed_amount};
            }}
            return true;
        }})()
        "#
    );
    evaluate_json(page, &script).await?;
    Ok(())
}

fn action_type(action: &PageAction) -> &'static str {
    match action {
        PageAction::Click { .. } => "click",
        PageAction::TypeText { .. } => "type",
        PageAction::Press { .. } => "press",
        PageAction::Scroll { .. } => "scroll",
        PageAction::Wait { .. } => "wait",
        PageAction::Screenshot { .. } => "screenshot",
        PageAction::ExecuteJs { .. } => "executeJs",
        PageAction::Scrape => "scrape",
    }
}

async fn launch_or_connect(config: &CrawlConfig) -> Result<(Browser, Handler, Option<std::path::PathBuf>), CrawlError> {
    if let Some(ref endpoint) = config.browser.endpoint {
        let (browser, handler) = Browser::connect(endpoint)
            .await
            .map_err(|e| CrawlError::browser_error(format!("failed to connect to {endpoint}: {e}")))?;
        Ok((browser, handler, None))
    } else {
        use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
        static LAUNCH_COUNTER: AtomicU64 = AtomicU64::new(0);
        let user_data_dir = std::env::temp_dir().join(format!(
            "crawlberg-interact-{}-{}",
            std::process::id(),
            LAUNCH_COUNTER.fetch_add(1, AtomicOrdering::Relaxed),
        ));

        let proxy_url = config
            .browser
            .proxy
            .as_ref()
            .or(config.proxy.as_ref())
            .map(|p| p.url.as_str());
        let builder = build_interact_launch_builder(&user_data_dir, proxy_url);
        let browser_config = builder
            .build()
            .map_err(|e| CrawlError::browser_error(format!("invalid browser config: {e}")))?;

        match Browser::launch(browser_config).await {
            Ok((browser, handler)) => Ok((browser, handler, Some(user_data_dir))),
            Err(e) => {
                let _ = std::fs::remove_dir_all(&user_data_dir);
                Err(CrawlError::browser_error(format!("failed to launch browser: {e}")))
            }
        }
    }
}

/// Build the [`ChromeBrowserConfig`] builder for a fresh interact-mode launch (not the
/// `browser.endpoint` connect branch).
///
/// ~keep Split out from `launch_or_connect` so a test can assert on the flags this
/// ~keep path actually passes without spawning a real Chrome process.
fn build_interact_launch_builder(
    user_data_dir: &std::path::Path,
    proxy_url: Option<&str>,
) -> chromiumoxide::browser::BrowserConfigBuilder {
    let mut builder = ChromeBrowserConfig::builder()
        .no_sandbox()
        .new_headless_mode()
        .user_data_dir(user_data_dir)
        .disable_default_args();
    builder = crate::browser_pool::apply_default_args(builder);
    if let Some(proxy) = proxy_url {
        // ~keep No `--` prefix: chromiumoxide adds it. With one, this rendered as
        // ~keep `----proxy-server=...` and the proxy was silently never applied.
        builder = builder.arg(format!("proxy-server={proxy}"));
    }
    builder
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_interact_launch_builder_carries_no_double_dashed_flag_and_the_macos_keychain_flag() {
        // ~keep Behavioral, not textual: this calls the exact function `launch_or_connect`
        // ~keep uses to build its `BrowserConfig`, so a path that stops calling
        // ~keep `apply_default_args` fails here because the returned flags actually change.
        let builder = build_interact_launch_builder(std::path::Path::new("/tmp/interact-test-profile"), None);
        crate::browser_pool::assert_launch_flags_are_normalized(&builder);
    }

    #[test]
    fn the_interact_launch_builder_still_normalizes_the_proxy_server_flag() {
        let builder = build_interact_launch_builder(
            std::path::Path::new("/tmp/interact-test-profile"),
            Some("http://127.0.0.1:9"),
        );
        let debug = format!("{builder:?}");
        assert!(
            debug.contains("key: \"proxy-server=http://127.0.0.1:9\""),
            "proxy-server flag missing or mis-normalized: {debug}"
        );
    }
}
