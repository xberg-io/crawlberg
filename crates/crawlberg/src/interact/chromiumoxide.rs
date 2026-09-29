use std::time::Duration;

use chromiumoxide::Handler;
use chromiumoxide::browser::{Browser, BrowserConfig as ChromeBrowserConfig};
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::page::ScreenshotParams;
use serde_json::json;
use tokio_stream::StreamExt;

use super::{PageAction, ScrollDirection, encode_screenshot_base64};
use crate::browser_pool::{ExternalTabCleanup, release_browser};
use crate::error::CrawlError;
use crate::types::{ActionResult, BrowserWait, CrawlConfig, InteractionResult};

pub(super) async fn run(
    url: &str,
    actions: &[PageAction],
    config: &CrawlConfig,
) -> Result<InteractionResult, CrawlError> {
    let (browser, mut handler, data_dir) = launch_or_connect(config).await?;
    let handler_handle = tokio::spawn(async move { while handler.next().await.is_some() {} });

    let result = run_with_browser(&browser, url, actions, config).await;

    release_browser(
        browser,
        handler_handle,
        ExternalTabCleanup::default(),
        config.browser.shutdown_timeout,
    )
    .await;
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
    url: &str,
    actions: &[PageAction],
    config: &CrawlConfig,
) -> Result<InteractionResult, CrawlError> {
    let page = browser
        .new_page("about:blank")
        .await
        .map_err(|e| CrawlError::browser_error(format!("failed to create page: {e}")))?;

    let result = async {
        prepare_page(&page, config).await?;
        // ~keep The interception stays on until the page is closed: it adds the seed-host
        // ~keep headers and the SSRF check to requests the actions send too.
        let interceptor = crate::ssrf_intercept::start_ssrf_interception(&page, config).await?;
        let outcome = interact_on_page(&page, url, actions, config, &interceptor).await;
        interceptor.finish().await;
        outcome
    }
    .await;

    let _ = page.close().await;
    result
}

/// Navigate, run the actions, and read the final page, under the caller's interception.
async fn interact_on_page(
    page: &chromiumoxide::Page,
    url: &str,
    actions: &[PageAction],
    config: &CrawlConfig,
    interceptor: &crate::ssrf_intercept::SsrfInterceptGuard,
) -> Result<InteractionResult, CrawlError> {
    navigate_and_wait(page, url, config, interceptor).await?;
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

async fn prepare_page(page: &chromiumoxide::Page, config: &CrawlConfig) -> Result<(), CrawlError> {
    if matches!(config.browser.mode, crate::types::BrowserMode::Stealth) {
        crate::stealth::apply_stealth_patches(page).await;
    }

    if let Some(ref ua) = config.user_agent {
        page.set_user_agent(ua)
            .await
            .map_err(|e| CrawlError::browser_error(format!("failed to set user agent: {e}")))?;
    }

    Ok(())
}

// ~keep Mirrors `browser::navigation::page_fetch`'s interception shape (xberg-io/crawlberg#74):
// ~keep the pre-flight check in `interact::run` only covers the seed URL, and a browser follows
// ~keep redirects/client-side navigations internally, so per-request CDP interception is still
// ~keep needed here to close that gap for this backend the same way the scrape/crawl path does.
async fn navigate_and_wait(
    page: &chromiumoxide::Page,
    url: &str,
    config: &CrawlConfig,
    interceptor: &crate::ssrf_intercept::SsrfInterceptGuard,
) -> Result<(), CrawlError> {
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

    resolve_navigation_outcome(navigation, interceptor.take_blocked(), timeout)?;

    if let Some(extra) = config.browser.extra_wait {
        tokio::time::sleep(extra).await;
    }

    Ok(())
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
        // ~keep Built through `ssrf_violation`, never a struct literal, for the same reason as
        // ~keep `browser::navigation::resolve_navigation_outcome`: `ssrf_intercept` records a URL
        // ~keep with userinfo without it, and `ssrf_violation` redacts again as the last guard.
        // ~keep xberg-io/crawlberg#180.
        return Err(CrawlError::ssrf_violation(blocked_url, reason));
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
        let (browser, handler) = crate::browser_pool::connect_endpoint(endpoint).await?;
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

    /// A refused redirect target that carries `user:pass@` userinfo must be reported with its
    /// credentials redacted.
    ///
    /// ~keep The seed URL is deliberately not the vector: the pre-navigation check refuses a
    /// ~keep credential-bearing seed through an already-redacting path, so a test built on one
    /// ~keep would pass with or without this fix. What leaks is the *intercepted* URL - Chrome
    /// ~keep follows the redirect itself and `Fetch.requestPaused` reports the target verbatim,
    /// ~keep which `ssrf_intercept` records unchanged. xberg-io/crawlberg#180.
    #[test]
    fn a_blocked_url_with_userinfo_is_reported_with_its_credentials_redacted() {
        let blocked = Some((
            "https://user:secret@10.0.0.1/".to_owned(),
            "denied by SSRF policy: private_network".to_owned(),
        ));
        let navigation = Ok(Err(CrawlError::browser_error("navigation failed: net::ERR_FAILED")));

        let error = resolve_navigation_outcome(navigation, blocked, Duration::from_secs(7))
            .expect_err("a blocked request must surface as an error");

        let CrawlError::SsrfPolicyViolation { url, .. } = &error else {
            panic!("expected an SSRF policy violation, got: {error:?}");
        };
        assert_eq!(
            url.as_str(),
            "https://***:***@10.0.0.1/",
            "the refused URL must be stored credential-redacted"
        );
        assert!(
            !error.to_string().contains("secret"),
            "the rendered error must not carry the refused URL's password, got: {error}"
        );
    }

    /// `launch_or_connect`'s connect-error message must never carry a `browser.endpoint`
    /// password or path token, though the failing origin must still be readable for debugging.
    ///
    /// ~keep The launch path has the same test: xberg-io/crawlberg#473 was this test missing
    /// ~keep here after #424 added it only there, so each connect site keeps its own. A closed
    /// ~keep local port refuses the connection immediately, so this needs no real Chrome and
    /// ~keep stays fast; `ws://` skips chromiumoxide's `json/version` HTTP probe and goes
    /// ~keep straight to the WebSocket handshake, so this is the only way to reach this
    /// ~keep function's error path without a real remote browser.
    #[tokio::test]
    async fn connect_error_prints_only_the_endpoint_origin() {
        let config = CrawlConfig {
            browser: crate::types::BrowserConfig {
                endpoint: Some("ws://user:hunter2@127.0.0.1:1/devtools/browser/b1946ac9-guid".into()),
                ..Default::default()
            },
            ..Default::default()
        };

        let err = launch_or_connect(&config)
            .await
            .expect_err("a refused local port must fail the connect");
        let msg = err.to_string();
        assert!(
            !msg.contains("hunter2"),
            "password must not survive into the error, got: {msg}"
        );
        assert!(
            !msg.contains("b1946ac9-guid"),
            "the CDP path token must not survive into the error, got: {msg}"
        );
        assert!(
            msg.contains("127.0.0.1"),
            "host must still appear in the error, got: {msg}"
        );
    }

    /// Every spelling of `browser.endpoint` that the config check accepts must reach the browser.
    #[tokio::test]
    async fn connects_every_endpoint_spelling_the_checks_accept() {
        crate::browser_pool::tests::assert_every_accepted_endpoint_reaches_the_browser(|endpoint| async move {
            let config = CrawlConfig {
                browser: crate::types::BrowserConfig {
                    endpoint: Some(endpoint),
                    ..Default::default()
                },
                ..Default::default()
            };
            launch_or_connect(&config).await
        })
        .await;
    }
}
