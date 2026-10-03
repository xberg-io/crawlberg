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
use crate::net::error_with_causes;
use crate::net::interceptor::matches_block_pattern;
use crate::net::proxy::UpstreamProxy;
use crate::net::resolver::{EnvironmentSystemProxySelector, SystemProxySelector, reqwest_builder_and_route_for_url};
use crate::net::ssrf::{DefaultSsrfValidator, SsrfValidator};

pub struct BrowserModuleLoader {
    pub base_url: String,
    /// Proxy threaded through to every dynamic ES-module fetch (#139).
    /// `None` keeps the pre-#139 direct-connection behaviour for callers
    /// that haven't been updated.
    pub proxy: Option<UpstreamProxy>,
    /// SSRF policy applied to every dynamic `import()`. Page JS chooses these URLs,
    /// so they are as untrusted as any other page-initiated fetch.
    pub ssrf: Arc<dyn SsrfValidator>,
    system_proxy_selector: Arc<dyn SystemProxySelector>,
    /// The page state the runtime shares with its ops. A module on the scoped host gets the
    /// page client's [`OriginHeaders`](crate::net::OriginHeaders), as a `fetch()` does.
    pub op_state: SharedState,
}

impl BrowserModuleLoader {
    pub fn new(base_url: &str) -> Self {
        Self::with_proxy(base_url, None)
    }

    pub fn with_proxy(base_url: &str, proxy: Option<UpstreamProxy>) -> Self {
        Self::with_ssrf(
            base_url,
            proxy,
            Arc::new(DefaultSsrfValidator::from_env()),
            Rc::new(RefCell::new(JsOpState::new())),
        )
    }

    pub fn with_ssrf(
        base_url: &str,
        proxy: Option<UpstreamProxy>,
        ssrf: Arc<dyn SsrfValidator>,
        op_state: SharedState,
    ) -> Self {
        BrowserModuleLoader {
            base_url: base_url.to_string(),
            proxy,
            ssrf,
            system_proxy_selector: Arc::new(EnvironmentSystemProxySelector),
            op_state,
        }
    }

    #[cfg(test)]
    fn with_ssrf_and_proxy_selector(
        base_url: &str,
        ssrf: Arc<dyn SsrfValidator>,
        op_state: SharedState,
        system_proxy_selector: Arc<dyn SystemProxySelector>,
    ) -> Self {
        Self {
            base_url: base_url.to_owned(),
            proxy: None,
            ssrf,
            system_proxy_selector,
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
        let proxy = self.proxy.clone();
        let ssrf = self.ssrf.clone();
        let system_proxy_selector = self.system_proxy_selector.clone();
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

            let mut current = parsed;
            let mut redirects_followed = 0;
            let resp = loop {
                if matches_block_pattern(&block_patterns, current.as_str()) {
                    return Err(io_err(format!("Module {} blocked by interception", current)));
                }
                // ~keep Build per hop: a redirect can cross a `NO_PROXY` boundary, and the
                // resolver must follow the selected route for that hop.
                let builder = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none());
                let (builder, route) = reqwest_builder_and_route_for_url(
                    builder,
                    &current,
                    proxy.as_ref(),
                    system_proxy_selector.as_ref(),
                    &ssrf,
                )
                .map_err(|e| io_err(format!("Invalid module proxy: {e}")))?;
                tracing::debug!(url = %current, proxy_route = route.as_str(), "loading ES module");
                let client = builder.build().map_err(|e| io_err(format!("HTTP client error: {e}")))?;
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
                    .map_err(|e| io_err(format!("Failed to fetch module {}: {}", url, error_with_causes(&e))))?;
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

    #[derive(Debug)]
    struct AllowAll;

    #[async_trait::async_trait]
    impl SsrfValidator for AllowAll {
        async fn validate(&self, _url: &url::Url) -> Result<(), String> {
            Ok(())
        }
    }

    async fn module_fetch(proxy: UpstreamProxy, specifier: &str) -> Result<String, String> {
        let loader = BrowserModuleLoader::with_ssrf(
            "http://origin.test/",
            Some(proxy),
            Arc::new(AllowAll),
            Rc::new(RefCell::new(JsOpState::new())),
        );
        load_module(&loader, specifier).await
    }

    async fn load_module(loader: &BrowserModuleLoader, specifier: &str) -> Result<String, String> {
        let specifier = ModuleSpecifier::parse(specifier).expect("valid specifier");
        let options = ModuleLoadOptions {
            is_dynamic_import: true,
            is_synchronous: false,
            requested_module_type: deno_core::RequestedModuleType::None,
        };
        let ModuleLoadResponse::Async(load) = loader.load(&specifier, None, options) else {
            panic!("the loader fetches over the network, so the load must be async");
        };
        match tokio::time::timeout(std::time::Duration::from_secs(10), load)
            .await
            .expect("the module fetch must finish")
        {
            Ok(source) => match source.code {
                ModuleSourceCode::String(code) => Ok(code.as_str().to_string()),
                ModuleSourceCode::Bytes(_) => Err("the loader returns module text".to_string()),
            },
            Err(e) => Err(e.to_string()),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_credentialed_proxy_carries_the_module_fetch_with_its_credentials() {
        use crate::net::proxy::credentialed_proxy;
        let (proxy, requests) = credentialed_proxy::start().await;

        let source = module_fetch(proxy, "http://origin.test/module.js")
            .await
            .expect("the proxy accepts the credentials, so the module must load");

        assert!(
            source.contains("via-proxy"),
            "the module must come from the proxy: {source}"
        );
        credentialed_proxy::assert_one_authenticated_request(&requests, "http://origin.test/module.js");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_proxy_that_refuses_the_credentials_fails_the_module_fetch_without_showing_them() {
        use crate::net::proxy::credentialed_proxy;
        let (proxy, requests) = credentialed_proxy::start().await;
        let direct = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let target = format!("http://{}/module.js", direct.local_addr().expect("addr"));

        let message = module_fetch(credentialed_proxy::with_wrong_password(&proxy), &target)
            .await
            .expect_err("a refused proxy must fail the module fetch");

        assert!(
            message.contains("407"),
            "the error must name the proxy's answer: {message}"
        );
        assert!(!message.contains(credentialed_proxy::PASSWORD), "{message}");
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(200), direct.accept())
                .await
                .is_err(),
            "the module fetch connected directly"
        );
        let requests = requests.lock().expect("lock");
        assert_eq!(requests.len(), 1, "the module fetch must go to the proxy: {requests:?}");
        assert!(credentialed_proxy::proxy_authorization(&requests[0]).is_some());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_module_redirect_reselects_the_system_proxy_and_can_cross_to_direct() {
        use crate::net::resolver::tests::{TestProxySelector, denied_server};

        let (target_port, target_requests) =
            denied_server("HTTP/1.1 200 OK\r\nContent-Length: 17\r\nConnection: close\r\n\r\nexport default 1;").await;
        let redirect = format!(
            "HTTP/1.1 302 Found\r\nLocation: http://localhost:{target_port}/module.js\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let redirect: &'static str = Box::leak(redirect.into_boxed_str());
        let (proxy_port, proxy_requests) = denied_server(redirect).await;
        let selector = Arc::new(TestProxySelector::default());
        selector.set_proxy(&format!("http://localhost:{proxy_port}"));
        selector.direct_host("localhost");
        let loader = BrowserModuleLoader::with_ssrf_and_proxy_selector(
            "http://origin.test/",
            Arc::new(AllowAll),
            Rc::new(RefCell::new(JsOpState::new())),
            selector,
        );

        let source = load_module(&loader, "http://origin.test/module.js")
            .await
            .expect("both module hops must succeed");

        assert_eq!(source, "export default 1;");
        assert_eq!(proxy_requests.lock().expect("lock").len(), 1);
        assert_eq!(target_requests.lock().expect("lock").len(), 1);
    }

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

    #[tokio::test(flavor = "current_thread")]
    async fn a_module_refused_at_connect_time_names_the_policy_reason() {
        use crate::net::resolver::tests::{RebindingPolicy, TestProxySelector, denied_server};

        let (port, seen) = denied_server("HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
        let loader = BrowserModuleLoader::with_ssrf_and_proxy_selector(
            "http://example.com/",
            Arc::new(RebindingPolicy::default()),
            Rc::new(RefCell::new(JsOpState::new())),
            Arc::new(TestProxySelector::default()),
        );
        let specifier = ModuleSpecifier::parse(&format!("http://localhost:{port}/m.js")).expect("parse");
        let options = ModuleLoadOptions {
            is_dynamic_import: false,
            is_synchronous: false,
            requested_module_type: deno_core::RequestedModuleType::None,
        };

        let ModuleLoadResponse::Async(load) = loader.load(&specifier, None, options) else {
            panic!("a module load fetches asynchronously");
        };
        let Err(error) = load.await else {
            panic!("the connection's lookup answers a denied address");
        };

        assert!(
            error.to_string().contains("denied by the test policy: 127.0.0.1"),
            "the refusal must carry the policy's reason: {error}"
        );
        assert!(
            seen.lock().expect("lock").is_empty(),
            "the denied address must receive no connection"
        );
    }
}
