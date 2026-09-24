use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::dom::DomTree;
use deno_core::{JsRuntime, RuntimeOptions};

use super::ops::{JsOpState, build_extension};
use crate::js::module_loader::BrowserModuleLoader;

mod convert;
mod eval;
mod modules;
mod objects;
mod script;
#[cfg(test)]
mod tests;

static SNAPSHOT: &[u8] = include_bytes!(env!("CRAWLBERG_BROWSER_SNAPSHOT_PATH"));

#[derive(Debug, Clone)]
pub struct RemoteObjectInfo {
    pub js_type: String,
    pub subtype: Option<String>,
    pub class_name: String,
    pub description: String,
    pub object_id: Option<String>,
    pub value: Option<serde_json::Value>,
}

pub struct BrowserJsRuntime {
    runtime: JsRuntime,
    state: Rc<RefCell<JsOpState>>,
    object_store: HashMap<String, String>,
    object_counter: u64,
}

impl BrowserJsRuntime {
    pub fn new() -> Self {
        Self::with_base_url("about:blank")
    }

    pub fn with_base_url(base_url: &str) -> Self {
        Self::with_base_url_and_proxy(base_url, None)
    }

    /// Construct a runtime whose ES-module loader routes dynamic imports
    /// through `proxy_url` (#139). `None` is equivalent to `with_base_url`
    /// (direct connection).
    pub fn with_base_url_and_proxy(base_url: &str, proxy_url: Option<String>) -> Self {
        Self::with_base_url_proxy_and_ssrf(
            base_url,
            proxy_url,
            std::sync::Arc::new(crate::net::ssrf::DefaultSsrfValidator::from_env()),
        )
    }

    /// Construct a runtime whose module loader *and* op state both carry `ssrf`.
    ///
    /// The module loader is built here, so a validator set afterwards via
    /// [`Self::set_ssrf_validator`] would not reach dynamic `import()`.
    pub fn with_base_url_proxy_and_ssrf(
        base_url: &str,
        proxy_url: Option<String>,
        ssrf: std::sync::Arc<dyn crate::net::ssrf::SsrfValidator>,
    ) -> Self {
        let state = Rc::new(RefCell::new(JsOpState::new()));
        state.borrow_mut().ssrf = ssrf.clone();
        let state_clone = state.clone();

        let module_loader = Rc::new(BrowserModuleLoader::with_ssrf(base_url, proxy_url, ssrf));

        // ~keep deno_core captures `Handle::try_current().ok()` when it registers the isolate
        // ~keep and, if that handle is `None`, calls `std::process::abort()` from a V8
        // ~keep background thread the moment V8 posts a delayed task for it (the GC memory
        // ~keep reducer is the usual one). That is a bare SIGABRT with no panic, no backtrace
        // ~keep and no failing test name -- issue #48. The abort window is the isolate's whole
        // ~keep lifetime, so this has to be caught at construction, not at first use.
        debug_assert!(
            tokio::runtime::Handle::try_current().is_ok(),
            "BrowserJsRuntime must be constructed inside a tokio runtime context; deno_core \
             aborts the process instead of panicking when V8 posts a delayed task for an \
             isolate registered without one"
        );

        let mut runtime = JsRuntime::new(RuntimeOptions {
            extensions: vec![build_extension()],
            module_loader: Some(module_loader),
            startup_snapshot: Some(SNAPSHOT),
            ..Default::default()
        });

        runtime.op_state().borrow_mut().put(state_clone);

        runtime
            .execute_script(
                "<crawlberg:init>",
                "globalThis.__crawlberg_objects = {}; globalThis.__crawlberg_oid = 0; globalThis.__crawlberg_init();"
                    .to_string(),
            )
            .expect("init should not fail");

        BrowserJsRuntime {
            runtime,
            state,
            object_store: HashMap::new(),
            object_counter: 0,
        }
    }

    pub fn set_cookie_jar(&self, jar: std::sync::Arc<crate::net::CookieJar>) {
        self.state.borrow_mut().cookie_jar = Some(jar);
    }

    pub fn set_http_client(&self, client: std::sync::Arc<crate::net::HttpClient>) {
        self.state.borrow_mut().http_client = Some(client);
    }

    /// Apply an SSRF policy to page-initiated `fetch`/XHR and dynamic `import()`.
    ///
    /// The JS bridge builds its own HTTP client, so it does not inherit the policy set
    /// on [`crate::net::HttpClient`] and must be told separately.
    pub fn set_ssrf_validator(&self, validator: std::sync::Arc<dyn crate::net::ssrf::SsrfValidator>) {
        self.state.borrow_mut().ssrf = validator;
    }

    pub fn set_dom(&self, dom: DomTree) {
        self.state.borrow_mut().dom = Some(dom);
    }

    pub fn set_url(&self, url: &str) {
        self.state.borrow_mut().url = url.to_string();
    }

    pub fn set_title(&self, title: &str) {
        self.state.borrow_mut().title = title.to_string();
    }

    pub fn set_blocked_urls(&self, patterns: Vec<String>) {
        self.state.borrow_mut().blocked_urls = patterns;
    }

    pub fn take_pending_navigation(&self) -> Option<(String, String, String)> {
        self.state.borrow_mut().pending_navigation.take()
    }

    pub fn set_intercept_tx(&self, tx: tokio::sync::mpsc::UnboundedSender<super::ops::InterceptedRequest>) {
        let mut state = self.state.borrow_mut();
        state.intercept_tx = Some(tx);
        state.intercept_enabled = true;
    }

    pub fn set_user_agent(&mut self, ua: &str) {
        let escaped = ua.replace('\\', "\\\\").replace('\'', "\\'");
        let _ = self
            .runtime
            .execute_script("<set-ua>", format!("globalThis.__crawlberg_ua = '{}';", escaped));
    }

    pub fn take_dom(&self) -> Option<DomTree> {
        self.state.borrow_mut().dom.take()
    }

    pub fn with_dom<R>(&self, f: impl FnOnce(&DomTree) -> R) -> Option<R> {
        let state = self.state.borrow();
        state.dom.as_ref().map(f)
    }

    pub fn dom_ref(&self) -> Option<std::cell::Ref<'_, Option<DomTree>>> {
        let r = self.state.borrow();
        if r.dom.is_some() {
            Some(std::cell::Ref::map(r, |s| &s.dom))
        } else {
            None
        }
    }
}

impl Default for BrowserJsRuntime {
    fn default() -> Self {
        Self::new()
    }
}
