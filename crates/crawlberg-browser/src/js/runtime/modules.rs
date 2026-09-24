//! ES-module loading and evaluation for dynamic `import()` and inline module scripts.

use super::BrowserJsRuntime;

impl BrowserJsRuntime {
    pub async fn load_module(&mut self, url: &str) -> Result<(), String> {
        let specifier =
            deno_core::ModuleSpecifier::parse(url).map_err(|e| format!("Invalid module URL {}: {}", url, e))?;

        let module_id = self
            .runtime
            .load_side_es_module_from_code(&specifier, deno_core::ModuleCodeString::from_static(""))
            .await
            .map_err(|e| format!("Module load error: {}", e))?;

        let result = self.runtime.mod_evaluate(module_id);

        let timeout = tokio::time::timeout(
            tokio::time::Duration::from_secs(10),
            self.runtime.run_event_loop(deno_core::PollEventLoopOptions::default()),
        )
        .await;

        match timeout {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(format!("Module event loop error: {}", e)),
            Err(_) => {
                tracing::warn!("Module evaluation timed out after 10s: {}", url);
                return Ok(());
            }
        }

        match result.await {
            Ok(()) => Ok(()),
            Err(e) => {
                tracing::warn!("Module eval error: {}", e);
                Ok(())
            }
        }
    }

    pub async fn load_inline_module(&mut self, code: &str, base_url: &str) -> Result<(), String> {
        let specifier =
            deno_core::ModuleSpecifier::parse(&format!("{}#inline-module-{}", base_url, self.object_counter))
                .unwrap_or_else(|_| deno_core::ModuleSpecifier::parse("about:blank").unwrap());

        self.object_counter += 1;

        let module_id = self
            .runtime
            .load_side_es_module_from_code(&specifier, deno_core::ModuleCodeString::from(code.to_string()))
            .await
            .map_err(|e| format!("Inline module load error: {}", e))?;

        let result = self.runtime.mod_evaluate(module_id);

        let timeout = tokio::time::timeout(
            tokio::time::Duration::from_secs(10),
            self.runtime.run_event_loop(deno_core::PollEventLoopOptions::default()),
        )
        .await;

        match timeout {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(format!("Module event loop error: {}", e)),
            Err(_) => {
                tracing::warn!("Inline module timed out after 10s");
                return Ok(());
            }
        }

        match result.await {
            Ok(()) => Ok(()),
            Err(e) => {
                tracing::warn!("Inline module eval error: {}", e);
                Ok(())
            }
        }
    }
}
