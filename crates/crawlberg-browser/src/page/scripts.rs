//! Classic and module `<script>` collection, fetching and execution.

use url::Url;

use super::Page;
use super::security::subresource_allowed;
use crate::dom::DomTree;
use crate::net::Response;

/// Wall-clock budget for draining the microtask/event queue once a document's scripts have run.
const EVENT_LOOP_DRAIN_BUDGET: std::time::Duration = std::time::Duration::from_millis(500);
/// How long a single `run_event_loop` turn may take before the drain loop re-checks its deadline.
const EVENT_LOOP_TICK: std::time::Duration = std::time::Duration::from_millis(10);
/// Pause between drain turns while sub-resource requests are still in flight.
const EVENT_LOOP_BUSY_BACKOFF: std::time::Duration = std::time::Duration::from_millis(1);
/// Consecutive quiet turns required before the drain loop calls the page settled.
const REQUIRED_IDLE_TICKS: u32 = 2;
/// Status recorded for the synthetic network event of a successfully loaded ES module.
const SYNTHETIC_MODULE_EVENT_STATUS: u16 = 200;
#[derive(Debug)]
struct ScriptInfo {
    src: Option<String>,
    inline: String,
    is_defer: bool,
    is_async: bool,
    is_module: bool,
}

/// Whether a `<script type>` names a classic or module script the browser should execute.
///
/// An empty `type` means classic JavaScript; anything else unrecognised (`text/template`,
/// `application/json`, an import map) is data the page reads itself, not code to run.
fn is_executable_script_type(script_type: &str) -> bool {
    script_type.is_empty()
        || script_type == "text/javascript"
        || script_type == "application/javascript"
        || script_type == "module"
}

fn collect_scripts_from_dom(dom: &DomTree) -> Vec<ScriptInfo> {
    let mut scripts = Vec::new();
    for script_id in dom.query_selector_all("script").unwrap_or_default() {
        let Some(node) = dom.get_node(script_id) else {
            continue;
        };
        let script_type = node.get_attribute("type").unwrap_or("").to_string();
        if !is_executable_script_type(&script_type) {
            continue;
        }

        let src = node.get_attribute("src").map(|s| s.to_string());
        let inline = if src.is_none() {
            dom.text_content(script_id)
        } else {
            String::new()
        };
        if src.is_none() && inline.trim().is_empty() {
            continue;
        }

        scripts.push(ScriptInfo {
            is_defer: node.get_attribute("defer").is_some(),
            is_async: node.get_attribute("async").is_some(),
            is_module: script_type == "module",
            src,
            inline,
        });
    }
    scripts
}

/// Split scripts into the classic execution order (regular, then `defer`, then `async`) and
/// the module scripts, which run after every classic script.
fn partition_scripts(all_scripts: Vec<ScriptInfo>) -> (Vec<ScriptInfo>, Vec<ScriptInfo>) {
    let mut regular = Vec::new();
    let mut deferred = Vec::new();
    let mut async_scripts = Vec::new();
    let mut module_scripts = Vec::new();

    for script in all_scripts {
        if script.is_module {
            module_scripts.push(script);
        } else if script.is_defer {
            deferred.push(script);
        } else if script.is_async {
            async_scripts.push(script);
        } else {
            regular.push(script);
        }
    }

    tracing::info!(
        "Found {} regular + {} deferred + {} async scripts",
        regular.len(),
        deferred.len(),
        async_scripts.len()
    );

    let classic = regular.into_iter().chain(deferred).chain(async_scripts).collect();
    (classic, module_scripts)
}
impl Page {
    pub(super) async fn execute_scripts(&mut self) {
        tracing::info!("execute_scripts called, js runtime exists: {}", self.js.is_some());

        let Some(all_scripts) = self.collect_scripts() else {
            return;
        };
        let (classic_scripts, module_scripts) = partition_scripts(all_scripts);

        let fetched = self.fetch_classic_script_sources(&classic_scripts).await;
        self.run_classic_scripts(&classic_scripts, fetched);
        self.run_module_scripts(&module_scripts).await;
        self.fire_load_events();
        self.drain_event_loop().await;
    }

    /// `None` when the page has no JS realm, which means no script may run at all.
    fn collect_scripts(&self) -> Option<Vec<ScriptInfo>> {
        let js = self.js.as_ref()?;
        Some(js.with_dom(collect_scripts_from_dom).unwrap_or_default())
    }

    /// Indices and resolved URLs of the `src` scripts that policy and interception permit.
    fn allowed_script_urls(&self, scripts: &[ScriptInfo]) -> Vec<(usize, String)> {
        let mut allowed = Vec::new();
        for (index, script) in scripts.iter().enumerate() {
            let Some(src) = &script.src else {
                continue;
            };
            let full_url = self.resolve_subresource_url(src);

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
            allowed.push((index, full_url));
        }
        allowed
    }

    /// Fetch every permitted external script concurrently, keyed by its index in `scripts`.
    async fn fetch_classic_script_sources(
        &self,
        scripts: &[ScriptInfo],
    ) -> std::collections::HashMap<usize, (String, String, Response)> {
        let client = self.http_client.clone();
        let fetch_futures: Vec<_> = self
            .allowed_script_urls(scripts)
            .into_iter()
            .map(|(index, url)| {
                let client = client.clone();
                async move {
                    let parsed = Url::parse(&url).unwrap_or_else(|_| Url::parse("about:blank").unwrap());
                    match client.fetch(&parsed).await {
                        Ok(response) => Some((index, url, response)),
                        Err(error) => {
                            tracing::warn!("Failed to fetch script {}: {}", url, error);
                            None
                        }
                    }
                }
            })
            .collect();

        let mut fetched = std::collections::HashMap::new();
        for (index, url, response) in futures::future::join_all(fetch_futures).await.into_iter().flatten() {
            let code = String::from_utf8_lossy(&response.body).to_string();
            fetched.insert(index, (url, code, response));
        }
        fetched
    }

    fn run_classic_scripts(
        &mut self,
        scripts: &[ScriptInfo],
        mut fetched: std::collections::HashMap<usize, (String, String, Response)>,
    ) {
        for (index, script) in scripts.iter().enumerate() {
            if script.src.is_some()
                && let Some((url, code, response)) = fetched.remove(&index)
            {
                tracing::info!("Executing script ({} bytes): {}", code.len(), url);
                self.record_network_event(
                    &url,
                    "GET",
                    "Script",
                    response.status,
                    &response.headers,
                    response.body.len(),
                );
                if let Some(js) = &mut self.js
                    && let Err(error) = js.execute_script_guarded(&url, &code)
                {
                    tracing::warn!("Script error ({}): {}", url, error);
                }
            } else if !script.inline.is_empty()
                && let Some(js) = &mut self.js
                && let Err(error) = js.execute_script_guarded("<inline>", &script.inline)
            {
                tracing::warn!("Inline script error: {}", error);
            }
        }
    }

    async fn run_module_scripts(&mut self, module_scripts: &[ScriptInfo]) {
        for module_script in module_scripts {
            if let Some(src) = &module_script.src {
                let full_url = self.resolve_subresource_url(src);
                self.load_remote_module(&full_url).await;
            } else if !module_script.inline.is_empty() {
                let base = self.url_string();
                if let Some(js) = &mut self.js
                    && let Err(error) = js.load_inline_module(&module_script.inline, &base).await
                {
                    tracing::warn!("Inline ES module error: {}", error);
                }
            }
        }
    }

    async fn load_remote_module(&mut self, full_url: &str) {
        tracing::info!("Loading ES module: {}", full_url);
        if let Some(js) = &mut self.js {
            match js.load_module(full_url).await {
                Ok(()) => {
                    tracing::info!("ES module loaded: {}", full_url);
                    self.record_network_event(
                        full_url,
                        "GET",
                        "Script",
                        SYNTHETIC_MODULE_EVENT_STATUS,
                        &std::collections::HashMap::new(),
                        0,
                    );
                }
                Err(error) => {
                    tracing::warn!("ES module error ({}): {}", full_url, error);
                }
            }
        }
    }

    fn fire_load_events(&mut self) {
        if let Some(js) = &mut self.js {
            let _ = js.execute_script(
                "<load-events>",
                "if (typeof window.onload === 'function') { try { window.onload(); } catch(e) {} }\n\
                 try { document.dispatchEvent(new Event('DOMContentLoaded')); } catch(e) {}\n\
                 try { window.dispatchEvent(new Event('load')); } catch(e) {}",
            );
        }
    }

    /// Pump the JS event loop until the page goes quiet or the drain budget runs out.
    async fn drain_event_loop(&mut self) {
        let Some(js) = &mut self.js else {
            return;
        };
        let deadline = tokio::time::Instant::now() + EVENT_LOOP_DRAIN_BUDGET;
        let mut idle_ticks = 0u32;

        loop {
            match tokio::time::timeout(EVENT_LOOP_TICK, js.run_event_loop()).await {
                Ok(Ok(())) => {
                    if self.http_client.active_requests() > 0 {
                        idle_ticks = 0;
                        tokio::time::sleep(EVENT_LOOP_BUSY_BACKOFF).await;
                        continue;
                    }
                    idle_ticks += 1;
                    if idle_ticks >= REQUIRED_IDLE_TICKS {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                Ok(Err(_)) => break,
                Err(_) => {
                    idle_ticks = 0;
                    if tokio::time::Instant::now() >= deadline {
                        break;
                    }
                }
            }
        }
    }
}
