use std::time::Duration;

use chromiumoxide::Handler;
use chromiumoxide::browser::{Browser, BrowserConfig as ChromeBrowserConfig};
use chromiumoxide::cdp::browser_protocol::network::{Headers, SetExtraHttpHeadersParams};
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::page::ScreenshotParams;
use serde_json::json;
use tokio_stream::StreamExt;

use super::{PageAction, ScrollDirection, encode_screenshot_base64};
use crate::browser_pool::{ExternalTabCleanup, release_browser};
use crate::chrome_frame::{CommittedDocument, committed_document, read_one_document};
use crate::error::CrawlError;
use crate::net::redact_url_credentials;
use crate::ssrf_intercept::InterceptOutcome;
use crate::types::{ActionResult, AuthConfig, BrowserWait, CrawlConfig, InteractionResult};

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
        navigate_and_wait(&page, url, config).await?;
        if let Some(ref script) = config.browser.eval_script {
            evaluate_json(&page, script).await.map_err(|e| {
                CrawlError::browser_error(format!(
                    "post-navigation eval_script failed before interaction actions: {e}"
                ))
            })?;
        }

        let (action_results, screenshot) = run_actions(&page, actions).await;

        let page_ref = &page;
        let final_html = read_site_content(
            || committed_document(page_ref),
            move || async move {
                page_ref
                    .content()
                    .await
                    .map_err(|e| CrawlError::browser_error(format!("failed to extract final HTML: {e}")))
            },
        )
        .await?;
        let final_url = evaluate_json(&page, "location.href")
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
    .await;

    let _ = page.close().await;
    result
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

// ~keep Mirrors `browser::navigation::page_fetch`'s interception shape (xberg-io/crawlberg#74):
// ~keep the pre-flight check in `interact::run` only covers the seed URL, and a browser follows
// ~keep redirects/client-side navigations internally, so per-request CDP interception is still
// ~keep needed here to close that gap for this backend the same way the scrape/crawl path does.
async fn navigate_and_wait(page: &chromiumoxide::Page, url: &str, config: &CrawlConfig) -> Result<(), CrawlError> {
    let timeout = config.browser.timeout;
    let interceptor = crate::ssrf_intercept::start_ssrf_interception(page, &config.ssrf).await?;

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

    let intercepted = interceptor.finish().await;
    resolve_navigation_outcome(navigation, intercepted, timeout)?;

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
/// ~keep A refused main-frame navigation fails even when the navigation succeeded: the page
/// ~keep navigated again during the wait, and Chrome shows its error page in its place (#369).
/// ~keep A refused subresource alone does not fail a navigation that succeeded.
fn resolve_navigation_outcome(
    navigation: Result<Result<(), CrawlError>, tokio::time::error::Elapsed>,
    intercepted: InterceptOutcome,
    timeout: Duration,
) -> Result<(), CrawlError> {
    // ~keep Built through `ssrf_violation`, never a struct literal, for the same reason as
    // ~keep `browser::navigation::resolve_navigation_outcome`: the blocked URL is the raw
    // ~keep `Fetch.requestPaused` URL, so it still carries any `user:pass@` userinfo the
    // ~keep refused request had. xberg-io/crawlberg#180.
    if let Some((blocked_url, reason)) = intercepted.blocked_navigation {
        return Err(CrawlError::ssrf_violation(blocked_url, reason));
    }
    let navigation_error = match navigation {
        Ok(Ok(())) => return Ok(()),
        Ok(Err(error)) => error,
        Err(_) => CrawlError::browser_timeout(format!("browser timed out after {timeout:?}")),
    };
    if let Some((blocked_url, reason)) = intercepted.blocked {
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
            let bytes = fail_if_run_on_error_page(page, async {
                page.screenshot(params)
                    .await
                    .map_err(|e| CrawlError::browser_error(format!("failed to capture screenshot: {e}")))
            })
            .await?;
            let len = bytes.len();
            Ok(ActionData {
                data: Some(json!({ "bytes": len, "format": "png" })),
                screenshot: Some(bytes),
            })
        }
        PageAction::ExecuteJs { script } => {
            let value = fail_if_run_on_error_page(page, evaluate_json(page, script)).await?;
            Ok(ActionData::data(value))
        }
        PageAction::Scrape => {
            let html = read_site_content(
                || committed_document(page),
                move || async move {
                    page.content()
                        .await
                        .map_err(|e| CrawlError::browser_error(format!("failed to scrape current page: {e}")))
                },
            )
            .await?;
            Ok(ActionData::data(json!({ "html": html })))
        }
    }
}

/// Read the page with `read`, bound to one committed document by [`read_one_document`], and fail
/// when that document is Chrome's own error page, which is never the site's content. A document
/// that cannot be read fails, because a page that cannot be checked is not known to be the site's.
async fn read_site_content<T, D, R>(read_document: impl FnMut() -> D, read: impl FnMut() -> R) -> Result<T, CrawlError>
where
    D: std::future::Future<Output = Result<CommittedDocument, CrawlError>>,
    R: std::future::Future<Output = Result<T, CrawlError>>,
{
    let (value, document) = read_one_document(read_document, read).await?;
    error_page_verdict(document.unreachable_url)?;
    Ok(value)
}

/// Run `action` once, and fail when the document committed just before it started was Chrome's
/// own error page.
///
/// ~keep Checks once, not bound to a document the way [`read_site_content`] binds Scrape's read:
/// ~keep that binding repeats the read when the document changes between its own before and after
/// ~keep check, which is safe for Scrape's `page.content()` but not here. ExecuteJs and Screenshot
/// ~keep can have side effects: a script may navigate the page away from the error page, the same
/// ~keep `history.back()` every error-page test in this file already uses to recover the session.
/// ~keep A fast back-navigation can commit inside the round trip of an after-check, and repeating
/// ~keep the script on that mismatch would run it a second time, overshooting the navigation it
/// ~keep just made (measured: doubles `history.back()` into `about:blank`).
async fn fail_if_run_on_error_page<T>(
    page: &chromiumoxide::Page,
    action: impl std::future::Future<Output = Result<T, CrawlError>>,
) -> Result<T, CrawlError> {
    let document = committed_document(page).await?;
    let value = action.await?;
    error_page_verdict(document.unreachable_url)?;
    Ok(value)
}

/// Judge the main frame's `unreachable_url`. The error for Chrome's error page names the URL Chrome
/// could not show, with its credentials redacted.
fn error_page_verdict(unreachable_url: Option<String>) -> Result<(), CrawlError> {
    match unreachable_url {
        Some(failed_url) => Err(CrawlError::browser_error(format!(
            "Chrome could not load {} and showed its own error page",
            redact_url_credentials(&failed_url)
        ))),
        None => Ok(()),
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
        let intercepted = InterceptOutcome {
            blocked: Some((
                "https://user:secret@10.0.0.1/".to_owned(),
                "denied by SSRF policy: private_network".to_owned(),
            )),
            ..InterceptOutcome::default()
        };
        let navigation = Ok(Err(CrawlError::browser_error("navigation failed: net::ERR_FAILED")));

        let error = resolve_navigation_outcome(navigation, intercepted, Duration::from_secs(7))
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

    fn refused(url: &str) -> Option<(String, String)> {
        Some((url.to_owned(), "denied by SSRF policy: link_local".to_owned()))
    }

    /// A page that navigates to a refused URL during the wait leaves Chrome's error page while the
    /// navigation itself succeeded. The refusal must still fail the session (#369).
    #[test]
    fn a_refused_main_frame_navigation_fails_a_navigation_that_succeeded() {
        let intercepted = InterceptOutcome {
            blocked: refused("http://user:secret@169.254.169.254/latest"),
            blocked_navigation: refused("http://user:secret@169.254.169.254/latest"),
        };

        let error = resolve_navigation_outcome(Ok(Ok(())), intercepted, Duration::from_secs(7))
            .expect_err("a refused main-frame navigation must fail the session");

        let CrawlError::SsrfPolicyViolation { url, .. } = &error else {
            panic!("expected an SSRF policy violation, got: {error:?}");
        };
        assert_eq!(url.as_str(), "http://***:***@169.254.169.254/latest");
    }

    #[test]
    fn a_refused_subresource_does_not_fail_a_navigation_that_succeeded() {
        let intercepted = InterceptOutcome {
            blocked: refused("http://169.254.169.254/pixel.png"),
            ..InterceptOutcome::default()
        };

        assert!(resolve_navigation_outcome(Ok(Ok(())), intercepted, Duration::from_secs(7)).is_ok());
    }

    fn committed(loader_id: &str, unreachable_url: Option<&str>) -> CommittedDocument {
        CommittedDocument {
            loader_id: loader_id.to_owned(),
            unreachable_url: unreachable_url.map(str::to_owned),
        }
    }

    /// Read a scripted page through [`read_site_content`]. `documents[k]` is the document the page
    /// has committed after `k` content reads, and `contents[k]` is what content read `k` returns,
    /// so a commit lands between a content read and the document read that follows it.
    ///
    /// ~keep Scripted because the window between the two CDP calls is one round trip: a real page
    /// ~keep cannot be made to commit inside it on demand.
    async fn read_scripted_page(documents: &[CommittedDocument], contents: &[&str]) -> Result<String, CrawlError> {
        let reads = std::cell::Cell::new(0_usize);
        read_site_content(
            || {
                let document = documents.get(reads.get()).cloned();
                std::future::ready(document.ok_or_else(|| CrawlError::browser_error("no scripted document")))
            },
            || {
                let read = reads.get();
                reads.set(read + 1);
                let content = contents.get(read).map(|content| (*content).to_owned());
                std::future::ready(content.ok_or_else(|| CrawlError::browser_error("no scripted content")))
            },
        )
        .await
    }

    const ERROR_PAGE_HTML: &str = "<html><body>This site can't be reached</body></html>";

    /// The error page is committed when the read starts, and the site's document commits before
    /// the page is checked. The error page's HTML must never come back as the page's content.
    #[tokio::test]
    async fn an_error_page_read_before_a_new_document_commits_is_never_the_content() {
        let error_page = committed("ERROR", Some("http://127.0.0.1/dl"));
        let site = committed("SITE", None);

        let content = read_scripted_page(&[error_page, site.clone(), site], &[ERROR_PAGE_HTML, "<p>site</p>"])
            .await
            .expect("the site's document is committed after the second read");

        assert_eq!(content, "<p>site</p>");
    }

    /// The site's document is read, and the error page commits before the page is checked. The
    /// read is repeated on the error page, which then fails.
    #[tokio::test]
    async fn an_error_page_committed_during_the_read_fails_the_read() {
        let site = committed("SITE", None);
        let error_page = committed("ERROR", Some("http://127.0.0.1/dl"));

        let error = read_scripted_page(
            &[site, error_page.clone(), error_page],
            &["<p>site</p>", ERROR_PAGE_HTML],
        )
        .await
        .expect_err("the read belongs to Chrome's error page");

        assert!(error.to_string().contains("error page"), "{error}");
    }

    #[tokio::test]
    async fn chrome_s_error_page_fails_and_names_the_redacted_url() {
        let error_page = committed("ERROR", Some("http://user:secret@127.0.0.1/dl"));

        let error = read_scripted_page(&[error_page.clone(), error_page], &[ERROR_PAGE_HTML])
            .await
            .expect_err("Chrome's error page must fail");

        let message = error.to_string();
        assert!(
            message.contains("***:***@127.0.0.1/dl") && !message.contains("secret"),
            "{message}"
        );
    }

    #[tokio::test]
    async fn a_page_that_cannot_be_checked_fails_closed() {
        let error = read_scripted_page(&[], &[ERROR_PAGE_HTML])
            .await
            .expect_err("a page that cannot be checked must fail");

        assert!(error.to_string().contains("no scripted document"), "{error}");
    }

    #[tokio::test]
    async fn the_site_s_document_is_the_content() {
        let site = committed("SITE", None);

        let content = read_scripted_page(&[site.clone(), site], &["<p>site</p>"]).await;

        assert_eq!(content.ok().as_deref(), Some("<p>site</p>"));
    }
}
