use std::cell::RefCell;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;

use deno_core::ModuleLoadOptions;
use deno_core::ModuleLoadReferrer;
use deno_core::ModuleLoadResponse;
use deno_core::ModuleLoader;
use deno_core::ModuleSource;
use deno_core::ModuleSourceCode;
use deno_core::ModuleSpecifier;
use deno_core::error::ModuleLoaderError;

use crate::js::ops::{JsOpState, SharedState};
use crate::net::credential::{has_userinfo, without_userinfo};
use crate::net::interceptor::matches_block_pattern;
use crate::net::resolver::with_policy_resolver;
use crate::net::ssrf::{DefaultSsrfValidator, SsrfValidator};

pub struct BrowserModuleLoader {
    pub base_url: String,
    /// Proxy URL threaded through to every dynamic ES-module fetch (#139).
    /// `None` keeps the pre-#139 direct-connection behaviour for callers
    /// that haven't been updated.
    pub proxy_url: Option<String>,
    /// SSRF policy applied to every dynamic `import()`. Page JS chooses these URLs,
    /// so they are as untrusted as any other page-initiated fetch.
    pub ssrf: Arc<dyn SsrfValidator>,
    /// The page state the runtime shares with its ops. A module on the scoped host gets the
    /// page client's [`OriginHeaders`](crate::net::OriginHeaders), as a `fetch()` does.
    pub op_state: SharedState,
}

impl BrowserModuleLoader {
    pub fn new(base_url: &str) -> Self {
        Self::with_proxy(base_url, None)
    }

    pub fn with_proxy(base_url: &str, proxy_url: Option<String>) -> Self {
        Self::with_ssrf(
            base_url,
            proxy_url,
            Arc::new(DefaultSsrfValidator::from_env()),
            Rc::new(RefCell::new(JsOpState::new())),
        )
    }

    pub fn with_ssrf(
        base_url: &str,
        proxy_url: Option<String>,
        ssrf: Arc<dyn SsrfValidator>,
        op_state: SharedState,
    ) -> Self {
        BrowserModuleLoader {
            base_url: base_url.to_string(),
            proxy_url,
            ssrf,
            op_state,
        }
    }
}

/// Redirects a module fetch follows, as many as reqwest's default policy.
const MODULE_REDIRECT_LIMIT: usize = 10;

fn io_err(msg: String) -> ModuleLoaderError {
    deno_error::JsErrorBox::generic(msg)
}

impl ModuleLoader for BrowserModuleLoader {
    fn resolve(
        &self,
        specifier: &str,
        referrer: &str,
        _kind: deno_core::ResolutionKind,
    ) -> Result<ModuleSpecifier, ModuleLoaderError> {
        let base = if referrer.is_empty() || referrer.starts_with('<') || referrer == "." || referrer == "about:blank" {
            &self.base_url
        } else {
            referrer
        };

        let resolved =
            deno_core::resolve_import(specifier, base).map_err(|e| deno_error::JsErrorBox::generic(e.to_string()))?;
        // ~keep Refused here, before `load` fetches it or prints it in an error.
        if has_userinfo(&resolved) {
            return Err(io_err(format!(
                "a module URL with credentials in it is refused: {}",
                without_userinfo(&resolved)
            )));
        }
        Ok(resolved)
    }

    fn load(
        &self,
        module_specifier: &ModuleSpecifier,
        _maybe_referrer: Option<&ModuleLoadReferrer>,
        _options: ModuleLoadOptions,
    ) -> ModuleLoadResponse {
        let url = module_specifier.to_string();
        let proxy_url = self.proxy_url.clone();
        let ssrf = self.ssrf.clone();
        // ~keep Read before the future: the state is not `Send` and ops borrow it mutably. An
        // ~keep unreadable state refuses the module, so the block list cannot be skipped.
        let Ok(state) = self.op_state.try_borrow() else {
            return ModuleLoadResponse::Sync(Err(io_err(format!("Module {} refused: page state is busy", url))));
        };
        let origin_headers = state.origin_headers();
        let block_patterns = state.intercept_block_patterns.clone();
        let user_agent = state.user_agent.clone();
        drop(state);

        ModuleLoadResponse::Async(Pin::from(Box::new(async move {
            let parsed =
                ModuleSpecifier::parse(&url).map_err(|e| io_err(format!("Invalid module URL {}: {}", url, e)))?;
            ssrf.validate(&parsed)
                .await
                .map_err(|e| io_err(format!("Module {} blocked by SSRF policy: {}", url, e)))?;

            // ~keep Manual redirects: each hop is checked against the SSRF policy and gets the
            // ~keep scoped headers only on their host. reqwest drops only `Authorization` when a
            // ~keep redirect leaves the host, and a scoped header can have another name.
            let mut builder = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none());
            if let Some(ref proxy) = proxy_url {
                match reqwest::Proxy::all(proxy) {
                    Ok(p) => builder = builder.proxy(p),
                    Err(e) => {
                        return Err(io_err(format!("Invalid module proxy '{}': {}", proxy, e)));
                    }
                }
            }
            let client = with_policy_resolver(builder, proxy_url.is_some(), &ssrf)
                .build()
                .map_err(|e| io_err(format!("HTTP client error: {}", e)))?;

            tracing::debug!(
                "Loading ES module: {} (proxy: {})",
                url,
                proxy_url.as_deref().unwrap_or("direct")
            );

            let mut current = parsed;
            let mut redirects_followed = 0;
            let resp = loop {
                if matches_block_pattern(&block_patterns, current.as_str()) {
                    return Err(io_err(format!("Module {} blocked by interception", current)));
                }
                let mut request = client
                    .get(current.as_str())
                    .header("Accept", "application/javascript, text/javascript, */*");
                if let Some(user_agent) = &user_agent {
                    request = request.header(reqwest::header::USER_AGENT, user_agent.as_str());
                }
                for (name, value) in origin_headers.iter().flat_map(|scoped| scoped.headers_for(&current)) {
                    request = request.header(name.as_str(), value.as_str());
                }
                let resp = request
                    .send()
                    .await
                    .map_err(|e| io_err(format!("Failed to fetch module {}: {}", url, e)))?;
                let Some(next) = resp
                    .status()
                    .is_redirection()
                    .then(|| resp.headers().get(reqwest::header::LOCATION))
                    .flatten()
                    .and_then(|location| location.to_str().ok())
                    .and_then(|location| current.join(location).ok())
                else {
                    break resp;
                };
                let next = without_userinfo(&next);
                ssrf.validate(&next)
                    .await
                    .map_err(|e| io_err(format!("Module {} blocked by SSRF policy: {}", next, e)))?;
                redirects_followed += 1;
                if redirects_followed > MODULE_REDIRECT_LIMIT {
                    return Err(io_err(format!("Module {} redirected too many times", url)));
                }
                current = next;
            };

            if !resp.status().is_success() {
                return Err(io_err(format!("Module {} returned HTTP {}", url, resp.status())));
            }

            let code = resp
                .text()
                .await
                .map_err(|e| io_err(format!("Failed to read module body {}: {}", url, e)))?;

            let specifier =
                ModuleSpecifier::parse(&url).map_err(|e| io_err(format!("Invalid module URL {}: {}", url, e)))?;

            Ok(ModuleSource::new(
                deno_core::ModuleType::JavaScript,
                ModuleSourceCode::String(code.into()),
                &specifier,
                None,
            ))
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_import_with_userinfo_is_refused_without_it() {
        let loader = BrowserModuleLoader::new("http://example.com/");
        let error = loader
            .resolve(
                "http://user:s3cret@example.com/m.js",
                "http://example.com/",
                deno_core::ResolutionKind::DynamicImport,
            )
            .expect_err("a module URL with userinfo must be refused");
        let message = error.to_string();
        assert!(!message.contains("s3cret"), "{message}");
        assert!(message.contains("http://example.com/m.js"), "{message}");

        let resolved = loader
            .resolve("/m.js", "http://example.com/", deno_core::ResolutionKind::DynamicImport)
            .expect("an import without userinfo resolves");
        assert_eq!(resolved.as_str(), "http://example.com/m.js");
    }

    #[test]
    fn a_module_is_refused_while_the_page_state_cannot_be_read() {
        let loader = BrowserModuleLoader::new("http://example.com/");
        let held = loader.op_state.clone();
        let _busy = held.borrow_mut();
        let specifier = ModuleSpecifier::parse("http://example.com/m.js").expect("parse");
        let options = ModuleLoadOptions {
            is_dynamic_import: false,
            is_synchronous: false,
            requested_module_type: deno_core::RequestedModuleType::None,
        };

        let ModuleLoadResponse::Sync(result) = loader.load(&specifier, None, options) else {
            panic!("an unreadable page state must refuse the module before any fetch starts");
        };
        let Err(error) = result else {
            panic!("the module must be refused");
        };
        assert!(error.to_string().contains("http://example.com/m.js"), "{error}");
    }
}
