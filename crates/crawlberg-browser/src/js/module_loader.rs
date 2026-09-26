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
                match reqwest::Proxy::all(proxy) {
                    Ok(p) => builder = builder.proxy(p),
                    Err(e) => {
                        // ~keep A proxy address that fails to parse cannot be redacted, so it
                        // must not be logged at all, not even unredacted.
                        return Err(io_err(format!("Invalid module proxy: {}", e)));
                    }
                }
            }
            let client = builder
                .build()
                .map_err(|e| io_err(format!("HTTP client error: {}", e)))?;

            // ~keep The proxy address can carry credentials, and there is no safe way to show
            // part of it without a redactor this crate cannot reach, so only whether one is
            // configured is logged, never the address itself.
            tracing::debug!(
                "Loading ES module: {} (proxy: {})",
                url,
                if proxy_url.is_some() { "configured" } else { "direct" }
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
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Level, Metadata};

    use super::*;

    const PROXY_PASSWORD: &str = "s3cr3t-proxy-pw";

    /// A proxy URL that `Url::parse` rejects, carrying userinfo so a leak of it into an error
    /// would be detectable. Same fixture as
    /// `crates/crawlberg/tests/test_proxy_bypass_is_logged.rs`.
    fn unparseable_proxy_url() -> String {
        format!("://operator:{PROXY_PASSWORD}@proxy.invalid:8080")
    }

    fn load_options() -> ModuleLoadOptions {
        ModuleLoadOptions {
            is_dynamic_import: true,
            is_synchronous: false,
            requested_module_type: deno_core::RequestedModuleType::None,
        }
    }

    async fn run_load(loader: &BrowserModuleLoader, url: &str) -> Result<ModuleSource, ModuleLoaderError> {
        let specifier = ModuleSpecifier::parse(url).expect("test URL must parse");
        let response = loader.load(&specifier, None, load_options());
        let ModuleLoadResponse::Async(fut) = response else {
            panic!("BrowserModuleLoader::load must return an async response");
        };
        tokio::time::timeout(Duration::from_secs(5), fut)
            .await
            .expect("module load must not hang")
    }

    /// Records every field of every captured event, formatted the way tracing dispatches
    /// `%value`, `?value`, and plain `Display`/`Debug` fields.
    struct FieldVisitor<'a>(&'a mut Vec<(String, String)>);

    impl Visit for FieldVisitor<'_> {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.0.push((field.name().to_owned(), format!("{value:?}")));
        }
    }

    /// Captured `DEBUG`-level events only.
    struct DebugEventSubscriber {
        sink: Arc<Mutex<Vec<(String, String)>>>,
    }

    impl tracing::Subscriber for DebugEventSubscriber {
        fn enabled(&self, metadata: &Metadata<'_>) -> bool {
            *metadata.level() == Level::DEBUG
        }

        fn new_span(&self, _attrs: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }

        fn record(&self, _span: &Id, _values: &Record<'_>) {}
        fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

        fn event(&self, event: &Event<'_>) {
            if *event.metadata().level() != Level::DEBUG {
                return;
            }
            let mut fields = self.sink.lock().expect("sink mutex must not be poisoned");
            event.record(&mut FieldVisitor(&mut fields));
        }

        fn enter(&self, _span: &Id) {}
        fn exit(&self, _span: &Id) {}
    }

    #[tokio::test(flavor = "current_thread")]
    async fn debug_log_does_not_carry_the_proxy_password() {
        // ~keep A well-formed proxy so the load reaches the debug log line; the port is closed
        // so the eventual fetch fails fast without any real network dependency.
        let loader = BrowserModuleLoader::with_proxy(
            "https://example.com/",
            Some(format!("http://operator:{PROXY_PASSWORD}@127.0.0.1:1")),
        );

        let sink: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let _guard = tracing::subscriber::set_default(DebugEventSubscriber { sink: sink.clone() });

        let _ = run_load(&loader, "https://example.com/mod.js").await;

        let recorded = sink.lock().expect("sink mutex must not be poisoned");
        assert!(
            recorded.iter().any(|(_, v)| v.contains("Loading ES module")),
            "expected the module-load debug line to fire (positive control), got {recorded:?}"
        );
        // ~keep Positive twin for the absence assertion below: the line must still say a proxy
        // is configured, so the test cannot pass merely because nothing about the proxy prints.
        assert!(
            recorded.iter().any(|(_, v)| v.contains("configured")),
            "expected the debug line to say a proxy is configured, got {recorded:?}"
        );
        assert!(
            recorded.iter().all(|(_, v)| !v.contains(PROXY_PASSWORD)),
            "the proxy password must never reach a log field, got {recorded:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invalid_proxy_error_does_not_carry_the_password() {
        let loader = BrowserModuleLoader::with_proxy("https://example.com/", Some(unparseable_proxy_url()));

        let err = run_load(&loader, "https://example.com/mod.js")
            .await
            .expect_err("an unparseable proxy must fail the module load");
        let message = err.to_string();

        assert!(
            message.contains("Invalid module proxy"),
            "expected the invalid-proxy branch, got '{message}'"
        );
        assert!(
            !message.contains(PROXY_PASSWORD),
            "the proxy password must never reach the error text, got '{message}'"
        );
    }
}
