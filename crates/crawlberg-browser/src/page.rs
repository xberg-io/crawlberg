use std::sync::Arc;

use crate::dom::{DomTree, parse_html};
use crate::js::runtime::BrowserJsRuntime;
use crate::net::{HttpClient, NetError, Response};
use url::Url;

use crate::context::BrowserContext;
use crate::lifecycle::LifecycleState;

#[cfg(feature = "stealth")]
use crate::net::StealthHttpClient;

/// Wall-clock bound on [`Page::execute_preload_script`], mirroring `adapter::EVAL_SCRIPT_TIMEOUT`
/// and `adapter::EXECUTE_JS_TIMEOUT` (#71): a fixed, watchdog-enforced ceiling for
/// operator/config-supplied scripts, independent of any caller-configured, potentially
/// unbounded timeout.
const PRELOAD_SCRIPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Returns true when a JS-initiated navigation would step from a
/// non-file scheme into a file: URL. We treat that move as an SOP
/// violation because the existing realm survives the navigation and
/// can read the new document's body.
fn cross_scheme_to_file(from: &str, to: &str) -> bool {
    let to_is_file = Url::parse(to)
        .map(|u| u.scheme().eq_ignore_ascii_case("file"))
        .unwrap_or(false);
    if !to_is_file {
        return false;
    }
    Url::parse(from)
        .map(|u| !u.scheme().eq_ignore_ascii_case("file"))
        .unwrap_or(true)
}

/// Sub-resource fetch policy. A page may only pull a `<script src>` /
/// `<link rel=stylesheet href>` / etc. when the URL scheme is safe for
/// the page's origin. http(s) pages cannot reach into file: or data:
/// to fabricate scripts, and pages with no origin only get http/https.
fn subresource_allowed(page_url: Option<&Url>, resource: &str) -> bool {
    let Ok(target) = Url::parse(resource) else { return false };
    let scheme = target.scheme().to_ascii_lowercase();
    match scheme.as_str() {
        "http" | "https" => true,
        "file" => page_url
            .map(|u| u.scheme().eq_ignore_ascii_case("file"))
            .unwrap_or(false),
        _ => false,
    }
}

/// Escape a value for safe inclusion inside a JavaScript template
/// literal. The previous implementation only escaped `\`, `` ` `` and
/// `${`; that left U+2028 / U+2029 (the JS-specific line terminators)
/// and other control characters as breakout vectors. Done at the
/// callsite means future tweaks come back to one function.
fn escape_for_js_template_literal(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '`' => out.push_str("\\`"),
            '$' => out.push_str("\\$"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            '\u{0000}' => out.push_str("\\0"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

#[derive(Debug, Clone)]
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
        let stealth_client = if context.stealth {
            // ~keep `wreq` cannot speak SOCKS5; validate schemes instead of rewriting `socks5://` to `http://`.
            // ~keep Share the plain client's SSRF policy: the stealth path is an
            // alternate transport, not an alternate policy.
            Some(Arc::new(StealthHttpClient::with_ssrf(
                context.cookie_jar.clone(),
                context.proxy_url.as_deref(),
                http_client.ssrf.clone(),
            )))
        } else {
            None
        };

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
        if !self.intercept_enabled || self.intercept_block_patterns.is_empty() {
            return false;
        }
        for pattern in &self.intercept_block_patterns {
            if pattern == "*" {
                return true;
            }
            if pattern.starts_with('*') && pattern.ends_with('*') {
                if url.contains(&pattern[1..pattern.len() - 1]) {
                    return true;
                }
            } else if let Some(suffix) = pattern.strip_prefix('*') {
                if url.ends_with(suffix) {
                    return true;
                }
            } else if let Some(prefix) = pattern.strip_suffix('*') {
                if url.starts_with(prefix) {
                    return true;
                }
            } else if url.contains(pattern.as_str()) {
                return true;
            }
        }
        false
    }

    async fn do_fetch(&self, url: &Url) -> Result<Response, NetError> {
        #[cfg(feature = "stealth")]
        if let Some(ref stealth) = self.stealth_client {
            return stealth.fetch(url).await;
        }
        self.http_client.fetch(url).await
    }
    fn init_js(&mut self) {
        // ~keep Recreate the JS realm every navigation so prior-page handlers cannot run in the next document.
        if self.js.is_some() {
            let _ = self.js.take();
        }

        // ~keep Thread the context proxy into ES modules and JS fetch/XHR so page JS honors upstream proxy settings.
        let mut rt = BrowserJsRuntime::with_base_url_proxy_and_ssrf(
            &self.url_string(),
            self.context.proxy_url.clone(),
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

        if let Some(tx) = &self.intercept_tx {
            rt.set_intercept_tx(tx.clone());
        }

        if let Some(dom) = self.dom.take() {
            rt.set_dom(dom);
        }

        self.js = Some(rt);
    }

    async fn execute_scripts(&mut self) {
        tracing::info!("execute_scripts called, js runtime exists: {}", self.js.is_some());

        #[derive(Debug)]
        struct ScriptInfo {
            src: Option<String>,
            inline: String,
            is_defer: bool,
            is_async: bool,
            is_module: bool,
        }

        let all_scripts = match &self.js {
            Some(js) => js
                .with_dom(|dom| {
                    let script_ids = dom.query_selector_all("script").unwrap_or_default();
                    let mut scripts = Vec::new();

                    for sid in script_ids {
                        if let Some(node) = dom.get_node(sid) {
                            let src = node.get_attribute("src").map(|s| s.to_string());
                            let script_type = node.get_attribute("type").unwrap_or("").to_string();
                            let is_defer = node.get_attribute("defer").is_some();
                            let is_async = node.get_attribute("async").is_some();
                            let is_module = script_type == "module";

                            if !script_type.is_empty()
                                && script_type != "text/javascript"
                                && script_type != "application/javascript"
                                && script_type != "module"
                            {
                                continue;
                            }

                            let inline_code = if src.is_none() {
                                dom.text_content(sid)
                            } else {
                                String::new()
                            };

                            if src.is_some() || !inline_code.trim().is_empty() {
                                scripts.push(ScriptInfo {
                                    src,
                                    inline: inline_code,
                                    is_defer,
                                    is_async,
                                    is_module,
                                });
                            }
                        }
                    }
                    scripts
                })
                .unwrap_or_default(),
            None => return,
        };

        let mut regular = Vec::new();
        let mut deferred = Vec::new();
        let mut async_scripts = Vec::new();

        let mut module_scripts = Vec::new();

        for script in all_scripts {
            if script.is_module {
                module_scripts.push(script);
                continue;
            }
            if script.is_defer {
                deferred.push(script);
            } else if script.is_async {
                async_scripts.push(script);
            } else {
                regular.push(script);
            }
        }

        let scripts = regular;

        tracing::info!(
            "Found {} regular + {} deferred + {} async scripts",
            scripts.len(),
            deferred.len(),
            async_scripts.len()
        );
        let all_to_execute: Vec<ScriptInfo> = scripts.into_iter().chain(deferred).chain(async_scripts).collect();

        let mut resolved: Vec<(usize, String)> = Vec::new();
        let mut fetch_tasks: Vec<(usize, String)> = Vec::new();

        for (i, script) in all_to_execute.iter().enumerate() {
            if let Some(src_url) = &script.src {
                let full_url = if src_url.starts_with("http://") || src_url.starts_with("https://") {
                    src_url.clone()
                } else if let Some(base) = &self.url {
                    base.join(src_url)
                        .map(|u| u.to_string())
                        .unwrap_or_else(|_| src_url.clone())
                } else {
                    src_url.clone()
                };

                if !subresource_allowed(self.url.as_ref(), &full_url) {
                    // ~keep Block off-origin script schemes so an HTTP page cannot read local files as JS source.
                    tracing::warn!(
                        "blocking cross-scheme <script src>: page={} src={}",
                        self.url_string(),
                        full_url,
                    );
                    continue;
                }
                if self.should_block_url(&full_url) {
                    tracing::info!("Blocked script by interception: {}", full_url);
                    continue;
                }
                resolved.push((i, full_url.clone()));
                fetch_tasks.push((i, full_url));
            }
        }

        let client = self.http_client.clone();
        let fetch_futures: Vec<_> = fetch_tasks
            .iter()
            .map(|(idx, url)| {
                let client = client.clone();
                let url = url.clone();
                let idx = *idx;
                async move {
                    let parsed = Url::parse(&url).unwrap_or_else(|_| Url::parse("about:blank").unwrap());
                    match client.fetch(&parsed).await {
                        Ok(resp) => Some((idx, url, resp)),
                        Err(e) => {
                            tracing::warn!("Failed to fetch script {}: {}", url, e);
                            None
                        }
                    }
                }
            })
            .collect();

        let fetch_results = futures::future::join_all(fetch_futures).await;

        let mut fetched: std::collections::HashMap<usize, (String, String, crate::net::Response)> =
            std::collections::HashMap::new();
        for (idx, url, resp) in fetch_results.into_iter().flatten() {
            let code = String::from_utf8_lossy(&resp.body).to_string();
            fetched.insert(idx, (url, code, resp));
        }

        for (i, script) in all_to_execute.iter().enumerate() {
            if script.src.is_some()
                && let Some((url, code, resp)) = fetched.remove(&i)
            {
                tracing::info!("Executing script ({} bytes): {}", code.len(), url);
                self.record_network_event(&url, "GET", "Script", resp.status, &resp.headers, resp.body.len());
                if let Some(js) = &mut self.js
                    && let Err(e) = js.execute_script_guarded(&url, &code)
                {
                    tracing::warn!("Script error ({}): {}", url, e);
                }
            } else if !script.inline.is_empty()
                && let Some(js) = &mut self.js
                && let Err(e) = js.execute_script_guarded("<inline>", &script.inline)
            {
                tracing::warn!("Inline script error: {}", e);
            }
        }

        for module_script in &module_scripts {
            if let Some(ref src) = module_script.src {
                let full_url = if src.starts_with("http://") || src.starts_with("https://") {
                    src.clone()
                } else if let Some(base) = &self.url {
                    base.join(src).map(|u| u.to_string()).unwrap_or_else(|_| src.clone())
                } else {
                    src.clone()
                };

                tracing::info!("Loading ES module: {}", full_url);
                if let Some(js) = &mut self.js {
                    match js.load_module(&full_url).await {
                        Ok(()) => {
                            tracing::info!("ES module loaded: {}", full_url);
                            self.record_network_event(
                                &full_url,
                                "GET",
                                "Script",
                                200,
                                &std::collections::HashMap::new(),
                                0,
                            );
                        }
                        Err(e) => {
                            tracing::warn!("ES module error ({}): {}", full_url, e);
                        }
                    }
                }
            } else if !module_script.inline.is_empty() {
                let base = self.url_string();
                if let Some(js) = &mut self.js
                    && let Err(e) = js.load_inline_module(&module_script.inline, &base).await
                {
                    tracing::warn!("Inline ES module error: {}", e);
                }
            }
        }

        if let Some(js) = &mut self.js {
            let _ = js.execute_script(
                "<load-events>",
                "if (typeof window.onload === 'function') { try { window.onload(); } catch(e) {} }\n\
                 try { document.dispatchEvent(new Event('DOMContentLoaded')); } catch(e) {}\n\
                 try { window.dispatchEvent(new Event('load')); } catch(e) {}",
            );
        }

        if let Some(js) = &mut self.js {
            let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(500);
            let mut idle_count = 0u32;
            loop {
                let result = tokio::time::timeout(tokio::time::Duration::from_millis(10), js.run_event_loop()).await;

                match result {
                    Ok(Ok(())) => {
                        if self.http_client.active_requests() == 0 {
                            idle_count += 1;
                            if idle_count >= 2 {
                                break;
                            }
                            tokio::task::yield_now().await;
                        } else {
                            idle_count = 0;
                            tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
                        }
                    }
                    Ok(Err(_)) => break,
                    Err(_) => {
                        idle_count = 0;
                        if tokio::time::Instant::now() >= deadline {
                            break;
                        }
                    }
                }
            }
        }
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
        let mut current_url = url_str.to_string();
        let mut current_method = method.to_string();
        let mut current_body = body.to_string();
        const REDIRECT_LIMIT: usize = 10;
        for chain in 0..REDIRECT_LIMIT {
            self.navigate_single(&current_url, wait_until, &current_method, &current_body)
                .await?;
            if let Some((next_url, next_method, next_body)) = self.take_pending_navigation() {
                if cross_scheme_to_file(&current_url, &next_url) {
                    // ~keep SOP gate: HTTP(S) pages must not navigate to `file:` and then read the loaded document.
                    tracing::warn!(
                        "blocking JS-initiated cross-scheme navigation to file: {} -> {}",
                        current_url,
                        next_url,
                    );
                    break;
                }
                tracing::info!(
                    "JS-triggered navigation chain: {} {} -> {}",
                    current_method,
                    current_url,
                    next_url
                );
                current_url = next_url;
                current_method = next_method;
                current_body = next_body;
                if chain + 1 == REDIRECT_LIMIT {
                    // ~keep Exceeding the JS navigation cap is an error so redirect storms are not reported as loads.
                    return Err(PageError::TooManyRedirects(REDIRECT_LIMIT));
                }
                continue;
            }
            break;
        }
        Ok(())
    }

    async fn navigate_single(
        &mut self,
        url_str: &str,
        wait_until: crate::lifecycle::WaitUntil,
        method: &str,
        body: &str,
    ) -> Result<(), PageError> {
        let url = Url::parse(url_str).map_err(|e| PageError::InvalidUrl(e.to_string()))?;

        self.lifecycle = LifecycleState::Loading;
        self.url = Some(url.clone());
        self.network_events.clear();

        if self.context.obey_robots
            && let Some(domain) = url.host_str()
        {
            if self.context.robots_cache.is_allowed(domain, "/robots.txt") {
                let robots_url = format!("{}://{}/robots.txt", url.scheme(), domain);
                if let Ok(robots_url) = Url::parse(&robots_url)
                    && let Ok(resp) = self.http_client.fetch(&robots_url).await
                    && resp.status == 200
                {
                    let body = String::from_utf8_lossy(&resp.body);
                    self.context
                        .robots_cache
                        .parse_and_store(domain, &body, &self.context.user_agent);
                }
            }

            if !self.context.robots_cache.is_allowed(domain, url.path()) {
                self.lifecycle = LifecycleState::Failed;
                return Err(PageError::NetworkError(format!("Blocked by robots.txt: {}", url)));
            }
        }

        let response = if method == "POST" {
            self.http_client.post_form(&url, body).await
        } else {
            self.do_fetch(&url).await
        }
        .map_err(|e| {
            self.lifecycle = LifecycleState::Failed;
            PageError::NetworkError(e.to_string())
        })?;

        self.record_network_event(
            url.as_str(),
            "GET",
            "Document",
            response.status,
            &response.headers,
            response.body.len(),
        );

        if !response.redirected_from.is_empty() {
            self.url = Some(response.url.clone());
        }

        let body_text = String::from_utf8_lossy(&response.body).to_string();
        let dom = parse_html(&body_text);

        self.title = dom
            .query_selector("title")
            .ok()
            .flatten()
            .map(|title_id| dom.text_content(title_id))
            .unwrap_or_default();

        let stylesheet_urls: Vec<String> = dom
            .query_selector_all("link")
            .unwrap_or_default()
            .iter()
            .filter_map(|&nid| {
                let node = dom.get_node(nid)?;
                let rel = node.get_attribute("rel")?;
                if rel.to_lowercase() != "stylesheet" {
                    return None;
                }
                node.get_attribute("href").map(|s| s.to_string())
            })
            .collect();

        let mut css_fetch_urls: Vec<String> = Vec::new();
        for href in &stylesheet_urls {
            let full_url = if href.starts_with("http://") || href.starts_with("https://") {
                href.clone()
            } else if let Some(base) = &self.url {
                base.join(href).map(|u| u.to_string()).unwrap_or_else(|_| href.clone())
            } else {
                href.clone()
            };
            if !subresource_allowed(self.url.as_ref(), &full_url) {
                tracing::warn!(
                    "blocking cross-scheme <link rel=stylesheet href>: page={} href={}",
                    self.url_string(),
                    full_url,
                );
                continue;
            }
            if self.should_block_url(&full_url) {
                tracing::info!("Blocked stylesheet by interception: {}", full_url);
                continue;
            }
            css_fetch_urls.push(full_url);
        }

        let client = self.http_client.clone();
        let css_futures: Vec<_> = css_fetch_urls
            .iter()
            .map(|full_url| {
                let client = client.clone();
                let url_str = full_url.clone();
                async move {
                    let parsed = Url::parse(&url_str).unwrap_or_else(|_| Url::parse("about:blank").unwrap());
                    match client.fetch(&parsed).await {
                        Ok(resp) => Some((url_str, resp)),
                        Err(e) => {
                            tracing::debug!("Failed to fetch stylesheet {}: {}", url_str, e);
                            None
                        }
                    }
                }
            })
            .collect();

        let css_results = futures::future::join_all(css_futures).await;
        let mut css_sources = Vec::new();
        for (url_str, resp) in css_results.into_iter().flatten() {
            let css = String::from_utf8_lossy(&resp.body).to_string();
            self.record_network_event(
                &url_str,
                "GET",
                "Stylesheet",
                resp.status,
                &resp.headers,
                resp.body.len(),
            );
            css_sources.push(css);
        }

        self.dom = Some(dom);
        self.lifecycle = LifecycleState::DomContentLoaded;

        if wait_until == crate::lifecycle::WaitUntil::DomContentLoaded {
            self.init_js();
            return Ok(());
        }

        self.init_js();

        if !css_sources.is_empty()
            && let Some(js) = &mut self.js
        {
            let combined_css = css_sources.join("\n");
            // ~keep Escape JS template literals fully so attacker-controlled CSS cannot break into executable JS.
            let escaped = escape_for_js_template_literal(&combined_css);
            let code = format!("globalThis.__crawlberg_css = `{}`;", escaped);
            let _ = js.execute_script("<css>", &code);
        }
        if let Some(js) = &mut self.js {
            let _ = js.execute_script("<iframe-load>",
                "(function() { var iframes = document.querySelectorAll('iframe[src]'); for (var i = 0; i < iframes.length; i++) { var src = iframes[i].getAttribute('src'); if (src && src !== 'about:blank') iframes[i]._loadIframeSrc(src); } })()");
        }

        self.execute_scripts().await;

        if let Some(js) = &mut self.js
            && let Ok(new_title) = js.evaluate("document.title")
            && let Some(t) = new_title.as_str()
        {
            self.title = t.to_string();
        }

        self.lifecycle = LifecycleState::Loaded;

        if matches!(
            wait_until,
            crate::lifecycle::WaitUntil::NetworkIdle0 | crate::lifecycle::WaitUntil::NetworkIdle2
        ) {
            let threshold = match wait_until {
                crate::lifecycle::WaitUntil::NetworkIdle0 => 0,
                crate::lifecycle::WaitUntil::NetworkIdle2 => 2,
                _ => 0,
            };

            let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
            let mut idle_since: Option<tokio::time::Instant> = None;

            loop {
                let active = self.http_client.active_requests();
                let now = tokio::time::Instant::now();

                if active <= threshold {
                    if idle_since.is_none() {
                        idle_since = Some(now);
                    }
                    if now.duration_since(idle_since.unwrap()) >= tokio::time::Duration::from_millis(500) {
                        break;
                    }
                } else {
                    idle_since = None;
                }

                if now >= deadline {
                    tracing::debug!("Network idle timeout reached with {} active requests", active);
                    break;
                }

                if let Some(js) = &mut self.js {
                    let _ = tokio::time::timeout(tokio::time::Duration::from_millis(50), js.run_event_loop()).await;
                } else {
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                }
            }

            self.lifecycle = LifecycleState::NetworkIdle;
        }

        Ok(())
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
}

impl From<NetError> for PageError {
    fn from(e: NetError) -> Self {
        PageError::NetworkError(e.to_string())
    }
}
