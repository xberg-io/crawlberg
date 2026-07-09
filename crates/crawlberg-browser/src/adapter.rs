//! Crawlberg-facing adapter for the native browser backend.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

pub use crate::page::PageError;

use crate::context::BrowserContext;
use crate::lifecycle::WaitUntil;
use crate::page::Page;

/// A cookie passed into or captured from the native browser.
#[derive(Debug, Clone)]
pub struct NativeCookie {
    pub name: String,
    pub value: String,
    pub domain: Option<String>,
    pub path: Option<String>,
    pub secure: bool,
    pub http_only: bool,
}

/// A single network event recorded during page navigation.
#[derive(Debug, Clone)]
pub struct NativeNetworkEvent {
    pub url: String,
    pub method: String,
    pub resource_type: String,
    pub status: u16,
    pub request_headers: HashMap<String, String>,
    pub response_headers: HashMap<String, String>,
    pub body_size: usize,
    pub timestamp_ms: u64,
}

#[derive(Debug, Clone)]
pub struct NativeBrowserConfig {
    pub user_agent: Option<String>,
    pub timeout: Duration,
    pub wait_until: NativeBrowserWait,
    pub extra_headers: HashMap<String, String>,
    pub respect_robots_txt: bool,
    /// Use Chrome 145 TLS fingerprint via wreq stealth client.
    pub stealth: bool,
    /// Proxy URL (http/https only). No SOCKS5 — use chromiumoxide for that.
    pub proxy_url: Option<String>,
    /// Cookies pre-populated into the jar before navigation.
    pub prior_cookies: Vec<NativeCookie>,
    /// URL patterns to block (supports `*` wildcards).
    pub block_url_patterns: Vec<String>,
    /// JavaScript snippet evaluated after navigation.
    pub eval_script: Option<String>,
    /// CSS selector to wait for (used when `wait_until == Selector`).
    pub wait_selector: Option<String>,
    /// User-agent for robots.txt fetches. Defaults to `user_agent`.
    pub robots_user_agent: Option<String>,
    /// Capture the full network event stream into the result.
    pub capture_network_events: bool,
}

impl Default for NativeBrowserConfig {
    fn default() -> Self {
        Self {
            user_agent: None,
            timeout: Duration::from_secs(30),
            wait_until: NativeBrowserWait::NetworkIdle,
            extra_headers: HashMap::new(),
            respect_robots_txt: false,
            stealth: false,
            proxy_url: None,
            prior_cookies: Vec::new(),
            block_url_patterns: Vec::new(),
            eval_script: None,
            wait_selector: None,
            robots_user_agent: None,
            capture_network_events: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeBrowserWait {
    Load,
    NetworkIdle,
    /// Poll `document.querySelector(selector)` every 100 ms until found.
    Selector,
}

#[derive(Debug, Clone)]
pub struct RenderedPage {
    pub final_url: String,
    pub status: Option<u16>,
    pub html: String,
    pub headers: HashMap<String, String>,
    /// Return value of `eval_script`, when provided.
    pub eval_result: Option<serde_json::Value>,
    /// Network events recorded during navigation (populated when `capture_network_events`).
    pub network_events: Vec<NativeNetworkEvent>,
    /// All non-expired cookies from the jar after navigation.
    pub cookies: Vec<NativeCookie>,
}

const DEFAULT_SCROLL_AMOUNT: i64 = 800;
const DEFAULT_SELECTOR_WAIT_MS: i64 = 30_000;
const SELECTOR_POLL_INTERVAL: Duration = Duration::from_millis(100);
const SCREENSHOT_VIEWPORT_WIDTH: u32 = 1280;
const SCREENSHOT_VIEWPORT_HEIGHT: u32 = 720;
const MAX_NATIVE_SCREENSHOT_HEIGHT: u32 = 16_384;
const RGBA_CHANNELS: usize = 4;
const SNAPSHOT_MARGIN: u32 = 24;
const SNAPSHOT_ROW_HEIGHT: u32 = 18;
const SNAPSHOT_ROW_GAP: u32 = 6;
const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;
const DEFAULT_NATIVE_WORKER_LIMIT: usize = 8;
const DEFAULT_QUEUE_CAPACITY_PER_WORKER: usize = 64;

/// Scroll direction for native page interactions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeScrollDirection {
    /// Scroll upward.
    Up,
    /// Scroll downward.
    Down,
}

/// A backend-neutral page action translated for the native browser adapter.
#[derive(Debug, Clone)]
pub enum NativePageAction {
    /// Click an element matched by a CSS selector.
    Click { selector: String },
    /// Type text into an element matched by a CSS selector.
    TypeText { selector: String, text: String },
    /// Dispatch a key press to the active element.
    Press { key: String },
    /// Scroll the page or a scrollable element.
    Scroll {
        direction: NativeScrollDirection,
        selector: Option<String>,
        amount: Option<i64>,
    },
    /// Wait for a duration or selector.
    Wait {
        milliseconds: Option<i64>,
        selector: Option<String>,
    },
    /// Request a screenshot.
    Screenshot { full_page: Option<bool> },
    /// Execute JavaScript in the page context.
    ExecuteJs { script: String },
    /// Capture the current HTML.
    Scrape,
}

/// Result from a single native page action.
#[derive(Debug, Clone)]
pub struct NativeActionResult {
    /// Zero-based action index in the submitted sequence.
    pub action_index: usize,
    /// Stable action type string.
    pub action_type: String,
    /// Whether the action completed successfully.
    pub success: bool,
    /// Action-specific return data.
    pub data: Option<serde_json::Value>,
    /// Error message for failed actions.
    pub error: Option<String>,
}

/// Result of native interaction execution.
#[derive(Debug, Clone)]
pub struct NativeInteractionResult {
    /// Per-action execution results.
    pub action_results: Vec<NativeActionResult>,
    /// Final page HTML after all actions.
    pub final_html: String,
    /// Final page URL after all actions.
    pub final_url: String,
    /// Screenshot bytes when supported and requested.
    pub screenshot: Option<Vec<u8>>,
}

/// Configuration for [`NativeBrowserExecutor`].
///
/// Rust-only: excluded from alef-generated polyglot bindings. Intended for
/// long-lived Rust processes that own an executor at the application layer
/// and reuse it across crawls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(alef, alef(skip))]
pub struct NativeBrowserExecutorConfig {
    /// Number of dedicated browser worker threads.
    pub workers: usize,
    /// Bounded job queue capacity for each worker.
    pub queue_capacity_per_worker: usize,
}

impl NativeBrowserExecutorConfig {
    /// Create a config with an explicit worker count and default queue capacity.
    pub fn with_workers(workers: usize) -> Self {
        Self {
            workers,
            queue_capacity_per_worker: DEFAULT_QUEUE_CAPACITY_PER_WORKER,
        }
    }
}

impl Default for NativeBrowserExecutorConfig {
    fn default() -> Self {
        let workers = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1)
            .clamp(1, DEFAULT_NATIVE_WORKER_LIMIT);
        Self {
            workers,
            queue_capacity_per_worker: DEFAULT_QUEUE_CAPACITY_PER_WORKER,
        }
    }
}

/// Dedicated native browser worker pool.
///
/// Each worker owns one OS thread and one current-thread Tokio runtime. Native
/// page state and V8 `JsRuntime`s are created and used only inside a worker, so
/// public executor futures stay `Send` without sharing V8 isolates across
/// threads.
///
/// Rust-only: excluded from alef-generated polyglot bindings. Bindings drive
/// the engine API directly and don't manipulate the executor.
#[derive(Clone)]
#[cfg_attr(alef, alef(skip))]
pub struct NativeBrowserExecutor {
    inner: Arc<NativeBrowserExecutorInner>,
}

struct NativeBrowserExecutorInner {
    workers: Mutex<Vec<tokio::sync::mpsc::Sender<NativeBrowserJob>>>,
    handles: Mutex<Vec<JoinHandle<()>>>,
    next_worker: AtomicUsize,
}

enum NativeBrowserJob {
    Render {
        url: String,
        config: NativeBrowserConfig,
        reply: tokio::sync::oneshot::Sender<Result<RenderedPage, PageError>>,
    },
    Interact {
        url: String,
        config: NativeBrowserConfig,
        actions: Vec<NativePageAction>,
        post_navigation_wait: Option<Duration>,
        reply: tokio::sync::oneshot::Sender<Result<NativeInteractionResult, PageError>>,
    },
}

impl NativeBrowserExecutor {
    /// Start a native browser worker pool.
    pub fn new(config: NativeBrowserExecutorConfig) -> Result<Self, PageError> {
        if config.workers == 0 {
            return Err(PageError::ParseError(
                "native browser executor requires at least one worker".to_owned(),
            ));
        }
        if config.queue_capacity_per_worker == 0 {
            return Err(PageError::ParseError(
                "native browser executor requires queue_capacity_per_worker > 0".to_owned(),
            ));
        }

        let mut workers = Vec::with_capacity(config.workers);
        let mut handles = Vec::with_capacity(config.workers);
        for index in 0..config.workers {
            let (sender, receiver) = tokio::sync::mpsc::channel(config.queue_capacity_per_worker);
            let (startup_sender, startup_receiver) = std::sync::mpsc::channel();
            let handle = std::thread::Builder::new()
                .name(format!("crawlberg-native-browser-{index}"))
                .spawn(move || run_native_worker(receiver, startup_sender))
                .map_err(|e| PageError::NetworkError(format!("failed to start native browser worker: {e}")))?;

            match startup_receiver.recv() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    let _ = handle.join();
                    return Err(PageError::NetworkError(format!(
                        "failed to start native browser worker runtime: {error}"
                    )));
                }
                Err(error) => {
                    let _ = handle.join();
                    return Err(PageError::NetworkError(format!(
                        "native browser worker stopped during startup: {error}"
                    )));
                }
            }

            workers.push(sender);
            handles.push(handle);
        }

        Ok(Self {
            inner: Arc::new(NativeBrowserExecutorInner {
                workers: Mutex::new(workers),
                handles: Mutex::new(handles),
                next_worker: AtomicUsize::new(0),
            }),
        })
    }

    /// Navigate to a URL and return the rendered page.
    pub async fn render_url(&self, url: &str, config: &NativeBrowserConfig) -> Result<RenderedPage, PageError> {
        let (reply, result) = tokio::sync::oneshot::channel();
        let job = NativeBrowserJob::Render {
            url: url.to_owned(),
            config: config.clone(),
            reply,
        };
        self.send_job(job).await?;
        result.await.map_err(|_| {
            PageError::NetworkError("native browser worker stopped before returning render result".to_owned())
        })?
    }

    /// Navigate to a URL and execute page actions.
    pub async fn interact_url(
        &self,
        url: &str,
        config: &NativeBrowserConfig,
        actions: &[NativePageAction],
        post_navigation_wait: Option<Duration>,
    ) -> Result<NativeInteractionResult, PageError> {
        let (reply, result) = tokio::sync::oneshot::channel();
        let job = NativeBrowserJob::Interact {
            url: url.to_owned(),
            config: config.clone(),
            actions: actions.to_vec(),
            post_navigation_wait,
            reply,
        };
        self.send_job(job).await?;
        result.await.map_err(|_| {
            PageError::NetworkError("native browser worker stopped before returning interact result".to_owned())
        })?
    }

    async fn send_job(&self, mut job: NativeBrowserJob) -> Result<(), PageError> {
        let workers = self.worker_senders()?;
        let start = self.inner.next_worker.fetch_add(1, Ordering::Relaxed);

        for offset in 0..workers.len() {
            let index = (start + offset) % workers.len();
            match workers[index].send(job).await {
                Ok(()) => return Ok(()),
                Err(error) => {
                    job = error.0;
                }
            }
        }

        Err(PageError::NetworkError(
            "native browser worker pool is shut down".to_owned(),
        ))
    }

    fn worker_senders(&self) -> Result<Vec<tokio::sync::mpsc::Sender<NativeBrowserJob>>, PageError> {
        let workers = self
            .inner
            .workers
            .lock()
            .map_err(|_| PageError::NetworkError("native browser worker pool lock is poisoned".to_owned()))?;
        if workers.is_empty() {
            return Err(PageError::NetworkError(
                "native browser worker pool is shut down".to_owned(),
            ));
        }
        Ok(workers.clone())
    }
}

impl Drop for NativeBrowserExecutorInner {
    fn drop(&mut self) {
        if let Ok(mut workers) = self.workers.lock() {
            workers.clear();
        }
        if let Ok(mut handles) = self.handles.lock() {
            for handle in handles.drain(..) {
                let _ = handle.join();
            }
        }
    }
}

fn run_native_worker(
    mut receiver: tokio::sync::mpsc::Receiver<NativeBrowserJob>,
    startup_sender: std::sync::mpsc::Sender<Result<(), String>>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => {
            let _ = startup_sender.send(Ok(()));
            runtime
        }
        Err(error) => {
            let _ = startup_sender.send(Err(error.to_string()));
            return;
        }
    };

    runtime.block_on(async move {
        while let Some(job) = receiver.recv().await {
            match job {
                NativeBrowserJob::Render { url, config, reply } => {
                    let _ = reply.send(render_url_local(&url, &config).await);
                }
                NativeBrowserJob::Interact {
                    url,
                    config,
                    actions,
                    post_navigation_wait,
                    reply,
                } => {
                    let _ = reply.send(interact_url_local(&url, &config, &actions, post_navigation_wait).await);
                }
            }
        }
    });
}

pub async fn render_url(url: &str, config: &NativeBrowserConfig) -> Result<RenderedPage, PageError> {
    let executor = NativeBrowserExecutor::new(NativeBrowserExecutorConfig::with_workers(1))?;
    executor.render_url(url, config).await
}

/// Navigate to a URL and execute page actions using the native browser backend.
pub async fn interact_url(
    url: &str,
    config: &NativeBrowserConfig,
    actions: &[NativePageAction],
    post_navigation_wait: Option<Duration>,
) -> Result<NativeInteractionResult, PageError> {
    let executor = NativeBrowserExecutor::new(NativeBrowserExecutorConfig::with_workers(1))?;
    executor.interact_url(url, config, actions, post_navigation_wait).await
}

async fn render_url_local(url: &str, config: &NativeBrowserConfig) -> Result<RenderedPage, PageError> {
    let context = create_context(config).await;
    render_with_context(url, config, context).await
}

async fn interact_url_local(
    url: &str,
    config: &NativeBrowserConfig,
    actions: &[NativePageAction],
    post_navigation_wait: Option<Duration>,
) -> Result<NativeInteractionResult, PageError> {
    let context = create_context(config).await;
    let mut page = Page::new("page-1".to_string(), context);
    configure_page_interception(&mut page, config);
    navigate_configured(&mut page, url, config).await?;

    if let Some(wait) = post_navigation_wait {
        tokio::time::sleep(wait).await;
    }
    if let Some(ref script) = config.eval_script {
        page.evaluate_result(script)
            .map_err(|e| PageError::ParseError(format!("post-navigation eval_script failed: {e}")))?;
    }

    let mut action_results = Vec::with_capacity(actions.len());
    let mut screenshot = None;
    for (index, action) in actions.iter().enumerate() {
        match execute_action(&mut page, action).await {
            Ok(data) => {
                if let Some(bytes) = data.screenshot {
                    screenshot = Some(bytes);
                }
                action_results.push(NativeActionResult {
                    action_index: index,
                    action_type: action_type(action).to_owned(),
                    success: true,
                    data: data.data,
                    error: None,
                });
            }
            Err(error) => {
                action_results.push(NativeActionResult {
                    action_index: index,
                    action_type: action_type(action).to_owned(),
                    success: false,
                    data: None,
                    error: Some(error),
                });
            }
        }
    }

    let final_url = page.url_string();
    let final_html = rendered_html(&page)
        .ok_or_else(|| PageError::ParseError(format!("no rendered DOM available for {final_url}")))?;

    Ok(NativeInteractionResult {
        action_results,
        final_html,
        final_url,
        screenshot,
    })
}

async fn create_context(config: &NativeBrowserConfig) -> Arc<BrowserContext> {
    let mut context = BrowserContext::with_full_options(
        "crawlberg".to_string(),
        config.proxy_url.clone(),
        config.stealth,
        config.user_agent.clone(),
    );
    context.obey_robots = config.respect_robots_txt;
    if let Some(ref robots_ua) = config.robots_user_agent {
        context.user_agent = robots_ua.clone();
    }
    let context = Arc::new(context);
    context
        .http_client
        .set_extra_headers(config.extra_headers.clone())
        .await;

    for cookie in &config.prior_cookies {
        context.cookie_jar.set_parsed_cookie(
            &cookie.name,
            &cookie.value,
            cookie.domain.as_deref(),
            cookie.path.as_deref(),
            cookie.secure,
            cookie.http_only,
        );
    }

    context
}

async fn render_with_context(
    url: &str,
    config: &NativeBrowserConfig,
    context: Arc<BrowserContext>,
) -> Result<RenderedPage, PageError> {
    let mut page = Page::new("page-1".to_string(), context.clone());
    configure_page_interception(&mut page, config);
    navigate_configured(&mut page, url, config).await?;

    let final_url = page.url_string();
    let status = page
        .network_events
        .iter()
        .rev()
        .find(|event| event.resource_type == "Document")
        .map(|event| event.status);
    let headers = page
        .network_events
        .iter()
        .rev()
        .find(|event| event.resource_type == "Document")
        .map(|event| (*event.response_headers).clone())
        .unwrap_or_default();

    let eval_result = if let Some(ref script) = config.eval_script {
        let val = page.evaluate(script);
        if val.is_null() { None } else { Some(val) }
    } else {
        None
    };

    let network_events = if config.capture_network_events {
        page.network_events
            .iter()
            .map(|ev| NativeNetworkEvent {
                url: ev.url.clone(),
                method: ev.method.clone(),
                resource_type: ev.resource_type.clone(),
                status: ev.status,
                request_headers: ev.headers.clone(),
                response_headers: (*ev.response_headers).clone(),
                body_size: ev.body_size,
                timestamp_ms: (ev.timestamp * 1000.0) as u64,
            })
            .collect()
    } else {
        Vec::new()
    };

    let cookies = context
        .cookie_jar
        .snapshot()
        .into_iter()
        .map(|(name, value, domain, path, secure, http_only)| NativeCookie {
            name,
            value,
            domain: Some(domain),
            path: Some(path),
            secure,
            http_only,
        })
        .collect();

    let html = rendered_html(&page)
        .ok_or_else(|| PageError::ParseError(format!("no rendered DOM available for {final_url}")))?;

    Ok(RenderedPage {
        final_url,
        status,
        html,
        headers,
        eval_result,
        network_events,
        cookies,
    })
}

fn configure_page_interception(page: &mut Page, config: &NativeBrowserConfig) {
    if !config.block_url_patterns.is_empty() {
        page.intercept_enabled = true;
        page.intercept_block_patterns = config.block_url_patterns.clone();
    }
}

async fn navigate_configured(page: &mut Page, url: &str, config: &NativeBrowserConfig) -> Result<(), PageError> {
    let wait_until = match config.wait_until {
        NativeBrowserWait::Load => WaitUntil::Load,
        NativeBrowserWait::NetworkIdle | NativeBrowserWait::Selector => WaitUntil::NetworkIdle0,
    };

    tokio::time::timeout(config.timeout, page.navigate_with_wait(url, wait_until))
        .await
        .map_err(|_| PageError::NetworkError(format!("browser timed out after {:?}", config.timeout)))??;

    // ~keep Selector waits use the remaining timeout budget so navigation time counts against the same deadline.
    if config.wait_until == NativeBrowserWait::Selector
        && let Some(ref selector) = config.wait_selector
    {
        let deadline = tokio::time::Instant::now() + config.timeout;
        loop {
            let found = selector_exists(page, selector)
                .map_err(|e| PageError::ParseError(format!("invalid wait selector {selector:?}: {e}")))?;
            if found {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(PageError::NetworkError(format!(
                    "browser timed out waiting for selector '{selector}' after {:?}",
                    config.timeout
                )));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    Ok(())
}

struct NativeActionData {
    data: Option<serde_json::Value>,
    screenshot: Option<Vec<u8>>,
}

impl NativeActionData {
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

async fn execute_action(page: &mut Page, action: &NativePageAction) -> Result<NativeActionData, String> {
    match action {
        NativePageAction::Click { selector } => {
            click(page, selector).await?;
            Ok(NativeActionData::empty())
        }
        NativePageAction::TypeText { selector, text } => {
            type_text(page, selector, text)?;
            Ok(NativeActionData::empty())
        }
        NativePageAction::Press { key } => {
            press(page, key).await?;
            Ok(NativeActionData::empty())
        }
        NativePageAction::Scroll {
            direction,
            selector,
            amount,
        } => {
            scroll(page, *direction, selector.as_deref(), *amount)?;
            Ok(NativeActionData::empty())
        }
        NativePageAction::Wait { milliseconds, selector } => {
            wait_for_action(page, *milliseconds, selector.as_deref()).await?;
            Ok(NativeActionData::empty())
        }
        NativePageAction::Screenshot { full_page } => {
            let full_page = full_page.unwrap_or(false);
            let bytes = screenshot(page, full_page).await?;
            let len = bytes.len();
            Ok(NativeActionData {
                data: Some(serde_json::json!({
                    "bytes": len,
                    "format": "png",
                    "full_page": full_page,
                })),
                screenshot: Some(bytes),
            })
        }
        NativePageAction::ExecuteJs { script } => page.evaluate_result(script).map(NativeActionData::data),
        NativePageAction::Scrape => {
            let final_url = page.url_string();
            let html = rendered_html(page).ok_or_else(|| format!("no rendered DOM available for {final_url}"))?;
            Ok(NativeActionData::data(serde_json::json!({ "html": html })))
        }
    }
}

async fn click(page: &mut Page, selector: &str) -> Result<(), String> {
    validate_selector_syntax(page, selector)?;
    let selector_json = json_string(selector, "selector")?;
    let script = format!(
        r#"
        (() => {{
            const selector = {selector_json};
            const target = document.querySelector(selector);
            if (!target) {{
                return {{ ok: false, error: `click target not found: ${{selector}}` }};
            }}
            target.focus && target.focus();
            target.dispatchEvent(new MouseEvent("mousedown", {{ bubbles: true, cancelable: true, button: 0 }}));
            target.dispatchEvent(new MouseEvent("mouseup", {{ bubbles: true, cancelable: true, button: 0 }}));
            target.click();
            return {{ ok: true }};
        }})()
        "#
    );
    let result = page
        .evaluate_result(&script)
        .map_err(|e| format!("click selector evaluation failed: {e}"))?;
    expect_ok(result, "click")?;
    page.process_pending_navigation()
        .await
        .map_err(|e| format!("failed to process click navigation: {e}"))?;
    Ok(())
}

fn type_text(page: &mut Page, selector: &str, text: &str) -> Result<(), String> {
    validate_selector_syntax(page, selector)?;
    let selector_json = json_string(selector, "selector")?;
    let text_json = json_string(text, "text")?;
    let script = format!(
        r#"
        (() => {{
            const selector = {selector_json};
            const text = {text_json};
            const target = document.querySelector(selector);
            if (!target) {{
                return {{ ok: false, error: `type target not found: ${{selector}}` }};
            }}
            target.focus && target.focus();
            for (const char of Array.from(text)) {{
                const keydownAllowed = target.dispatchEvent(new KeyboardEvent("keydown", {{ key: char, bubbles: true, cancelable: true }}));
                const keypressAllowed = keydownAllowed
                    ? target.dispatchEvent(new KeyboardEvent("keypress", {{ key: char, bubbles: true, cancelable: true }}))
                    : false;
                if (keydownAllowed && keypressAllowed) {{
                    const current = target.value == null ? "" : String(target.value);
                    target.value = current + char;
                    target.dispatchEvent(new Event("input", {{ bubbles: true }}));
                }}
                target.dispatchEvent(new KeyboardEvent("keyup", {{ key: char, bubbles: true, cancelable: true }}));
            }}
            target.dispatchEvent(new Event("change", {{ bubbles: true }}));
            return {{ ok: true }};
        }})()
        "#
    );
    let result = page
        .evaluate_result(&script)
        .map_err(|e| format!("type selector evaluation failed: {e}"))?;
    expect_ok(result, "type")
}

async fn press(page: &mut Page, key: &str) -> Result<(), String> {
    let key_json = json_string(key, "key")?;
    let script = format!(
        r#"
        (() => {{
            const key = {key_json};
            const target = document.activeElement || document.body || document;
            const keydownAllowed = target.dispatchEvent(new KeyboardEvent("keydown", {{ key, code: key, bubbles: true, cancelable: true }}));
            let keypressAllowed = true;
            if (key === "Enter") {{
                keypressAllowed = keydownAllowed
                    ? target.dispatchEvent(new KeyboardEvent("keypress", {{ key, code: key, bubbles: true, cancelable: true }}))
                    : false;
                const form = target.form || (target.closest && target.closest("form"));
                if (keydownAllowed && keypressAllowed && form && typeof form.submit === "function") {{
                    form.submit();
                }}
            }} else if (key === "Backspace") {{
                if (keydownAllowed && target && (target.localName === "input" || target.localName === "textarea")) {{
                    target.value = String(target.value || "").slice(0, -1);
                    target.dispatchEvent(new Event("input", {{ bubbles: true }}));
                }}
            }} else if (Array.from(key).length === 1) {{
                keypressAllowed = keydownAllowed
                    ? target.dispatchEvent(new KeyboardEvent("keypress", {{ key, code: key, bubbles: true, cancelable: true }}))
                    : false;
                if (keydownAllowed && keypressAllowed && target && (target.localName === "input" || target.localName === "textarea")) {{
                    target.value = String(target.value || "") + key;
                    target.dispatchEvent(new Event("input", {{ bubbles: true }}));
                }}
            }}
            target.dispatchEvent(new KeyboardEvent("keyup", {{ key, code: key, bubbles: true, cancelable: true }}));
            return {{ ok: true }};
        }})()
        "#
    );
    expect_ok(page.evaluate(&script), "press")?;
    page.process_pending_navigation()
        .await
        .map_err(|e| format!("failed to process key navigation: {e}"))?;
    Ok(())
}

async fn screenshot(page: &mut Page, full_page: bool) -> Result<Vec<u8>, String> {
    let html = rendered_html(page).ok_or_else(|| "no rendered DOM available for screenshot".to_string())?;
    let height = if full_page {
        screenshot_content_height(page, &html).max(SCREENSHOT_VIEWPORT_HEIGHT)
    } else {
        SCREENSHOT_VIEWPORT_HEIGHT
    }
    .min(MAX_NATIVE_SCREENSHOT_HEIGHT);

    tokio::task::spawn_blocking(move || render_snapshot_png(&html, SCREENSHOT_VIEWPORT_WIDTH, height))
        .await
        .map_err(|e| format!("native screenshot render task failed: {e}"))?
}

fn screenshot_content_height(page: &mut Page, html: &str) -> u32 {
    let dom_height = page
        .evaluate_result("document.documentElement && document.documentElement.scrollHeight")
        .ok()
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(SCREENSHOT_VIEWPORT_HEIGHT);
    dom_height
        .max(snapshot_content_height(html))
        .max(css_pixel_height_hint(html).unwrap_or(SCREENSHOT_VIEWPORT_HEIGHT))
}

fn render_snapshot_png(html: &str, width: u32, height: u32) -> Result<Vec<u8>, String> {
    let mut buffer = vec![255; width as usize * height as usize * RGBA_CHANNELS];
    draw_snapshot_background(&mut buffer, width, height);
    draw_snapshot_rows(&mut buffer, width, height, html);
    encode_png(&buffer, width, height)
}

fn draw_snapshot_background(buffer: &mut [u8], width: u32, height: u32) {
    for y in 0..height {
        for x in 0..width {
            let offset = pixel_offset(width, x, y);
            let shade = if y < 56 { 238 } else { 250 };
            buffer[offset] = shade;
            buffer[offset + 1] = shade;
            buffer[offset + 2] = shade;
            buffer[offset + 3] = 255;
        }
    }
}

fn draw_snapshot_rows(buffer: &mut [u8], width: u32, height: u32, html: &str) {
    let mut y = SNAPSHOT_MARGIN;
    for chunk in snapshot_chunks(html) {
        if y + SNAPSHOT_ROW_HEIGHT >= height {
            break;
        }
        let row_width = snapshot_row_width(width, &chunk);
        let color = snapshot_color(&chunk);
        fill_rect(buffer, width, SNAPSHOT_MARGIN, y, row_width, SNAPSHOT_ROW_HEIGHT, color);
        y += SNAPSHOT_ROW_HEIGHT + SNAPSHOT_ROW_GAP;
    }
}

fn snapshot_content_height(html: &str) -> u32 {
    let row_count = u32::try_from(snapshot_chunks(html).len()).unwrap_or(u32::MAX);
    SNAPSHOT_MARGIN
        .saturating_mul(2)
        .saturating_add(row_count.saturating_mul(SNAPSHOT_ROW_HEIGHT.saturating_add(SNAPSHOT_ROW_GAP)))
}

fn snapshot_chunks(html: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut text = String::new();
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => {
                push_snapshot_text(&mut chunks, &mut text);
                in_tag = true;
            }
            '>' => {
                in_tag = false;
            }
            _ if !in_tag => text.push(ch),
            _ => {}
        }
    }
    push_snapshot_text(&mut chunks, &mut text);
    if chunks.is_empty() {
        chunks.push("empty document".to_string());
    }
    chunks
}

fn css_pixel_height_hint(html: &str) -> Option<u32> {
    let mut rest = html;
    let mut height = None;
    while let Some(index) = rest.find("height:") {
        rest = &rest[index + "height:".len()..];
        let candidate = parse_css_pixel_value(rest);
        height = height.max(candidate);
    }
    height
}

fn parse_css_pixel_value(input: &str) -> Option<u32> {
    let trimmed = input.trim_start();
    let number: String = trimmed.chars().take_while(|ch| ch.is_ascii_digit()).collect();
    if number.is_empty() {
        return None;
    }
    let suffix = trimmed[number.len()..].trim_start();
    if !suffix.starts_with("px") {
        return None;
    }
    number.parse().ok()
}

fn push_snapshot_text(chunks: &mut Vec<String>, text: &mut String) {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if !normalized.is_empty() {
        chunks.push(normalized);
    }
    text.clear();
}

fn snapshot_row_width(width: u32, text: &str) -> u32 {
    let max_width = width.saturating_sub(SNAPSHOT_MARGIN * 2);
    let text_width = (text.chars().count() as u32).saturating_mul(9).max(48);
    text_width.min(max_width)
}

fn snapshot_color(text: &str) -> [u8; 4] {
    let bytes = stable_hash64(text).to_le_bytes();
    [
        80_u8.saturating_add(bytes[0] / 3),
        96_u8.saturating_add(bytes[1] / 3),
        112_u8.saturating_add(bytes[2] / 3),
        255,
    ]
}

fn stable_hash64(text: &str) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn fill_rect(buffer: &mut [u8], width: u32, x: u32, y: u32, rect_width: u32, rect_height: u32, color: [u8; 4]) {
    for row in y..y + rect_height {
        for col in x..x + rect_width {
            let offset = pixel_offset(width, col, row);
            buffer[offset..offset + RGBA_CHANNELS].copy_from_slice(&color);
        }
    }
}

fn pixel_offset(width: u32, x: u32, y: u32) -> usize {
    (y as usize * width as usize + x as usize) * RGBA_CHANNELS
}

fn encode_png(buffer: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut output, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|e| format!("failed to write native screenshot PNG header: {e}"))?;
        writer
            .write_image_data(buffer)
            .map_err(|e| format!("failed to write native screenshot PNG data: {e}"))?;
    }
    Ok(output)
}

fn scroll(
    page: &mut Page,
    direction: NativeScrollDirection,
    selector: Option<&str>,
    amount: Option<i64>,
) -> Result<(), String> {
    let amount = amount.unwrap_or(DEFAULT_SCROLL_AMOUNT).saturating_abs();
    let signed_amount = match direction {
        NativeScrollDirection::Up => -amount,
        NativeScrollDirection::Down => amount,
    };
    if let Some(selector) = selector {
        validate_selector_syntax(page, selector)?;
    }
    let selector_json = json_option_string(selector, "selector")?;
    let script = format!(
        r#"
        (() => {{
            const selector = {selector_json};
            if (selector) {{
                const target = document.querySelector(selector);
                if (!target) {{
                    return {{ ok: false, error: `scroll target not found: ${{selector}}` }};
                }}
                target.scrollTop = (target.scrollTop || 0) + {signed_amount};
                return {{ ok: true }};
            }}
            if (typeof window.scrollBy === "function") {{
                window.scrollBy(0, {signed_amount});
            }}
            globalThis.__crawlbergScrollY = (globalThis.__crawlbergScrollY || 0) + {signed_amount};
            return {{ ok: true }};
        }})()
        "#
    );
    let result = page
        .evaluate_result(&script)
        .map_err(|e| format!("scroll selector evaluation failed: {e}"))?;
    expect_ok(result, "scroll")
}

async fn wait_for_action(page: &mut Page, milliseconds: Option<i64>, selector: Option<&str>) -> Result<(), String> {
    if let Some(milliseconds) = milliseconds
        && milliseconds < 0
    {
        return Err(format!("wait time {milliseconds}ms must not be negative"));
    }

    if let Some(selector) = selector {
        let wait_ms = milliseconds.unwrap_or(DEFAULT_SELECTOR_WAIT_MS) as u64;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(wait_ms);
        loop {
            if selector_exists(page, selector)? {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(format!("timed out waiting for selector {selector:?}"));
            }
            tokio::time::sleep(SELECTOR_POLL_INTERVAL).await;
        }
    }

    if let Some(milliseconds) = milliseconds {
        tokio::time::sleep(Duration::from_millis(milliseconds as u64)).await;
    }
    Ok(())
}

fn selector_exists(page: &mut Page, selector: &str) -> Result<bool, String> {
    if let Some(result) = page.with_dom(|dom| dom.query_selector(selector)) {
        return result
            .map(|node| node.is_some())
            .map_err(|e| format!("selector syntax error: {e}"));
    }

    let selector_json = json_string(selector, "selector")?;
    let script = format!("!!document.querySelector({selector_json})");
    let found = page
        .evaluate_result(&script)
        .map_err(|e| format!("wait selector evaluation failed: {e}"))?;
    Ok(found.as_bool().unwrap_or(false))
}

fn validate_selector_syntax(page: &Page, selector: &str) -> Result<(), String> {
    if let Some(result) = page.with_dom(|dom| dom.query_selector(selector)) {
        result.map(|_| ()).map_err(|e| format!("selector syntax error: {e}"))?;
    }
    Ok(())
}

fn expect_ok(value: serde_json::Value, operation: &str) -> Result<(), String> {
    if value.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
        return Ok(());
    }
    if let Some(error) = value.get("error").and_then(serde_json::Value::as_str) {
        return Err(error.to_owned());
    }
    Err(format!("native {operation} script returned {value}"))
}

fn json_string(value: &str, field: &str) -> Result<String, String> {
    serde_json::to_string(value).map_err(|e| format!("failed to encode {field}: {e}"))
}

fn json_option_string(value: Option<&str>, field: &str) -> Result<String, String> {
    serde_json::to_string(&value).map_err(|e| format!("failed to encode {field}: {e}"))
}

fn action_type(action: &NativePageAction) -> &'static str {
    match action {
        NativePageAction::Click { .. } => "click",
        NativePageAction::TypeText { .. } => "type",
        NativePageAction::Press { .. } => "press",
        NativePageAction::Scroll { .. } => "scroll",
        NativePageAction::Wait { .. } => "wait",
        NativePageAction::Screenshot { .. } => "screenshot",
        NativePageAction::ExecuteJs { .. } => "executeJs",
        NativePageAction::Scrape => "scrape",
    }
}

fn rendered_html(page: &Page) -> Option<String> {
    page.with_dom(|dom| {
        if let Some(root) = dom.query_selector("html").ok().flatten() {
            dom.outer_html(root)
        } else {
            dom.outer_html(dom.document())
        }
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, OnceLock};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    static ALLOW_PRIVATE: OnceLock<()> = OnceLock::new();

    fn allow_private_network() {
        ALLOW_PRIVATE.get_or_init(|| {
            // ~keep SAFETY: tests write this process env var once before network work starts.
            #[allow(unsafe_code)]
            unsafe {
                std::env::set_var("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "1");
            }
        });
    }

    fn assert_send<T: Send>(_: T) {}

    #[test]
    fn native_browser_executor_futures_are_send() {
        let executor =
            NativeBrowserExecutor::new(NativeBrowserExecutorConfig::with_workers(1)).expect("executor should start");
        let config = NativeBrowserConfig::default();
        let actions = vec![NativePageAction::Scrape];

        assert_send(executor.render_url("http://example.com", &config));
        assert_send(executor.interact_url("http://example.com", &config, &actions, None));
        assert_send(render_url("http://example.com", &config));
        assert_send(interact_url("http://example.com", &config, &actions, None));
    }

    #[tokio::test]
    async fn native_browser_executor_runs_render_jobs_concurrently() {
        allow_private_network();
        let server = TestServer::start().await;
        let executor = NativeBrowserExecutor::new(NativeBrowserExecutorConfig {
            workers: 4,
            queue_capacity_per_worker: 8,
        })
        .expect("executor should start");

        let mut tasks = Vec::new();
        for index in 0..16 {
            let executor = executor.clone();
            let url = format!("{}/page-{index}", server.base_url);
            tasks.push(tokio::spawn(async move {
                executor.render_url(&url, &NativeBrowserConfig::default()).await
            }));
        }

        let results = futures::future::join_all(tasks).await;
        for result in results {
            let rendered = result.expect("task should join").expect("render should succeed");
            assert!(rendered.html.contains("Native executor"));
        }
        assert!(
            server.max_in_flight.load(Ordering::SeqCst) >= 2,
            "server should observe parallel native requests"
        );
    }

    #[tokio::test]
    async fn native_browser_executor_runs_interact_jobs_concurrently() {
        allow_private_network();
        let server = TestServer::start().await;
        let executor = NativeBrowserExecutor::new(NativeBrowserExecutorConfig {
            workers: 4,
            queue_capacity_per_worker: 8,
        })
        .expect("executor should start");

        let actions = vec![
            NativePageAction::Click {
                selector: "#go".to_owned(),
            },
            NativePageAction::Scrape,
        ];
        let mut tasks = Vec::new();
        for index in 0..12 {
            let executor = executor.clone();
            let actions = actions.clone();
            let url = format!("{}/action-{index}", server.base_url);
            tasks.push(tokio::spawn(async move {
                executor
                    .interact_url(&url, &NativeBrowserConfig::default(), &actions, None)
                    .await
            }));
        }

        let results = futures::future::join_all(tasks).await;
        for result in results {
            let interaction = result.expect("task should join").expect("interact should succeed");
            assert!(interaction.action_results.iter().all(|action| action.success));
            assert!(interaction.final_html.contains("clicked"));
        }
        assert!(
            server.max_in_flight.load(Ordering::SeqCst) >= 2,
            "server should observe parallel native interaction requests"
        );
    }

    #[tokio::test]
    async fn native_browser_executor_drops_after_work() {
        allow_private_network();
        let server = TestServer::start().await;
        let executor =
            NativeBrowserExecutor::new(NativeBrowserExecutorConfig::with_workers(2)).expect("executor should start");

        let rendered = executor
            .render_url(&server.base_url, &NativeBrowserConfig::default())
            .await
            .expect("render should succeed");
        assert!(rendered.html.contains("Native executor"));
        drop(executor);
    }

    struct TestServer {
        base_url: String,
        max_in_flight: Arc<AtomicUsize>,
    }

    impl TestServer {
        async fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("test server should bind");
            let addr = listener.local_addr().expect("test server should have local addr");
            let current = Arc::new(AtomicUsize::new(0));
            let max_in_flight = Arc::new(AtomicUsize::new(0));
            let current_for_task = current.clone();
            let max_for_task = max_in_flight.clone();

            tokio::spawn(async move {
                loop {
                    let Ok((mut stream, _)) = listener.accept().await else {
                        return;
                    };
                    let current = current_for_task.clone();
                    let max_in_flight = max_for_task.clone();
                    tokio::spawn(async move {
                        let active = current.fetch_add(1, Ordering::SeqCst) + 1;
                        max_in_flight.fetch_max(active, Ordering::SeqCst);

                        let mut buffer = [0_u8; 1024];
                        let _ = stream.read(&mut buffer).await;
                        tokio::time::sleep(Duration::from_millis(150)).await;
                        let body = r#"
                            <html>
                              <body>
                                <button id="go">Go</button>
                                <div id="status">Native executor</div>
                                <script>
                                  document.getElementById('go').addEventListener('click', () => {
                                    document.getElementById('status').textContent = 'clicked';
                                  });
                                </script>
                              </body>
                            </html>
                        "#;
                        let response = format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = stream.write_all(response.as_bytes()).await;
                        let _ = stream.shutdown().await;
                        current.fetch_sub(1, Ordering::SeqCst);
                    });
                }
            });

            Self {
                base_url: format!("http://{addr}"),
                max_in_flight,
            }
        }
    }
}
