use std::sync::Arc;
use std::time::Duration;

use chromiumoxide::Handler;
use chromiumoxide::browser::{Browser, BrowserConfig as ChromeBrowserConfig};
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::page::ScreenshotParams;
use serde_json::json;

use super::{PageAction, ScrollDirection, encode_screenshot_base64};
use crate::browser_pool::{ExternalTabCleanup, ScratchProfileDir, kill_browser, release_browser, spawn_handler};
use crate::chrome_frame::{
    CommittedDocument, committed_document, error_page_error, page_content, read_one_document, read_one_document_within,
};
use crate::error::CrawlError;
use crate::ssrf_intercept::{
    ACTION_GRACE, BrowserFirewall, BrowserOrigin, INPUT_ACTION_GRACE, PageContext, StoppedResponse, Watch,
    listed_refusal,
};
use crate::types::{ActionResult, BrowserWait, CrawlConfig, InteractionResult};

pub(super) async fn run(
    url: &str,
    actions: &[PageAction],
    config: &CrawlConfig,
) -> Result<InteractionResult, CrawlError> {
    run_launched(launch_or_connect(config).await?, url, actions, config).await
}

/// Run `actions` in a browser [`launch_or_connect`] returned, then tear the browser down and
/// remove its profile directory.
async fn run_launched(
    (browser, handler, data_dir): Launched,
    url: &str,
    actions: &[PageAction],
    config: &CrawlConfig,
) -> Result<InteractionResult, CrawlError> {
    let handler_handle = spawn_handler(handler);

    let browser = Arc::new(browser);
    // ~keep A launched browser (always with a throwaway profile here) is killed with interception
    // ~keep still on, never turned off, not even once the page is closed. Under load Chrome can
    // ~keep take longer than the watch's close bound to destroy a page or popup, and a page can
    // ~keep still send just after Chrome reports it destroyed. Turning interception off, or a
    // ~keep graceful `Browser.close` (which ends the DevTools session first), lets those requests
    // ~keep out (xberg-io/crawlberg#468).
    let origin = BrowserOrigin::of_session(config.browser.endpoint.as_deref(), data_dir.is_some());
    let result = match BrowserFirewall::start(
        Arc::clone(&browser),
        origin,
        PageContext::of_endpoint(config.browser.endpoint.as_deref()),
    )
    .await
    {
        Ok(firewall) => {
            let result = run_with_browser(&firewall, url, actions, config).await;
            firewall.stop().await;
            result
        }
        Err(error) => Err(error),
    };

    // ~keep The stopped firewall held the only other reference, so this is the browser itself.
    let shutdown_timeout = config.browser.shutdown_timeout;
    match (Arc::into_inner(browser), data_dir) {
        (Some(browser), Some(profile)) if origin == BrowserOrigin::Killed => {
            kill_browser(browser, handler_handle, profile.path().to_path_buf(), shutdown_timeout).await;
        }
        (browser, profile) => {
            match browser {
                Some(browser) => {
                    release_browser(browser, handler_handle, ExternalTabCleanup::default(), shutdown_timeout).await;
                }
                None => handler_handle.abort(),
            }
            drop(profile);
        }
    }

    result
}

/// Run every action in order, collecting one [`ActionResult`] each and the last screenshot taken.
///
/// A failing action is recorded and the run continues, so the caller always gets one result per
/// requested action. An action that sent a request the SSRF check refused fails with the policy
/// error.
async fn run_actions(
    page: &chromiumoxide::Page,
    watch: &Watch,
    actions: &[PageAction],
) -> (Vec<ActionResult>, Option<Vec<u8>>) {
    let mut action_results = Vec::with_capacity(actions.len());
    let mut screenshot = None;

    for (index, action) in actions.iter().enumerate() {
        let started = std::time::Instant::now();
        let outcome = run_action_with_timeout(page, action, index).await;
        let grace = match action {
            PageAction::Click { .. } | PageAction::Press { .. } | PageAction::TypeText { .. } => INPUT_ACTION_GRACE,
            _ => ACTION_GRACE,
        };
        let outcome = match watch.refusal_during(started, grace).await {
            Some((url, reason)) => Err(CrawlError::ssrf_violation(url, reason)),
            None => outcome,
        };
        match outcome {
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
    firewall: &BrowserFirewall,
    url: &str,
    actions: &[PageAction],
    config: &CrawlConfig,
) -> Result<InteractionResult, CrawlError> {
    // ~keep A launched Chrome has the proxy from `--proxy-server`; a connected one never got
    // ~keep that flag, so there the page's own browser context is made with the proxy. Under
    // ~keep `deny_private` the context goes through the SSRF proxy, which leaves through it.
    let proxy = if config.browser.endpoint.is_some() || config.ssrf.deny_private {
        crate::proxy::chrome_proxy_for(config)?
    } else {
        None
    };
    let sockets = crate::net::egress::socket_policy(
        &config.ssrf,
        config.browser.endpoint.as_deref(),
        &std::sync::Once::new(),
    );
    let page = firewall.handle().new_page(proxy.as_ref(), sockets).await?;

    // ~keep The SSRF check holds for the whole session, not just the first navigation: the
    // ~keep actions click, submit forms and run scripts, and each can send the page, a frame,
    // ~keep a worker or a popup to an address the policy refuses (xberg-io/crawlberg#153).
    // ~keep Closing the watch disposes the page's browser context, which takes the page, its
    // ~keep popups and their pending requests, and stops watching only once Chrome has
    // ~keep destroyed them, so the check answers until then.
    match firewall.handle().watch(&page, config, config.max_redirects).await {
        Ok(watch) => {
            let result = async {
                prepare_page(&page, config).await?;
                run_session(&page, &watch, url, actions, config).await
            }
            .await;
            watch.close().await;
            // ~keep The session has one page, so each socket its SSRF proxy refused is that page's.
            let mut result = result?;
            crate::net::egress::add_refused(&mut result.ssrf_refused_urls, firewall.handle().egress_refused().await);
            Ok(result)
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
    ensure_committed_page_is_readable(page, watch, config.browser.timeout).await?;
    if let Some(ref script) = config.browser.eval_script {
        let budget = config.browser.timeout;
        match tokio::time::timeout(budget, evaluate_json(page, script)).await {
            Ok(result) => {
                result.map_err(|e| {
                    CrawlError::browser_error(format!(
                        "post-navigation eval_script failed before interaction actions: {e}"
                    ))
                })?;
            }
            Err(_) => {
                return Err(CrawlError::browser_timeout(format!(
                    "post-navigation eval_script timed out after {budget:?}"
                )));
            }
        }
    }

    let (action_results, screenshot) = run_actions(page, watch, actions).await;

    let (final_html, final_url) = final_page(page, &watch.refused_urls().await, config.browser.timeout).await?;

    let screenshot_base64 = screenshot.as_deref().map(encode_screenshot_base64);

    Ok(InteractionResult {
        action_results,
        final_html,
        final_url,
        screenshot,
        screenshot_base64,
        ssrf_refused_urls: watch.refused_urls().await,
    })
}

async fn ensure_committed_page_is_readable(
    page: &chromiumoxide::Page,
    watch: &Watch,
    budget: Duration,
) -> Result<(), CrawlError> {
    if let Some(error) = unreadable_committed_success_error(watch) {
        return Err(error);
    }
    let document = tokio::time::timeout(budget, committed_document(page))
        .await
        .map_err(|_| {
            CrawlError::browser_timeout(format!(
                "browser timed out after {budget:?} checking the committed document before interaction actions"
            ))
        })??;
    if let Some(error) = unreadable_success_error(&document, watch) {
        return Err(error);
    }
    let Some(failed_url) = document.unreachable_url else {
        return Ok(());
    };
    Err(error_page_error(&failed_url))
}

fn unreadable_success_error(document: &CommittedDocument, watch: &Watch) -> Option<CrawlError> {
    let failed_url = document.unreachable_url.as_deref()?;
    let response = watch.document(&document.loader_id)?;
    unreadable_response_error(failed_url, response.status)
}

fn unreadable_committed_success_error(watch: &Watch) -> Option<CrawlError> {
    let (failed_url, response) = watch.committed_error_response()?;
    unreadable_response_error(&failed_url, response.status)
}

fn unreadable_response_error(failed_url: &str, status: u16) -> Option<CrawlError> {
    (200..300).contains(&status).then(|| {
        CrawlError::browser_error(format!(
            "Chrome could not read the body of the HTTP {status} response from {}",
            crate::net::redact_url_credentials(failed_url)
        ))
    })
}

async fn classify_unreadable_success_within(
    page: &chromiumoxide::Page,
    watch: &Watch,
    budget: Duration,
) -> Option<CrawlError> {
    if let Some(error) = unreadable_committed_success_error(watch) {
        return Some(error);
    }
    let document = tokio::time::timeout(budget, committed_document(page))
        .await
        .ok()?
        .ok()?;
    unreadable_success_error(&document, watch)
}

/// The final HTML and URL of the session, both of one committed document.
///
/// ~keep Chrome's own error page is never the final HTML. When it shows a navigation the SSRF check
/// ~keep refused, the refusal already failed the action that caused it and is in `refused`, so the
/// ~keep session keeps its result, with no HTML and the refused URL as it is listed. Any other error
/// ~keep page fails the session.
///
/// ~keep The page's reads share one `budget`, the session's `browser.timeout`.
async fn final_page(
    page: &chromiumoxide::Page,
    refused: &[String],
    budget: Duration,
) -> Result<(String, String), CrawlError> {
    let (html, document) = read_one_document_within(
        budget,
        || committed_document(page),
        || page_content(page, "extract final HTML"),
    )
    .await?;
    if let Some(failed_url) = document.unreachable_url {
        return match listed_refusal(&failed_url, refused) {
            Some(listed) => Ok((String::new(), listed)),
            None => Err(error_page_error(&failed_url)),
        };
    }
    Ok((html, document.url))
}

/// The HTML of `page` and the committed document it was read from, bound by
/// [`read_one_document`]. `what` names the read in its error.
async fn read_page_html(page: &chromiumoxide::Page, what: &str) -> Result<(String, CommittedDocument), CrawlError> {
    read_one_document(|| committed_document(page), || page_content(page, what)).await
}

/// The result of a navigation that ended on a terminal response: its URL and body, and a failed
/// result per action, since crawlberg stopped the navigation before an actionable page.
///
/// ~keep `scrape` reports the original terminal response metadata, but a redirect body is
/// ~keep intentionally empty because Chrome receives a complete synthetic inert document.
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
        final_html: stop.body.clone(),
        final_url: stop.url.clone(),
        screenshot: None,
        screenshot_base64: None,
        ssrf_refused_urls: Vec::new(),
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

    Ok(())
}

/// Navigate to `url` and wait for the page. Returns the response the navigation stopped on
/// when it has no document: the redirect past `max_redirects`, or a 204, 205 or 304.
/// Fails with the SSRF policy error when a main-frame navigation was refused, during the load
/// or the extra wait.
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
        let goto = watch
            .goto(page, url)
            .await
            .map_err(|e| CrawlError::browser_error(format!("navigation failed: {e}")));
        if let Some(unreadable) = classify_unreadable_success_within(page, watch, timeout).await {
            return (Err(unreadable), true);
        }
        if let Err(error) = goto {
            return (Err(error), false);
        }
        let ready = wait_for_ready(page, config)
            .await
            .map_err(|e| CrawlError::browser_error(format!("wait failed: {e}")));
        (ready, false)
    })
    .await;
    let (navigation, already_classified) = match navigation {
        Ok((result, classified)) => (Ok(result), classified),
        Err(elapsed) => (Err(elapsed), false),
    };

    watch.settle().await;
    let mut intercepted = watch.take_outcome();
    if let Some(stop) = intercepted.take_intentional_terminal() {
        watch.mark_unsettled();
        return Ok(Some(stop));
    }
    let was_blocked = intercepted.blocked.is_some();
    if let Err(error) = resolve_navigation_outcome(navigation, intercepted.blocked, timeout) {
        watch.mark_unsettled();
        let should_classify = !was_blocked && !already_classified && matches!(&error, CrawlError::BrowserError { .. });
        if should_classify && let Some(unreadable) = classify_unreadable_success_within(page, watch, timeout).await {
            return Err(unreadable);
        }
        return Err(error);
    }
    if let Some(stop) = intercepted.stopped_response {
        return Ok(Some(stop));
    }

    if let Some(extra) = config.browser.extra_wait {
        tokio::time::sleep(extra).await;
    }
    // ~keep A main-frame navigation the policy refused before the actions leaves Chrome's error
    // ~keep page in place of the page, so the session fails as a scrape does. One an action
    // ~keep starts fails that action instead.
    watch.settle().await;
    if let Some((blocked_url, reason)) = watch.blocked_navigation() {
        return Err(CrawlError::ssrf_violation(blocked_url, reason));
    }
    if let Some(stop) = watch.take_stopped_response_within(timeout).await? {
        if stop.terminal_intercepted {
            watch.mark_unsettled();
        }
        return Ok(Some(stop));
    }
    // ~keep The redirect limit bounds the navigation to `url`. A navigation an action starts is
    // ~keep the caller's own, so it is not counted.
    watch.end_navigation();

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
            // ~keep Chrome's own error page is never the site's content, so a Scrape on it fails.
            let (html, document) = read_page_html(page, "scrape current page").await?;
            if let Some(failed_url) = document.unreachable_url {
                return Err(error_page_error(&failed_url));
            }
            Ok(ActionData::data(json!({ "html": html })))
        }
    }
}

/// Run `action` once, and fail when the document committed just before it started was Chrome's
/// own error page.
///
/// ~keep Checks once, not bound to a document the way [`read_page_html`] binds a read: that
/// ~keep binding repeats the read when the document changes between its own before and after
/// ~keep check, which is safe for `page.content()` but not here. ExecuteJs and Screenshot can have
/// ~keep side effects: a script may navigate the page away from the error page, as
/// ~keep `history.back()` does, and a fast back-navigation can commit inside the round trip of an
/// ~keep after-check. Repeating the script on that mismatch would run it a second time. The action
/// ~keep still runs, so a script that leaves the error page can recover the session.
async fn fail_if_run_on_error_page<T>(
    page: &chromiumoxide::Page,
    action: impl std::future::Future<Output = Result<T, CrawlError>>,
) -> Result<T, CrawlError> {
    let document = committed_document(page).await?;
    let value = action.await?;
    if let Some(failed_url) = &document.unreachable_url {
        return Err(error_page_error(failed_url));
    }
    Ok(value)
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

/// A launched or connected browser, its CDP handler, and the profile directory of a launched one.
type Launched = (Browser, Handler, Option<ScratchProfileDir>);

async fn launch_or_connect(config: &CrawlConfig) -> Result<Launched, CrawlError> {
    if let Some(ref endpoint) = config.browser.endpoint {
        crate::types::warn_ignored_launch_options(
            &config.browser,
            "connecting to an external browser.endpoint, whose Chrome process is launched externally",
        );
        let (browser, handler) = crate::browser_pool::connect_endpoint(endpoint).await?;
        Ok((browser, handler, None))
    } else {
        // ~keep Removed on drop, so a failed or cancelled launch or run removes it too.
        let user_data_dir = ScratchProfileDir::create("crawlberg-interact-", config.browser.chrome_path.as_deref())?;

        let proxy = crate::proxy::chrome_proxy_for(config)?;
        if config.ssrf.deny_private {
            crate::browser_pool::disable_non_proxied_udp(user_data_dir.path())?;
        }
        let browser_config = build_interact_launch_builder(user_data_dir.path(), proxy.as_ref(), &config.browser)?
            .build()
            .map_err(|e| CrawlError::browser_error(format!("invalid browser config: {e}")))?;

        let (mut browser, handler, user_data_dir) = user_data_dir
            .launch(browser_config)
            .await
            .map_err(|e| CrawlError::browser_error(format!("failed to launch browser: {e}")))?;
        if config.ssrf.deny_private {
            crate::browser_pool::confirm_profile_in_use(&mut browser, user_data_dir.path()).await?;
        }
        Ok((browser, handler, Some(user_data_dir)))
    }
}

/// Build the [`ChromeBrowserConfig`] builder for a fresh interact-mode launch (not the
/// `browser.endpoint` connect branch).
///
/// ~keep Split out from `launch_or_connect` so a test can assert on the flags this
/// ~keep path actually passes without spawning a real Chrome process.
fn build_interact_launch_builder(
    user_data_dir: &std::path::Path,
    proxy: Option<&crate::proxy::ChromeProxy>,
    browser: &crate::types::BrowserConfig,
) -> Result<chromiumoxide::browser::BrowserConfigBuilder, CrawlError> {
    let mut builder = ChromeBrowserConfig::builder()
        .no_sandbox()
        .new_headless_mode()
        .user_data_dir(user_data_dir)
        .disable_default_args();
    builder = crate::browser_pool::apply_default_args(builder, &browser.chrome_args);
    crate::browser_pool::apply_launch_overrides(
        builder,
        "browser",
        browser.chrome_path.as_deref(),
        &browser.chrome_args,
        proxy,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[allow(clippy::print_stderr, reason = "test-only skip announcement")]
    async fn a_chrome_args_flag_reaches_the_chrome_interact_starts() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        const TEST_NAME: &str = "a_chrome_args_flag_reaches_the_chrome_interact_starts";
        const MARKER: &str = "crawlberg-interact-chrome-args-marker";
        let site = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/"))
            .and(header("user-agent", MARKER))
            .respond_with(ResponseTemplate::new(200).set_body_raw("<p>marker received</p>", "text/html"))
            .mount(&site)
            .await;
        let config = CrawlConfig {
            browser: crate::types::BrowserConfig {
                chrome_args: vec![format!("--user-agent={MARKER}")],
                ..crate::types::BrowserConfig::default()
            },
            ..CrawlConfig::builder().allow_private_networks(true).build()
        };
        let result = run(&site.uri(), &[], &config).await;
        let Some(result) = crate::browser_pool::tests::expect_chrome_or_skip(TEST_NAME, result) else {
            return;
        };
        assert!(
            result.final_html.contains("marker received"),
            "the interact Chrome must request the page with the caller flag: {}",
            result.final_html
        );
    }

    /// An interact run removes the profile directory of the Chrome it launched, with no Chrome
    /// process left using it.
    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::print_stderr, reason = "test-only skip announcement")]
    async fn an_interact_run_leaves_no_profile_directory_and_no_chrome_using_it() {
        const TEST_NAME: &str = "an_interact_run_leaves_no_profile_directory_and_no_chrome_using_it";
        let config = CrawlConfig::default();
        let Some(launched) =
            crate::browser_pool::tests::expect_chrome_or_skip(TEST_NAME, launch_or_connect(&config).await)
        else {
            return;
        };
        let path = launched
            .2
            .as_ref()
            .map(|dir| dir.path().to_path_buf())
            .expect("a launched Chrome must have a profile directory");
        assert!(path.is_dir(), "the profile directory must exist while Chrome runs");

        let before = crate::browser_pool::tests::profile_drops_here();
        let _ = run_launched(launched, "about:blank", &[], &config).await;
        crate::browser_pool::tests::assert_profile_teardown_left_this_thread(before);

        tokio::task::spawn_blocking(move || {
            crate::browser_pool::tests::assert_profile_directory_is_gone_for_good(&path)
        })
        .await
        .expect("an interact run must stop its Chrome and remove its profile directory");
    }

    /// An interact session whose Chrome dies during the session ends with an error. It does not
    /// wait for replies that the dead connection never sends (xberg-io/crawlberg#577).
    ///
    /// ~keep The page fetches `/loaded` once it has loaded, and the test kills Chrome's main process
    /// ~keep when that request arrives, so the kill lands inside the session, before its wait action
    /// ~keep ends. The session's next reads and the close of its page then go to a dead connection.
    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::print_stderr, reason = "test-only skip announcement")]
    async fn an_interact_session_whose_chrome_dies_ends_with_an_error() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let site = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                "<html><body><p>start</p><script>addEventListener('load', () => fetch('/loaded'));</script></body></html>",
                "text/html",
            ))
            .mount(&site)
            .await;
        Mock::given(method("GET"))
            .and(path("/loaded"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&site)
            .await;
        let seed = format!("http://localhost:{}/", site.address().port());
        let config = CrawlConfig {
            browser: crate::types::BrowserConfig {
                timeout: Duration::from_secs(10),
                ..crate::types::BrowserConfig::default()
            },
            ..CrawlConfig::builder()
                .ssrf_allowlist_host(crate::net::ssrf::HostMatcher::exact("localhost"))
                .build()
        };
        let test_name = "an_interact_session_whose_chrome_dies_ends_with_an_error";
        let Some(mut launched) =
            crate::browser_pool::tests::expect_chrome_or_skip(test_name, launch_or_connect(&config).await)
        else {
            return;
        };
        let chrome = launched
            .0
            .get_mut_child()
            .and_then(|child| child.as_mut_inner().id())
            .map(sysinfo::Pid::from_u32)
            .expect("a launched Chrome must have a process");
        let killer = tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
            while tokio::time::Instant::now() < deadline {
                let received = site.received_requests().await.unwrap_or_default();
                if received.iter().any(|request| request.url.path() == "/loaded") {
                    let mut system = sysinfo::System::new();
                    system.refresh_processes_specifics(
                        sysinfo::ProcessesToUpdate::Some(&[chrome]),
                        true,
                        sysinfo::ProcessRefreshKind::nothing(),
                    );
                    return system
                        .process(chrome)
                        .and_then(|process| process.kill_with(sysinfo::Signal::Kill))
                        .unwrap_or(false);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            false
        });
        let actions = [PageAction::Wait {
            milliseconds: Some(3_000),
            selector: None,
        }];
        let outcome = tokio::time::timeout(
            Duration::from_secs(60),
            run_launched(launched, &seed, &actions, &config),
        )
        .await;
        let killed = killer.await.expect("the task that kills Chrome must not panic");

        assert!(killed, "Chrome must be killed once the page has loaded");
        match outcome {
            Ok(result) => assert!(result.is_err(), "a session whose Chrome died must fail, got {result:?}"),
            Err(_) => panic!("the session must end once its Chrome has died, not wait 60 s and more"),
        }
    }

    /// An interact launch that fails drops its profile directory, off the executor thread.
    ///
    /// ~keep No Chrome is needed: `chrome_path` names a script that exits at once, so the check
    /// ~keep on the binary passes and the launch itself fails. The test goes through
    /// ~keep `launch_or_connect` itself, so a call site that stops dropping the directory on a
    /// ~keep failed launch fails here.
    #[tokio::test]
    async fn a_failed_interact_launch_removes_its_profile_directory() {
        let not_chrome = crate::types::executable_temp_file("interact-launch");
        let config = CrawlConfig {
            browser: crate::types::BrowserConfig {
                chrome_path: Some(not_chrome.clone()),
                ..Default::default()
            },
            ..Default::default()
        };
        let before = crate::browser_pool::tests::profile_drops_here();

        let launched = launch_or_connect(&config).await;

        let _ = std::fs::remove_file(&not_chrome);
        let Err(error) = launched else {
            panic!("a launch of a binary that is not Chrome must fail");
        };
        assert!(
            error.to_string().contains("failed to launch browser"),
            "the launch itself must fail, not the check on the binary: {error}"
        );
        crate::browser_pool::tests::assert_profile_teardown_left_this_thread(before);
    }

    /// A refused `chrome_path` removes the scratch directory `launch_or_connect` created for the
    /// launch it never made.
    ///
    /// ~keep Pins the call site in `launch_or_connect`: `ScratchProfileDir::create(..)?` then
    /// ~keep `build_interact_launch_builder(..)?`, whose `?` drops the guard on a refusal. No
    /// ~keep Chrome is needed: the check on the path fails before any process would be spawned.
    #[tokio::test]
    async fn an_interact_launch_refused_by_a_missing_chrome_path_leaves_no_profile_directory() {
        let config = CrawlConfig {
            browser: crate::types::BrowserConfig {
                chrome_path: Some(std::path::PathBuf::from("/nonexistent/crawlberg-interact-chrome")),
                ..Default::default()
            },
            ..Default::default()
        };
        let error =
            crate::browser_pool::tests::assert_refused_launch_leaves_no_scratch_dir(|| launch_or_connect(&config))
                .await;
        assert!(
            error.contains("cannot be used"),
            "the error must name the path, got: {error}"
        );
    }

    /// The same call site refused by a `chrome_args` entry instead of `chrome_path`.
    #[tokio::test]
    async fn an_interact_launch_refused_by_a_user_data_dir_flag_leaves_no_profile_directory() {
        let config = CrawlConfig {
            browser: crate::types::BrowserConfig {
                chrome_args: vec!["--user-data-dir=/tmp/crawlberg-interact-elsewhere".to_owned()],
                ..Default::default()
            },
            ..Default::default()
        };
        let error =
            crate::browser_pool::tests::assert_refused_launch_leaves_no_scratch_dir(|| launch_or_connect(&config))
                .await;
        assert!(
            error.contains("must not set --user-data-dir"),
            "the error must name the refused flag, got: {error}"
        );
    }

    /// An interact launch records the Chrome it starts, so dropping its profile directory stops
    /// that Chrome and removes the directory.
    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::print_stderr, reason = "test-only skip announcement")]
    async fn dropping_an_interact_launchs_profile_directory_stops_its_chrome() {
        let test_name = "dropping_an_interact_launchs_profile_directory_stops_its_chrome";
        let Some((browser, handler, dir)) = crate::browser_pool::tests::expect_chrome_or_skip(
            test_name,
            launch_or_connect(&CrawlConfig::default()).await,
        ) else {
            return;
        };
        let dir = dir.expect("a launched Chrome must have a profile directory");
        let path = dir.path().to_path_buf();

        crate::browser_pool::tests::assert_dropping_the_profile_stops_its_chrome(browser, handler, dir, path).await;
    }

    /// An interact run cut off during its launch hands its profile teardown off the executor thread.
    ///
    /// ~keep No Chrome is needed: without one the launch fails before the timeout, and the profile
    /// ~keep directory drops on the same path.
    #[tokio::test]
    async fn a_cancelled_interact_run_tears_its_profile_down_off_the_executor_thread() {
        let config = CrawlConfig::default();
        let before = crate::browser_pool::tests::profile_drops_here();

        let _ = tokio::time::timeout(std::time::Duration::from_millis(1), run("about:blank", &[], &config)).await;

        crate::browser_pool::tests::assert_profile_teardown_left_this_thread(before);
    }

    #[test]
    fn the_interact_launch_builder_carries_no_double_dashed_flag_and_the_macos_keychain_flag() {
        // ~keep Behavioral, not textual: this calls the exact function `launch_or_connect`
        // ~keep uses to build its `BrowserConfig`, so a path that stops calling
        // ~keep `apply_default_args` fails here because the returned flags actually change.
        let builder = build_interact_launch_builder(
            std::path::Path::new("/tmp/interact-test-profile"),
            None,
            &crate::types::BrowserConfig::default(),
        )
        .expect("the default browser config names no binary to check");
        crate::browser_pool::assert_launch_flags_are_normalized(&builder);
    }

    /// The Chrome proxy of the proxy address `url`.
    fn test_proxy(url: &str) -> crate::proxy::ChromeProxy {
        crate::proxy::chrome_proxy(&crate::types::ProxyConfig {
            url: url.into(),
            ..Default::default()
        })
        .expect("an http proxy is a Chrome proxy")
    }

    #[test]
    fn the_configured_proxy_replaces_a_caller_proxy_flag() {
        let proxy = test_proxy("http://127.0.0.1:9");
        for (caller_flag, caller_value) in [
            ("--proxy-server=http://127.0.0.1:7", "127.0.0.1:7"),
            ("--proxy-bypass-list=*.internal", "*.internal"),
            ("--proxy-pac-url=http://127.0.0.1:7/p.pac", "127.0.0.1:7/p.pac"),
            ("--no-proxy-server", "no-proxy-server"),
            ("--proxy-auto-detect", "proxy-auto-detect"),
        ] {
            let browser = crate::types::BrowserConfig {
                chrome_args: vec![caller_flag.to_owned()],
                ..Default::default()
            };
            let (built, fields) = crate::tracing_capture::capture_events(|| {
                build_interact_launch_builder(
                    std::path::Path::new("/tmp/interact-test-profile"),
                    Some(&proxy),
                    &browser,
                )
            });
            let debug = format!("{:?}", built.expect("no binary is named, so there is nothing to check"));
            for configured in ["proxy-server=http://127.0.0.1:9", "proxy-bypass-list=<-loopback>"] {
                assert!(
                    debug.contains(&format!("key: \"{configured}\"")),
                    "{caller_flag}: the configured proxy's {configured} is missing: {debug}"
                );
            }
            assert!(
                !debug.contains(caller_value),
                "{caller_flag}: the caller's flag must be dropped: {debug}"
            );
            let switch = caller_flag.split('=').next().expect("a switch name");
            // ~keep A switch with no value has no secret to hide; its name is what the warning prints.
            if caller_flag.contains('=') {
                crate::tracing_capture::assert_logged_without_secret(&fields, caller_value, switch);
            }
        }
    }

    #[test]
    fn the_interact_launch_builder_uses_the_configured_chrome_path_and_args() {
        crate::browser_pool::assert_launch_overrides_reach_the_builder(|chrome_path, chrome_args| {
            build_interact_launch_builder(
                std::path::Path::new("/tmp/interact-test-profile"),
                Some(&test_proxy("http://127.0.0.1:9")),
                &crate::types::BrowserConfig {
                    chrome_path,
                    chrome_args,
                    ..Default::default()
                },
            )
        });
    }

    #[test]
    fn the_interact_launch_builder_still_normalizes_the_proxy_server_flag() {
        let proxy = test_proxy("http://127.0.0.1:9");
        let builder = build_interact_launch_builder(
            std::path::Path::new("/tmp/interact-test-profile"),
            Some(&proxy),
            &crate::types::BrowserConfig::default(),
        )
        .expect("the default browser config names no binary to check");
        let debug = format!("{builder:?}");
        assert!(
            debug.contains("key: \"proxy-server=http://127.0.0.1:9\""),
            "proxy-server flag missing or mis-normalized: {debug}"
        );
    }

    #[test]
    fn the_interact_launch_takes_the_proxy_as_chrome_can_read_it() {
        for (raw, server) in [
            ("127.0.0.1:3128", "http://127.0.0.1:3128"),
            ("http:proxy.test:1", "http://proxy.test:1"),
        ] {
            let config = CrawlConfig {
                proxy: Some(crate::types::ProxyConfig {
                    url: raw.into(),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let proxy = crate::proxy::chrome_proxy_for(&config).expect("a usable proxy");
            let builder = build_interact_launch_builder(
                std::path::Path::new("/tmp/interact-test-profile"),
                proxy.as_ref(),
                &crate::types::BrowserConfig::default(),
            )
            .expect("the default browser config names no binary to check");
            let debug = format!("{builder:?}");
            assert!(
                debug.contains(&format!("key: \"proxy-server={server}\"")),
                "{raw}: Chrome must get {server}, got {debug}"
            );
        }
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
    /// ~keep straight to the WebSocket handshake. The endpoint-listener test just below reaches
    /// ~keep the same error path with a local socket that answers HTTP 418, so a closed port is
    /// ~keep no longer the only way here; it stays because it needs no listener at all.
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

    #[allow(
        clippy::print_stderr,
        reason = "test-only skip announcement, matching tests/common/mod.rs's convention"
    )]
    async fn interact_teardown_hits(
        mut site: crate::ssrf_intercept::SendingSite,
        defense: crate::ssrf_intercept::SessionRequestDefense,
        test_name: &str,
    ) -> Option<usize> {
        let chrome = match chromiumoxide::detection::default_executable(Default::default()) {
            Ok(chrome) => chrome,
            Err(message) => {
                eprintln!("skipping {test_name}: no usable Chrome: {message}");
                return None;
            }
        };
        site.config.browser.chrome_path = Some(chrome);
        let expected_open = site.popup.as_deref().unwrap_or(&site.seed).to_owned();
        let actions = site.action_script.as_ref().map_or_else(Vec::new, |script| {
            vec![PageAction::ExecuteJs { script: script.clone() }]
        });
        const HOLD: Duration = Duration::from_secs(1);
        let (result, stop_hold) =
            crate::ssrf_intercept::with_session_page_left_open(HOLD, defense, run(&site.seed, &actions, &site.config))
                .await;
        match result {
            Ok(_) | Err(CrawlError::SsrfPolicyViolation { .. } | CrawlError::BrowserTimeout { .. }) => {}
            Err(error) => panic!("{test_name}: the session must end: {error:?}"),
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        while !stop_hold.open_pages().iter().any(|url| url == &expected_open) && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        tokio::time::sleep(HOLD + Duration::from_secs(1)).await;
        assert!(
            stop_hold.open_pages().iter().any(|url| url == &expected_open),
            "{test_name}: {expected_open} must still be open after the stop, or the test proves nothing, open: {:?}",
            stop_hold.open_pages()
        );
        Some(site.denied_hits().await)
    }

    async fn assert_interact_teardown_refused(site: crate::ssrf_intercept::SendingSite, test_name: &str) {
        let Some(hits) =
            interact_teardown_hits(site, crate::ssrf_intercept::SessionRequestDefense::Both, test_name).await
        else {
            return;
        };
        assert_eq!(
            hits, 0,
            "{test_name}: a page still sending after the check stopped must not reach the denied address, \
             got {hits} requests"
        );
    }

    /// An interact session keeps refusing its page while the page sends after the check stops.
    ///
    /// ~keep The hook holds one sending page open across the stop, replacing the request flood
    /// ~keep that timed out on three-core runners (xberg-io/crawlberg#580).
    #[tokio::test(flavor = "multi_thread")]
    async fn interact_refuses_requests_the_page_sends_while_the_session_ends() {
        assert_interact_teardown_refused(
            crate::ssrf_intercept::SendingSite::start().await,
            "interact_refuses_requests_the_page_sends_while_the_session_ends",
        )
        .await;
    }

    /// An interact session keeps refusing its popup while the popup sends after the check stops.
    ///
    /// ~keep The assertion requires the popup URL to be open during the forced post-stop window,
    /// ~keep so a popup that never opened cannot make the denied-request count pass vacuously (#580).
    #[tokio::test(flavor = "multi_thread")]
    async fn interact_refuses_requests_a_popup_sends_while_the_session_ends() {
        assert_interact_teardown_refused(
            crate::ssrf_intercept::SendingSite::start_popup().await,
            "interact_refuses_requests_a_popup_sends_while_the_session_ends",
        )
        .await;
    }

    /// An interact session keeps refusing popups its page opens while the check stops.
    ///
    /// ~keep The retained page opens denied navigations throughout the forced post-stop window;
    /// ~keep the same fixture's no-defense control below proves those attempts reach the server.
    #[tokio::test(flavor = "multi_thread")]
    async fn interact_refuses_popups_the_page_keeps_opening_while_the_session_ends() {
        assert_interact_teardown_refused(
            crate::ssrf_intercept::SendingSite::start_opening_denied_popups().await,
            "interact_refuses_popups_the_page_keeps_opening_while_the_session_ends",
        )
        .await;
    }

    async fn assert_interact_teardown_reaches(site: crate::ssrf_intercept::SendingSite, test_name: &str) {
        let Some(hits) =
            interact_teardown_hits(site, crate::ssrf_intercept::SessionRequestDefense::Neither, test_name).await
        else {
            return;
        };
        assert!(
            hits > 0,
            "{test_name}: the negative control must reach the denied address or the property checks prove nothing"
        );
    }

    /// The page teardown window reaches the denied server with both defenses disabled.
    ///
    /// ~keep This negative control uses the same page fixture, interact path and hook as its
    /// ~keep property test; a zero count means that property could pass without a request (#580).
    #[tokio::test(flavor = "multi_thread")]
    async fn an_interact_page_reaches_the_denied_server_without_either_defense() {
        assert_interact_teardown_reaches(
            crate::ssrf_intercept::SendingSite::start().await,
            "an_interact_page_reaches_the_denied_server_without_either_defense",
        )
        .await;
    }

    /// The popup teardown window reaches the denied server with both defenses disabled.
    ///
    /// ~keep This negative control uses the same popup fixture, interact action and hook as its
    /// ~keep property test; it requires that popup open and a positive denied hit, so merely
    /// ~keep creating the popup cannot make the property pass vacuously (#580).
    #[tokio::test(flavor = "multi_thread")]
    async fn an_interact_popup_reaches_the_denied_server_without_either_defense() {
        assert_interact_teardown_reaches(
            crate::ssrf_intercept::SendingSite::start_popup().await,
            "an_interact_popup_reaches_the_denied_server_without_either_defense",
        )
        .await;
    }

    /// A retained page's repeated popup navigations reach the denied server without either defense.
    ///
    /// ~keep This is the negative control for the popup-opening teardown property: a positive hit
    /// ~keep proves the interval actually attempts denied navigations during the held window.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_interact_popup_opening_page_reaches_the_denied_server_without_either_defense() {
        assert_interact_teardown_reaches(
            crate::ssrf_intercept::SendingSite::start_opening_denied_popups().await,
            "an_interact_popup_opening_page_reaches_the_denied_server_without_either_defense",
        )
        .await;
    }

    /// A Scrape action returns the HTML of the document it checked for Chrome's error page. The
    /// test starts on Chrome's error page for a refused connection and navigates to a page of the
    /// site right after the Scrape's HTML read, so the error page's HTML must not be returned as
    /// the site page's. Launches a real Chrome and skips when none is found.
    #[tokio::test(flavor = "multi_thread")]
    #[allow(
        clippy::print_stderr,
        reason = "test-only skip announcement, matching tests/common/mod.rs's convention"
    )]
    async fn a_scrape_returns_the_html_of_the_document_it_checked_for_the_error_page() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let test_name = "a_scrape_returns_the_html_of_the_document_it_checked_for_the_error_page";
        let site = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/two"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw("<html><body><p>doc/two</p></body></html>", "text/html"),
            )
            .mount(&site)
            .await;
        let refused = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a local port must bind");
            format!(
                "http://127.0.0.1:{}/gone",
                listener.local_addr().expect("its address").port()
            )
        };
        let dir = std::env::temp_dir().join(format!("crawlberg-{test_name}-{}", std::process::id()));
        let builder = ChromeBrowserConfig::builder()
            .no_sandbox()
            .new_headless_mode()
            .user_data_dir(dir);
        let launched = match crate::browser_pool::apply_default_args(builder, &[]).build() {
            Ok(config) => Browser::launch(config).await.map_err(|e| e.to_string()),
            Err(error) => Err(error),
        };
        let Some((mut browser, handler)) = crate::browser_pool::tests::expect_chrome_or_skip(test_name, launched)
        else {
            return;
        };
        spawn_handler(handler);
        let page = browser.new_page("about:blank").await.expect("page");
        // ~keep The refused connection fails the navigation; the error page it commits is the point.
        let _ = page.goto(refused.as_str()).await;
        let start = committed_document(&page).await.expect("the committed document");
        let scraped = crate::chrome_frame::NAVIGATE_AFTER_CONTENT
            .scope(
                std::cell::Cell::new(Some(format!("{}/two", site.uri()))),
                execute_action(&page, &PageAction::Scrape),
            )
            .await;
        let _ = browser.close().await;
        let _ = browser.wait().await;

        assert!(
            start.unreachable_url.is_some(),
            "{test_name}: the page must start on Chrome's error page: {}",
            start.url
        );
        let data = scraped
            .expect("the Scrape must succeed on the site page")
            .data
            .expect("the Scrape must return data");
        let html = data["html"].as_str().unwrap_or_default();
        assert!(
            html.contains("doc/two"),
            "{test_name}: the Scrape checked /two, so its HTML must be /two's, not the error page's: {html}"
        );
    }
}
