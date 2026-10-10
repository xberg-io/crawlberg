//! Crawlberg-facing adapter for the native browser backend.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

pub use crate::net::OriginHeaders;
pub use crate::net::proxy::{
    ProxyCredentials, ProxyError, SUPPORTED_SCHEMES as SUPPORTED_PROXY_SCHEMES, UpstreamProxy, check_proxy_url,
};
pub use crate::net::ssrf::{
    DEFAULT_DENY_NET_CIDRS, DEFAULT_DENY_NET_RULES, DefaultSsrfValidator, NAMED_SCHEMES, SsrfValidator,
};
pub use crate::page::PageError;

use crate::context::BrowserContext;
use crate::lifecycle::WaitUntil;
use crate::net::CookieJar;
use crate::page::Page;
use crate::redact::{REDACTED, RedactedHeaders, RedactedValues};

mod executor;
mod snapshot;

pub use executor::{NativeBrowserExecutor, NativeBrowserExecutorConfig};
use snapshot::{
    MAX_NATIVE_SCREENSHOT_HEIGHT, SCREENSHOT_VIEWPORT_HEIGHT, SCREENSHOT_VIEWPORT_WIDTH, render_snapshot_png,
    screenshot_content_height,
};

/// A cookie passed into or captured from the native browser.
/// ~keep Opaque cookie and site-for-cookies state for a native navigation chain whose hops are
/// ~keep rendered as separate jobs. This preserves internal cookie attributes without adding
/// ~keep fields to the source-compatible [`NativeCookie`] transport type.
#[derive(Clone)]
pub struct NativeCookie {
    pub name: String,
    pub value: String,
    pub domain: Option<String>,
    pub path: Option<String>,
    pub secure: bool,
    pub http_only: bool,
    /// Sent to `domain` only, not to its subdomains: the page set it without a `Domain` attribute.
    pub host_only: bool,
}

#[derive(Clone)]
#[cfg_attr(alef, alef(skip))]
pub struct NativeRenderState {
    cookie_jar: Arc<CookieJar>,
    site_for_cookies: Option<url::Url>,
}

impl NativeRenderState {
    /// ~keep Create a fresh navigation state seeded with embedder-supplied cookies.
    pub fn new(prior_cookies: &[NativeCookie]) -> Self {
        let cookie_jar = Arc::new(CookieJar::new());
        for cookie in prior_cookies {
            cookie_jar.set_parsed_cookie(cookie);
        }
        Self {
            cookie_jar,
            site_for_cookies: None,
        }
    }

    /// ~keep Record the document that initiates the next top-level navigation.
    pub fn set_site_for_cookies(&mut self, url: &str) {
        self.site_for_cookies = url::Url::parse(url).ok();
    }
}

impl std::fmt::Debug for NativeCookie {
    /// Redacted: a cookie value is often a session credential. Shows whether a value is
    /// set, never the value itself.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            name,
            value,
            domain,
            path,
            secure,
            http_only,
            host_only,
        } = self;
        f.debug_struct("NativeCookie")
            .field("name", name)
            .field("value", &(!value.is_empty()).then_some(REDACTED))
            .field("domain", domain)
            .field("path", path)
            .field("secure", secure)
            .field("http_only", http_only)
            .field("host_only", host_only)
            .finish()
    }
}

/// A single network event recorded during page navigation.
#[derive(Clone)]
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

impl std::fmt::Debug for NativeNetworkEvent {
    /// Redacted: names stay visible throughout. Every *request* header value is hidden,
    /// because the map is populated from caller configuration and a credential can sit under
    /// any name. *Response* header values print except those of the credential denylist.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            url,
            method,
            resource_type,
            status,
            request_headers,
            response_headers,
            body_size,
            timestamp_ms,
        } = self;
        f.debug_struct("NativeNetworkEvent")
            .field("url", url)
            .field("method", method)
            .field("resource_type", resource_type)
            .field("status", status)
            .field("request_headers", &RedactedValues(request_headers))
            .field("response_headers", &RedactedHeaders(response_headers))
            .field("body_size", body_size)
            .field("timestamp_ms", timestamp_ms)
            .finish()
    }
}

#[derive(Clone)]
pub struct NativeBrowserConfig {
    pub user_agent: Option<String>,
    pub timeout: Duration,
    pub wait_until: NativeBrowserWait,
    pub extra_headers: HashMap<String, String>,
    pub respect_robots_txt: bool,
    /// Use Chrome 145 TLS fingerprint via wreq stealth client.
    pub stealth: bool,
    /// Proxy (http/https only). No SOCKS5 — use chromiumoxide for that.
    pub proxy: Option<UpstreamProxy>,
    /// The proxy as a URL. A user name and password in the URL become the proxy credentials. It
    /// gets the same checks as `proxy`, and a render fails when it is unusable or names a
    /// different proxy than `proxy`.
    #[deprecated(since = "1.9.0", note = "set `proxy` to an `UpstreamProxy` instead")]
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
    /// SSRF policy for every request this render makes — navigation, sub-resources,
    /// page-initiated fetch/XHR and dynamic `import()`.
    ///
    /// `None` falls back to [`DefaultSsrfValidator`], which enforces the default
    /// deny-list only. `crawlberg` always supplies the crawl's configured policy.
    pub ssrf: Option<Arc<dyn SsrfValidator>>,
    /// Whether `file://` URLs may be fetched. Off by default: a remote CDP client must
    /// not be able to point the browser at local files.
    pub allow_file_access: bool,
    /// Headers sent only to one host, such as a credential, on every request and redirect
    /// hop there, including a page script's `fetch()` and module imports.
    ///
    /// Unlike `extra_headers`, which every host receives, these never reach a third-party
    /// subresource or a cross-host redirect target.
    pub origin_headers: Option<OriginHeaders>,
    /// The most redirects the navigation to the page follows: HTTP redirects, and the
    /// navigations the page's script starts, one each. A chain of HTTP redirects past it ends on
    /// the redirect response at the limit; a script navigation past it is not taken. `None`
    /// keeps the backend's own caps. Navigations an interact action starts are not counted.
    pub max_redirects: Option<usize>,
    /// Turns the bytes of a document into its text. `None` reads every document as UTF-8, with
    /// a replacement character for each sequence that is not UTF-8.
    pub document_decoder: Option<DocumentDecoder>,
}

/// Decodes the bytes of a document. It gets the `Content-Type` header value, the URL and the
/// bytes of the response, and returns the text and the character set it used, if it names one.
pub type DocumentDecoder = fn(content_type: &str, url: &str, body: &[u8]) -> (String, Option<String>);

#[allow(deprecated)]
impl std::fmt::Debug for NativeBrowserConfig {
    /// Redacted: `extra_headers` carries the `Authorization` header built from the crawl's
    /// auth config, and `prior_cookies`
    /// are session cookies. `eval_script` can embed a token, so it prints as `***` with its
    /// length. Header names stay visible; secret values print as `***`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            user_agent,
            timeout,
            wait_until,
            extra_headers,
            respect_robots_txt,
            stealth,
            proxy,
            proxy_url,
            prior_cookies,
            block_url_patterns,
            eval_script,
            wait_selector,
            robots_user_agent,
            capture_network_events,
            ssrf,
            allow_file_access,
            origin_headers,
            max_redirects,
            document_decoder,
        } = self;
        f.debug_struct("NativeBrowserConfig")
            .field("user_agent", user_agent)
            .field("timeout", timeout)
            .field("wait_until", wait_until)
            .field("extra_headers", &RedactedValues(extra_headers))
            .field("respect_robots_txt", respect_robots_txt)
            .field("stealth", stealth)
            .field("proxy", proxy)
            .field("proxy_url", &proxy_url.as_ref().map(|_| REDACTED))
            .field("prior_cookies", prior_cookies)
            .field("block_url_patterns", block_url_patterns)
            .field(
                "eval_script",
                &eval_script
                    .as_ref()
                    .map(|script| format!("{REDACTED} ({} bytes)", script.len())),
            )
            .field("wait_selector", wait_selector)
            .field("robots_user_agent", robots_user_agent)
            .field("capture_network_events", capture_network_events)
            .field("ssrf", ssrf)
            .field("allow_file_access", allow_file_access)
            .field("origin_headers", origin_headers)
            .field("max_redirects", max_redirects)
            .field("document_decoder", &document_decoder.is_some())
            .finish()
    }
}

#[allow(deprecated)]
impl Default for NativeBrowserConfig {
    fn default() -> Self {
        Self {
            user_agent: None,
            timeout: Duration::from_secs(30),
            wait_until: NativeBrowserWait::NetworkIdle,
            extra_headers: HashMap::new(),
            respect_robots_txt: false,
            stealth: false,
            proxy: None,
            proxy_url: None,
            prior_cookies: Vec::new(),
            block_url_patterns: Vec::new(),
            eval_script: None,
            wait_selector: None,
            robots_user_agent: None,
            capture_network_events: false,
            ssrf: None,
            allow_file_access: false,
            origin_headers: None,
            max_redirects: None,
            document_decoder: None,
        }
    }
}

impl NativeBrowserConfig {
    /// The proxy a render goes through: `proxy` or the deprecated `proxy_url`. When both are set
    /// they must name the same proxy.
    #[allow(deprecated)]
    pub(crate) fn effective_proxy(&self) -> Result<Option<UpstreamProxy>, PageError> {
        let from_url = self
            .proxy_url
            .as_deref()
            .map(crate::net::proxy::proxy_from_url)
            .transpose()
            .map_err(|e| PageError::InvalidConfig(e.to_string()))?;
        match (&self.proxy, from_url) {
            (Some(proxy), Some(from_url)) if *proxy != from_url => Err(PageError::InvalidConfig(
                "proxy and proxy_url name different proxies; set only proxy, as proxy_url is deprecated".to_string(),
            )),
            (Some(proxy), _) => Ok(Some(proxy.clone())),
            (None, from_url) => Ok(from_url),
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

#[derive(Clone)]
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
    /// Redirects the navigation followed when `max_redirects` was set: HTTP redirects, and the
    /// navigations the page's script started, one each. 0 when it was not set.
    pub redirects: usize,
    /// The character set the document decoder reported for the document, if it reported one.
    pub charset: Option<String>,
}

impl std::fmt::Debug for RenderedPage {
    /// Redacted: `headers` can carry `Set-Cookie`. Header names stay visible; sensitive
    /// values print as `***`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            final_url,
            status,
            html,
            headers,
            eval_result,
            network_events,
            cookies,
            redirects,
            charset,
        } = self;
        f.debug_struct("RenderedPage")
            .field("final_url", final_url)
            .field("status", status)
            .field("html", html)
            .field("headers", &RedactedHeaders(headers))
            .field("eval_result", eval_result)
            .field("network_events", network_events)
            .field("cookies", cookies)
            .field("redirects", redirects)
            .field("charset", charset)
            .finish()
    }
}

/// Per-action ceiling in the native worker.
///
/// ~keep Deliberately generous: crawlberg validates a single `Wait` up to 5 minutes,
/// and this loop cannot see the caller's per-action budget, so a tighter value would
/// kill legitimate waits. Its job is only to stop a non-terminating action pinning the
/// worker's OS thread forever, not to enforce latency.
const ACTION_TIMEOUT: Duration = Duration::from_secs(360);

/// Wall-clock bound on a single `ExecuteJs` action, enforced from a watchdog thread via
/// `v8::IsolateHandle::terminate_execution` (#60).
///
/// ~keep `ACTION_TIMEOUT`'s `tokio::time::timeout` wrapper around `execute_action` cannot
/// preempt this action: `Page::evaluate_result` is a synchronous, non-yielding call into V8,
/// so the outer future never returns `Poll::Pending` for the executor to check against a
/// timer. This constant is deliberately much tighter than `ACTION_TIMEOUT` — an explicit,
/// caller-supplied script has no legitimate reason to block the isolate for minutes, and a
/// short bound keeps a hostile or buggy script from pinning a worker for long.
const EXECUTE_JS_TIMEOUT: Duration = Duration::from_secs(30);

/// Wall-clock bound on `config.eval_script`, enforced the same way as `EXECUTE_JS_TIMEOUT` (#71).
///
/// ~keep `eval_script` is operator/config-supplied rather than untrusted per-action input, but
/// it hits the identical hazard: it runs through `Page::evaluate`/`evaluate_result`, a
/// synchronous, non-yielding V8 call that no `tokio::time::timeout` around it can preempt. It
/// deliberately reuses `EXECUTE_JS_TIMEOUT`'s value rather than `config.timeout`: `config.timeout`
/// is caller-supplied and unbounded (crawlberg's `CrawlConfig::validate` does not clamp it), and
/// `evaluate_with_timeout` treats a zero duration as "no watchdog" — so deriving the guard from
/// it would let an unvalidated zero (or an operator's very long navigation budget) silently
/// reopen the hang this fix closes. A fixed, proven bound keeps the guarantee independent of
/// caller configuration.
const EVAL_SCRIPT_TIMEOUT: Duration = EXECUTE_JS_TIMEOUT;

const DEFAULT_SCROLL_AMOUNT: i64 = 800;
const DEFAULT_SELECTOR_WAIT_MS: i64 = 30_000;
const SELECTOR_POLL_INTERVAL: Duration = Duration::from_millis(100);

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
    let context = create_context(config).await?;
    render_with_context(url, config, context, None).await
}

async fn render_url_local_with_state(
    url: &str,
    config: &NativeBrowserConfig,
    state: NativeRenderState,
) -> Result<RenderedPage, PageError> {
    let context = create_context_with_cookie_jar(config, state.cookie_jar).await?;
    render_with_context(url, config, context, state.site_for_cookies).await
}

async fn interact_url_local(
    url: &str,
    config: &NativeBrowserConfig,
    actions: &[NativePageAction],
    post_navigation_wait: Option<Duration>,
) -> Result<NativeInteractionResult, PageError> {
    let context = create_context(config).await?;
    let mut page = Page::new("page-1".to_string(), context);
    page.document_decoder = config.document_decoder;
    configure_page_interception(&mut page, config);
    navigate_configured(&mut page, url, config).await?;

    if let Some(wait) = post_navigation_wait {
        tokio::time::sleep(wait).await;
    }
    if let Some(ref script) = config.eval_script {
        page.evaluate_result_with_timeout(script, EVAL_SCRIPT_TIMEOUT)
            .map_err(|e| PageError::ParseError(format!("post-navigation eval_script failed: {e}")))?;
    }

    let mut action_results = Vec::with_capacity(actions.len());
    let mut screenshot = None;
    for (index, action) in actions.iter().enumerate() {
        // ~keep This job runs on a dedicated OS thread fed by a blocking job queue, so a
        // caller-side timeout only stops the caller waiting — it cannot cancel this loop.
        // Without a timeout here a non-terminating ExecuteJs pins the worker thread and
        // leaks the Chrome subprocess for the life of the process.
        let outcome = match tokio::time::timeout(ACTION_TIMEOUT, execute_action(&mut page, action)).await {
            Ok(outcome) => outcome,
            Err(_) => Err(format!("action timed out after {}s", ACTION_TIMEOUT.as_secs())),
        };
        match outcome {
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

async fn create_context(config: &NativeBrowserConfig) -> Result<Arc<BrowserContext>, PageError> {
    let context = create_context_with_cookie_jar(config, Arc::new(CookieJar::new())).await?;
    for cookie in &config.prior_cookies {
        context.cookie_jar.set_parsed_cookie(cookie);
    }
    Ok(context)
}

async fn create_context_with_cookie_jar(
    config: &NativeBrowserConfig,
    cookie_jar: Arc<CookieJar>,
) -> Result<Arc<BrowserContext>, PageError> {
    let ssrf: Arc<dyn SsrfValidator> = config
        .ssrf
        .clone()
        .unwrap_or_else(|| Arc::new(DefaultSsrfValidator::from_env()));
    let proxy = config.effective_proxy()?;
    let mut context = BrowserContext::with_ssrf_and_cookie_jar(
        "crawlberg".to_string(),
        proxy,
        config.stealth,
        config.user_agent.clone(),
        ssrf,
        config.allow_file_access,
        cookie_jar,
    )?;
    context.obey_robots = config.respect_robots_txt;
    if let Some(ref robots_ua) = config.robots_user_agent {
        context.user_agent = robots_ua.clone();
    }
    let context = Arc::new(context);
    context
        .http_client
        .set_extra_headers(config.extra_headers.clone())
        .await;
    #[cfg(feature = "stealth")]
    if let Some(stealth) = &context.stealth_client {
        stealth.set_extra_headers(config.extra_headers.clone()).await;
    }
    context
        .http_client
        .set_origin_headers(config.origin_headers.clone())
        .await;

    Ok(context)
}

async fn render_with_context(
    url: &str,
    config: &NativeBrowserConfig,
    context: Arc<BrowserContext>,
    site_for_cookies: Option<url::Url>,
) -> Result<RenderedPage, PageError> {
    let mut page = Page::new("page-1".to_string(), context.clone());
    page.url = site_for_cookies;
    page.document_decoder = config.document_decoder;
    configure_page_interception(&mut page, config);
    let redirects = navigate_configured(&mut page, url, config).await?;

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

    let eval_result = evaluate_render_eval_script(&mut page, config);
    let network_events = collect_native_network_events(&page, config.capture_network_events);

    let cookies = context
        .cookie_jar
        .snapshot()
        .into_iter()
        .map(
            |(name, value, domain, path, secure, http_only, host_only)| NativeCookie {
                name,
                value,
                domain: Some(domain),
                path: Some(path),
                secure,
                http_only,
                host_only,
            },
        )
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
        redirects,
        charset: page.document_charset.clone(),
    })
}

fn evaluate_render_eval_script(page: &mut Page, config: &NativeBrowserConfig) -> Option<serde_json::Value> {
    let script = config.eval_script.as_ref()?;
    match page.evaluate_result_with_timeout(script, EVAL_SCRIPT_TIMEOUT) {
        Ok(value) if !value.is_null() => Some(value),
        Ok(_) => None,
        Err(error) => {
            tracing::debug!("eval_script error for '{}': {}", &script[..script.len().min(80)], error);
            None
        }
    }
}

fn collect_native_network_events(page: &Page, capture: bool) -> Vec<NativeNetworkEvent> {
    if !capture {
        return Vec::new();
    }
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
}

fn configure_page_interception(page: &mut Page, config: &NativeBrowserConfig) {
    if !config.block_url_patterns.is_empty() {
        page.intercept_enabled = true;
        page.intercept_block_patterns = config.block_url_patterns.clone();
    }
}

/// Navigate `page` to `url` as `config` asks, and return the redirects followed on the way
/// (0 without `max_redirects`).
async fn navigate_configured(page: &mut Page, url: &str, config: &NativeBrowserConfig) -> Result<usize, PageError> {
    let wait_until = match config.wait_until {
        NativeBrowserWait::Load => WaitUntil::Load,
        NativeBrowserWait::NetworkIdle | NativeBrowserWait::Selector => WaitUntil::NetworkIdle0,
    };

    let navigation = async {
        match config.max_redirects {
            Some(limit) => page.navigate_counting(url, wait_until, limit).await,
            None => page.navigate_with_wait(url, wait_until).await.map(|()| 0),
        }
    };
    let redirects = tokio::time::timeout(config.timeout, navigation)
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

    Ok(redirects)
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
        NativePageAction::ExecuteJs { script } => page
            .evaluate_result_with_timeout(script, EXECUTE_JS_TIMEOUT)
            .map(NativeActionData::data),
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
mod tests;
