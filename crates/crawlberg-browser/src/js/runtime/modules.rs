//! ES-module loading and evaluation for module scripts, by address or inline.

use super::BrowserJsRuntime;

/// How long fetching a module graph may take, and then how long running it may take.
const MODULE_BUDGET: tokio::time::Duration = tokio::time::Duration::from_secs(10);

impl BrowserJsRuntime {
    /// Fetch the module at `url`, and every module it imports, through the module loader, then run it.
    pub async fn load_module(&mut self, url: &str) -> Result<(), String> {
        let specifier =
            deno_core::ModuleSpecifier::parse(url).map_err(|e| format!("Invalid module URL {}: {}", url, e))?;

        // ~keep The loader's fetch has no timeout of its own, and a stalled module server must not
        // ~keep hold the page past its other scripts.
        let module_id = tokio::time::timeout(MODULE_BUDGET, self.runtime.load_side_es_module(&specifier))
            .await
            .map_err(|_| format!("Module load timed out after {:?}", MODULE_BUDGET))?
            .map_err(|e| format!("Module load error: {}", e))?;

        let result = self.runtime.mod_evaluate(module_id);

        let timeout = tokio::time::timeout(
            MODULE_BUDGET,
            self.runtime.run_event_loop(deno_core::PollEventLoopOptions::default()),
        )
        .await;

        match timeout {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(format!("Module event loop error: {}", e)),
            Err(_) => {
                tracing::warn!("Module evaluation timed out after {:?}: {}", MODULE_BUDGET, url);
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

        let module_id = tokio::time::timeout(
            MODULE_BUDGET,
            self.runtime
                .load_side_es_module_from_code(&specifier, deno_core::ModuleCodeString::from(code.to_string())),
        )
        .await
        .map_err(|_| format!("Inline module load timed out after {:?}", MODULE_BUDGET))?
        .map_err(|e| format!("Inline module load error: {}", e))?;

        let result = self.runtime.mod_evaluate(module_id);

        let timeout = tokio::time::timeout(
            MODULE_BUDGET,
            self.runtime.run_event_loop(deno_core::PollEventLoopOptions::default()),
        )
        .await;

        match timeout {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(format!("Module event loop error: {}", e)),
            Err(_) => {
                tracing::warn!("Inline module timed out after {:?}", MODULE_BUDGET);
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
