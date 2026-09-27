use std::pin::Pin;
use std::sync::Arc;

use deno_core::ModuleLoadOptions;
use deno_core::ModuleLoadReferrer;
use deno_core::ModuleLoadResponse;
use deno_core::ModuleLoader;
use deno_core::ModuleSource;
use deno_core::ModuleSourceCode;
use deno_core::ModuleSpecifier;
use deno_core::error::ModuleLoaderError;

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
}

impl BrowserModuleLoader {
    pub fn new(base_url: &str) -> Self {
        Self::with_proxy(base_url, None)
    }

    pub fn with_proxy(base_url: &str, proxy_url: Option<String>) -> Self {
        Self::with_ssrf(base_url, proxy_url, Arc::new(DefaultSsrfValidator::from_env()))
    }

    pub fn with_ssrf(base_url: &str, proxy_url: Option<String>, ssrf: Arc<dyn SsrfValidator>) -> Self {
        BrowserModuleLoader {
            base_url: base_url.to_string(),
            proxy_url,
            ssrf,
        }
    }
}

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

        deno_core::resolve_import(specifier, base).map_err(|e| deno_error::JsErrorBox::generic(e.to_string()))
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

        ModuleLoadResponse::Async(Pin::from(Box::new(async move {
            let parsed =
                ModuleSpecifier::parse(&url).map_err(|e| io_err(format!("Invalid module URL {}: {}", url, e)))?;
            ssrf.validate(&parsed)
                .await
                .map_err(|e| io_err(format!("Module {} blocked by SSRF policy: {}", url, e)))?;

            let mut builder = reqwest::Client::builder();
            if let Some(ref proxy) = proxy_url {
                match crate::net::proxy::reqwest_proxy(proxy) {
                    Ok(p) => builder = builder.proxy(p),
                    Err(e) => {
                        return Err(io_err(format!("Invalid module proxy: {e}")));
                    }
                }
            }
            let client = builder
                .build()
                .map_err(|e| io_err(format!("HTTP client error: {}", e)))?;

            tracing::debug!(
                "Loading ES module: {} (proxy: {})",
                url,
                proxy_url.as_deref().unwrap_or("direct")
            );

            let resp = client
                .get(&url)
                .header("Accept", "application/javascript, text/javascript, */*")
                .send()
                .await
                .map_err(|e| io_err(format!("Failed to fetch module {}: {}", url, e)))?;

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

    async fn module_fetch_error(proxy: &str) -> String {
        let loader = BrowserModuleLoader::with_ssrf("http://127.0.0.1:1/", Some(proxy.to_string()), Arc::new(AllowAll));
        let specifier = ModuleSpecifier::parse("http://127.0.0.1:1/module.js").expect("valid specifier");
        let options = ModuleLoadOptions {
            is_dynamic_import: true,
            is_synchronous: false,
            requested_module_type: deno_core::RequestedModuleType::None,
        };
        let ModuleLoadResponse::Async(load) = loader.load(&specifier, None, options) else {
            panic!("the loader fetches over the network, so the load must be async");
        };
        match load.await {
            Ok(_) => panic!("{proxy} must refuse the module fetch"),
            Err(e) => e.to_string(),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_scheme_less_or_unparseable_proxy_refuses_the_module_fetch_without_echoing_it() {
        for proxy in crate::net::proxy::credential_urls::URLS {
            crate::net::proxy::credential_urls::assert_not_shown(proxy, &module_fetch_error(proxy).await);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_proxy_scheme_reqwest_would_drop_refuses_the_module_fetch() {
        let message = module_fetch_error("ftp://operator:s3cr3t@127.0.0.1:1").await;
        assert!(
            message.contains("'ftp'"),
            "the error must name the scheme, got {message}"
        );
        assert!(
            !message.contains("s3cr3t"),
            "the error leaked the proxy password: {message}"
        );
    }
}
