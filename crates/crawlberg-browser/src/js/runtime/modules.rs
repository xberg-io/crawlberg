//! ES-module loading and evaluation for module scripts, by address or inline.

use std::task::Poll;

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

        self.evaluate_module(module_id, url).await
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

        self.evaluate_module(module_id, "inline module").await
    }

    /// Run a loaded module and drive the event loop until the module's own evaluation settles.
    async fn evaluate_module(&mut self, module_id: deno_core::ModuleId, name: &str) -> Result<(), String> {
        let evaluation = self.runtime.mod_evaluate(module_id);
        let waited = self.run_until_settled(evaluation, MODULE_BUDGET, name).await;
        let options = deno_core::PollEventLoopOptions::default();

        match waited {
            // ~keep deno_core reports a module that throws as an unhandled rejection on the event loop, not
            // ~keep through its evaluation. One more pass collects it here, so the module is not taken
            // ~keep for a success and the rejection does not fail the next module's wait.
            Ok(Ok(())) => std::future::poll_fn(|cx| match self.runtime.poll_event_loop(cx, options) {
                Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
                _ => Poll::Ready(Ok(())),
            })
            .await
            .map_err(|e| format!("Module evaluation error: {}", e)),
            Ok(Err(e)) => Err(format!("Module evaluation error: {}", e)),
            Err(_) => {
                tracing::warn!("Module evaluation timed out after {:?}: {}", MODULE_BUDGET, name);
                Ok(())
            }
        }
    }
}
