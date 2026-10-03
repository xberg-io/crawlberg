use std::sync::Arc;

use crate::dom::{DomTree, parse_html};
use crate::js::runtime::BrowserJsRuntime;
use crate::net::{HttpClient, NetError, Response};
use crate::redact::{RedactedHeaders, RedactedValues};
use url::Url;

use crate::context::BrowserContext;
use crate::lifecycle::LifecycleState;

#[cfg(feature = "stealth")]
use crate::net::StealthHttpClient;

mod navigation;
mod scripts;
mod security;

#[cfg(test)]
mod tests;

use security::cross_scheme_to_file;

/// Wall-clock bound on [`Page::execute_preload_script`], mirroring `adapter::EVAL_SCRIPT_TIMEOUT`
/// and `adapter::EXECUTE_JS_TIMEOUT` (#71): a fixed, watchdog-enforced ceiling for
/// operator/config-supplied scripts, independent of any caller-configured, potentially
/// unbounded timeout.
const PRELOAD_SCRIPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
#[derive(Clone)]
pub struct NetworkEvent {
    pub request_id: String,
    pub url: String,
    pub method: String,
    pub resource_type: String,
    pub status: u16,
    pub headers: std::collections::HashMap<String, String>,
    pub response_headers: Arc<std::collections::HashMap<String, String>>,
    pub body_size: usize,
    pub timestamp: f64,
}

impl std::fmt::Debug for NetworkEvent {
    /// Redacted: names stay visible throughout. `headers` is the *request* map, so every
    /// value is hidden; `response_headers` keeps every value but those of the credential
    /// denylist, `SENSITIVE_HEADERS`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            request_id,
            url,
            method,
            resource_type,
            status,
            headers,
            response_headers,
            body_size,
            timestamp,
        } = self;
        f.debug_struct("NetworkEvent")
            .field("request_id", request_id)
            .field("url", url)
            .field("method", method)
            .field("resource_type", resource_type)
            .field("status", status)
            .field("headers", &RedactedValues(headers))
            .field("response_headers", &RedactedHeaders(response_headers))
            .field("body_size", body_size)
            .field("timestamp", timestamp)
            .finish()
    }
}

pub struct Page {
    pub id: String,
    pub frame_id: String,
    pub url: Option<Url>,
    pub dom: Option<DomTree>,
    pub js: Option<BrowserJsRuntime>,
    pub lifecycle: LifecycleState,
    pub http_client: Arc<HttpClient>,
    pub context: Arc<BrowserContext>,
    pub title: String,
    pub network_events: Vec<NetworkEvent>,
    network_event_counter: u32,
    pub intercept_enabled: bool,
    pub intercept_block_patterns: Vec<String>,
    intercept_tx: Option<tokio::sync::mpsc::UnboundedSender<crate::js::ops::InterceptedRequest>>,
    #[cfg(feature = "stealth")]
    pub stealth_client: Option<Arc<StealthHttpClient>>,
}

impl Page {
    pub fn new(id: String, context: Arc<BrowserContext>) -> Self {
        let http_client = context.http_client.clone();
        // ~keep Playwright expects the main frame id to equal target id; diverging detaches the frame.
        let frame_id = id.clone();
        #[cfg(feature = "stealth")]
        let stealth_client = context.stealth_client.clone();
        // ~keep The scoped headers are set on the context's client before any page exists,
        // ~keep so they are already there to copy; the stealth client must scope them the same way.
        #[cfg(feature = "stealth")]
        if let Some(stealth) = &stealth_client
            && let (Ok(source), Ok(mut target)) = (
                http_client.origin_headers.try_read(),
                stealth.origin_headers.try_write(),
            )
        {
            target.clone_from(&source);
        }

        Page {
            id,
            frame_id,
            url: None,
            dom: None,
            js: None,
            lifecycle: LifecycleState::Idle,
            http_client,
            context,
            title: String::new(),
            network_events: Vec::new(),
            network_event_counter: 0,
            intercept_enabled: false,
            intercept_block_patterns: Vec::new(),
            intercept_tx: None,
            #[cfg(feature = "stealth")]
            stealth_client,
        }
    }

    fn should_block_url(&self, url: &str) -> bool {
        self.intercept_enabled && crate::net::interceptor::matches_block_pattern(&self.intercept_block_patterns, url)
    }

    /// Parse a sub-resource reference against the page URL; `None` when it does not parse or
    /// carries userinfo, which is refused before anything logs or fetches it.
    fn resolve_subresource_url(&self, reference: &str) -> Option<String> {
        Url::options()
            .base_url(self.url.as_ref())
            .parse(reference)
            .ok()
            .filter(|url| !crate::net::credential::has_userinfo(url))
            .map(String::from)
    }

    async fn do_fetch(&self, url: &Url, max_redirects: Option<usize>) -> Result<Response, NetError> {
        self.do_fetch_from(url, max_redirects, None).await
    }

    async fn do_fetch_from(
        &self,
        url: &Url,
        max_redirects: Option<usize>,
        initiator: Option<&Url>,
    ) -> Result<Response, NetError> {
        #[cfg(feature = "stealth")]
        if let Some(ref stealth) = self.stealth_client {
            return stealth.fetch_following_from(url, max_redirects, initiator).await;
        }
        self.http_client
            .fetch_following_from(reqwest::Method::GET, url, None, max_redirects, initiator)
            .await
    }
    fn init_js(&mut self) {
        // ~keep Recreate the JS realm every navigation so prior-page handlers cannot run in the next document.
        if self.js.is_some() {
            let _ = self.js.take();
        }

        // ~keep Thread the context proxy into ES modules and JS fetch/XHR so page JS honors upstream proxy settings.
        let mut rt = BrowserJsRuntime::with_base_url_proxy_and_ssrf(
            &self.url_string(),
            self.context.proxy.clone(),
            self.http_client.ssrf.clone(),
        );
        rt.set_url(&self.url_string());
        rt.set_title(&self.title);

        #[cfg(feature = "stealth")]
        if self.stealth_client.is_some() {
            rt.set_user_agent(crate::net::STEALTH_USER_AGENT);
        } else if let Ok(ua) = self.http_client.user_agent.try_read() {
            rt.set_user_agent(&ua);
        }
        #[cfg(not(feature = "stealth"))]
        if let Ok(ua) = self.http_client.user_agent.try_read() {
            rt.set_user_agent(&ua);
        }

        rt.set_cookie_jar(self.context.cookie_jar.clone());
        rt.set_http_client(self.http_client.clone());
        if self.intercept_enabled {
            rt.set_intercept_block_patterns(self.intercept_block_patterns.clone());
        }

        if let Some(tx) = &self.intercept_tx {
            rt.set_intercept_tx(tx.clone());
        }

        if let Some(dom) = self.dom.take() {
            rt.set_dom(dom);
        }

        self.js = Some(rt);
    }
    pub async fn navigate(&mut self, url_str: &str) -> Result<(), PageError> {
        self.navigate_with_wait(url_str, crate::lifecycle::WaitUntil::Load)
            .await
    }

    pub async fn navigate_with_wait(
        &mut self,
        url_str: &str,
        wait_until: crate::lifecycle::WaitUntil,
    ) -> Result<(), PageError> {
        self.navigate_with_wait_post(url_str, wait_until, "GET", "").await
    }

    pub async fn navigate_with_wait_post(
        &mut self,
        url_str: &str,
        wait_until: crate::lifecycle::WaitUntil,
        method: &str,
        body: &str,
    ) -> Result<(), PageError> {
        self.navigate_chain(url_str, wait_until, method, body, None)
            .await
            .map(drop)
    }

    /// Navigate to `url_str` and follow at most `max_redirects` redirects on the way: HTTP
    /// redirects, and the navigations the page's script starts, one each. Returns how many it
    /// followed. A chain of HTTP redirects past the limit ends on the redirect response at the
    /// limit; a script navigation past it is not taken, and the page keeps its document.
    pub async fn navigate_counting(
        &mut self,
        url_str: &str,
        wait_until: crate::lifecycle::WaitUntil,
        max_redirects: usize,
    ) -> Result<usize, PageError> {
        self.navigate_chain(url_str, wait_until, "GET", "", Some(max_redirects))
            .await
    }

    /// Navigate, then follow the navigations the page's script starts. With `max_redirects`
    /// set, it bounds every redirect of the chain and the count is returned; without it, HTTP
    /// redirects follow the client's cap and a chain of script navigations fails past its own.
    async fn navigate_chain(
        &mut self,
        url_str: &str,
        wait_until: crate::lifecycle::WaitUntil,
        method: &str,
        body: &str,
        max_redirects: Option<usize>,
    ) -> Result<usize, PageError> {
        let mut current_url = url_str.to_string();
        let mut current_method = method.to_string();
        let mut current_body = body.to_string();
        const REDIRECT_LIMIT: usize = 10;
        let mut followed = 0usize;
        let mut script_navigations = 0usize;
        loop {
            let left = max_redirects.map(|limit| limit.saturating_sub(followed));
            followed += self
                .navigate_single(&current_url, wait_until, &current_method, &current_body, left)
                .await?;
            let Some((next_url, next_method, next_body)) = self.take_pending_navigation() else {
                break;
            };
            if cross_scheme_to_file(&current_url, &next_url) {
                // ~keep SOP gate: HTTP(S) pages must not navigate to `file:` and then read the loaded document.
                tracing::warn!(
                    "blocking JS-initiated cross-scheme navigation to file: {} -> {}",
                    current_url,
                    next_url,
                );
                break;
            }
            if max_redirects.is_some_and(|limit| followed >= limit) {
                tracing::debug!(
                    "not following a script navigation past the redirect limit: {current_url} -> {next_url}"
                );
                break;
            }
            script_navigations += 1;
            if max_redirects.is_none() && script_navigations == REDIRECT_LIMIT {
                // ~keep Exceeding the JS navigation cap is an error so redirect storms are not reported as loads.
                return Err(PageError::TooManyRedirects(REDIRECT_LIMIT));
            }
            followed += 1;
            tracing::info!(
                "JS-triggered navigation chain: {} {} -> {}",
                current_method,
                current_url,
                next_url
            );
            current_url = next_url;
            current_method = next_method;
            current_body = next_body;
        }
        Ok(followed)
    }
    pub fn navigate_blank(&mut self) {
        self.js = None;
        self.url = Some(Url::parse("about:blank").unwrap());
        self.dom = Some(parse_html("<!DOCTYPE html><html><head></head><body></body></html>"));
        self.title = String::new();
        self.lifecycle = LifecycleState::Loaded;
    }

    pub fn url_string(&self) -> String {
        self.url
            .as_ref()
            .map(|u| u.to_string())
            .unwrap_or_else(|| "about:blank".to_string())
    }

    pub fn with_dom<R>(&self, f: impl FnOnce(&DomTree) -> R) -> Option<R> {
        if let Some(js) = &self.js {
            return js.with_dom(f);
        }
        self.dom.as_ref().map(f)
    }

    pub fn dom(&self) -> Option<&DomTree> {
        self.dom.as_ref()
    }

    pub fn evaluate(&mut self, expression: &str) -> serde_json::Value {
        match self.evaluate_result(expression) {
            Ok(value) => value,
            Err(error) => {
                tracing::debug!(
                    "JS eval error for '{}': {}",
                    &expression[..expression.len().min(80)],
                    error
                );
                serde_json::Value::Null
            }
        }
    }

    pub fn evaluate_result(&mut self, expression: &str) -> Result<serde_json::Value, String> {
        if let Some(js) = &mut self.js {
            js.evaluate(expression).map_err(|e| e.to_string())
        } else {
            Ok(match expression.trim() {
                "document.title" => serde_json::Value::String(self.title.clone()),
                "document.URL" | "document.location.href" | "window.location.href" => {
                    serde_json::Value::String(self.url_string())
                }
                _ => serde_json::Value::Null,
            })
        }
    }

    /// Like [`Self::evaluate_result`], but bounds execution to `timeout` from a companion
    /// watchdog thread so a non-terminating script cannot pin the caller's worker forever
    /// (#60). See [`BrowserJsRuntime::evaluate_with_timeout`] for the recovery mechanism.
    pub fn evaluate_result_with_timeout(
        &mut self,
        expression: &str,
        timeout: std::time::Duration,
    ) -> Result<serde_json::Value, String> {
        if let Some(js) = &mut self.js {
            js.evaluate_with_timeout(expression, timeout)
        } else {
            self.evaluate_result(expression)
        }
    }

    pub async fn evaluate_for_cdp(
        &mut self,
        expression: &str,
        return_by_value: bool,
        await_promise: bool,
    ) -> crate::js::runtime::RemoteObjectInfo {
        if let Some(js) = &mut self.js {
            match js.evaluate_for_cdp(expression, return_by_value, await_promise).await {
                Ok(info) => info,
                Err(e) => {
                    tracing::debug!("evaluate_for_cdp error: {}", e);
                    crate::js::runtime::RemoteObjectInfo {
                        js_type: "undefined".into(),
                        subtype: None,
                        class_name: String::new(),
                        description: String::new(),
                        object_id: None,
                        value: None,
                    }
                }
            }
        } else {
            let val = self.evaluate(expression);
            crate::js::runtime::RemoteObjectInfo {
                js_type: match &val {
                    serde_json::Value::String(_) => "string".into(),
                    serde_json::Value::Number(_) => "number".into(),
                    serde_json::Value::Bool(_) => "boolean".into(),
                    _ => "undefined".into(),
                },
                subtype: None,
                class_name: String::new(),
                description: String::new(),
                object_id: None,
                value: Some(val),
            }
        }
    }

    pub async fn call_function_on_for_cdp(
        &mut self,
        function_declaration: &str,
        object_id: Option<&str>,
        args: &[serde_json::Value],
        return_by_value: bool,
        await_promise: bool,
    ) -> crate::js::runtime::RemoteObjectInfo {
        if let Some(js) = &mut self.js {
            match js
                .call_function_on_for_cdp(function_declaration, object_id, args, return_by_value, await_promise)
                .await
            {
                Ok(info) => info,
                Err(e) => {
                    tracing::debug!("callFunctionOn error: {}", e);
                    crate::js::runtime::RemoteObjectInfo {
                        js_type: "undefined".into(),
                        subtype: None,
                        class_name: String::new(),
                        description: String::new(),
                        object_id: None,
                        value: None,
                    }
                }
            }
        } else {
            crate::js::runtime::RemoteObjectInfo {
                js_type: "undefined".into(),
                subtype: None,
                class_name: String::new(),
                description: String::new(),
                object_id: None,
                value: None,
            }
        }
    }

    pub fn set_blocked_urls(&mut self, patterns: Vec<String>) {
        if let Some(js) = &self.js {
            js.set_blocked_urls(patterns);
        }
    }

    pub fn release_object(&mut self, object_id: &str) {
        if let Some(js) = &mut self.js {
            js.release_object(object_id);
        }
    }

    fn record_network_event(
        &mut self,
        url: &str,
        method: &str,
        resource_type: &str,
        status: u16,
        response_headers: &std::collections::HashMap<String, String>,
        body_size: usize,
    ) {
        self.network_event_counter += 1;
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        self.network_events.push(NetworkEvent {
            request_id: format!("{}.{}", self.id, self.network_event_counter),
            url: url.to_string(),
            method: method.to_string(),
            resource_type: resource_type.to_string(),
            status,
            headers: std::collections::HashMap::new(),
            response_headers: Arc::new(response_headers.clone()),
            body_size,
            timestamp,
        });
    }

    /// ~keep A preload script is operator/config-supplied like `ExecuteJs` and `eval_script`
    /// (#71), so it hits the same hazard: `BrowserJsRuntime::execute_script` is a synchronous,
    /// non-yielding V8 call that a caller-side `tokio::time::timeout` cannot preempt. Routed
    /// through the watchdog-backed `execute_script_with_timeout` with the same 30s bound used
    /// for `ExecuteJs`/`eval_script` rather than a plain call.
    pub fn execute_preload_script(&mut self, source: &str) -> Result<(), String> {
        if let Some(js) = &mut self.js {
            js.execute_script_with_timeout(source, PRELOAD_SCRIPT_TIMEOUT)
        } else {
            Err("No JS runtime".to_string())
        }
    }

    pub fn suspend_js(&mut self) {
        if let Some(js) = &self.js
            && let Some(dom) = js.take_dom()
        {
            self.dom = Some(dom);
        }
        self.js = None;
    }

    pub fn resume_js(&mut self) {
        if self.js.is_some() {
            return;
        }
        self.init_js();
    }

    pub fn has_js(&self) -> bool {
        self.js.is_some()
    }

    pub fn release_object_group(&mut self) {
        if let Some(js) = &mut self.js {
            js.release_object_group();
        }
    }

    pub fn take_pending_navigation(&self) -> Option<(String, String, String)> {
        if let Some(js) = &self.js {
            js.take_pending_navigation()
        } else {
            None
        }
    }

    pub async fn process_pending_navigation(&mut self) -> Result<bool, PageError> {
        if let Some((url, method, body)) = self.take_pending_navigation() {
            self.navigate_with_wait_post(&url, crate::lifecycle::WaitUntil::Load, &method, &body)
                .await?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn set_intercept_tx(&mut self, tx: tokio::sync::mpsc::UnboundedSender<crate::js::ops::InterceptedRequest>) {
        self.intercept_tx = Some(tx.clone());
        if let Some(js) = &self.js {
            js.set_intercept_tx(tx);
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PageError {
    #[error("Invalid URL: {0}")]
    InvalidUrl(String),

    #[error("Network error: {0}")]
    NetworkError(String),

    #[error("Parse error: {0}")]
    ParseError(String),

    #[error("Too many redirects (limit {0})")]
    TooManyRedirects(usize),

    /// The render configuration cannot be used, so nothing was fetched.
    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),
}

impl From<NetError> for PageError {
    fn from(e: NetError) -> Self {
        match e {
            NetError::InvalidProxy(reason) => PageError::InvalidConfig(reason.to_string()),
            // ~keep Both variants print "Network error: "; keep one.
            NetError::Network(message) => PageError::NetworkError(message),
            other => PageError::NetworkError(other.to_string()),
        }
    }
}
