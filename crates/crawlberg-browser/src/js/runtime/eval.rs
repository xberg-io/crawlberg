//! Expression evaluation and the CDP `Runtime.evaluate`/`Runtime.callFunctionOn` surface:
//! wraps bare expressions, enforces the wall-clock watchdog on synchronous eval, and
//! builds/reads back the remote-object metadata CDP clients expect.

use super::{BrowserJsRuntime, RemoteObjectInfo};

impl BrowserJsRuntime {
    pub fn evaluate(&mut self, expression: &str) -> Result<serde_json::Value, String> {
        let wrapped = Self::wrap_expression(expression);
        let result = self
            .runtime
            .execute_script("<eval>", wrapped)
            .map_err(|e| format!("JS error: {}", e))?;
        self.v8_to_json(result)
    }

    /// Evaluate `expression` under a wall-clock bound enforced from a companion OS thread.
    ///
    /// `JsRuntime::execute_script` is a synchronous, non-yielding call into V8: an
    /// `async fn` wrapping it in `tokio::time::timeout` can never observe the deadline,
    /// because the executor only gets to check a timer between `Poll::Pending` yields and
    /// this call produces none (#60). The only supported way to reclaim a wedged isolate is
    /// [`deno_core::v8::IsolateHandle::terminate_execution`], called from a thread other than
    /// the one running the script. This spawns exactly one such watchdog thread, joins it
    /// before returning, and clears the termination flag via `cancel_terminate_execution` so
    /// the isolate is left usable for the caller's next call — mirroring the proven recovery
    /// in [`Self::execute_script_with_timeout`] and its `execute_script_guarded_kills_small_infinite_loop`
    /// regression test.
    pub fn evaluate_with_timeout(
        &mut self,
        expression: &str,
        timeout: std::time::Duration,
    ) -> Result<serde_json::Value, String> {
        if timeout.is_zero() {
            return self.evaluate(expression);
        }

        let wrapped = Self::wrap_expression(expression);
        let isolate_handle = self.runtime.v8_isolate().thread_safe_handle();

        let pair = std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let pair_clone = pair.clone();

        let watchdog = std::thread::spawn(move || {
            let (lock, cvar) = &*pair_clone;
            let mut cancelled = lock.lock().unwrap();
            let deadline = std::time::Instant::now() + timeout;

            loop {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    isolate_handle.terminate_execution();
                    return;
                }

                let result = cvar.wait_timeout(cancelled, remaining).unwrap();
                cancelled = result.0;
                if *cancelled {
                    return;
                }
            }
        });

        let result = self.runtime.execute_script("<eval>", wrapped);

        {
            let (lock, cvar) = &*pair;
            let mut cancelled = lock.lock().unwrap();
            *cancelled = true;
            cvar.notify_one();
        }
        let _ = watchdog.join();

        self.runtime.v8_isolate().cancel_terminate_execution();

        match result {
            Ok(value) => self.v8_to_json(value),
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("Uncaught Error: execution terminated") {
                    tracing::warn!("ExecuteJs script killed after {}s timeout", timeout.as_secs());
                    Err(format!(
                        "script execution exceeded the {}s limit and was terminated",
                        timeout.as_secs()
                    ))
                } else {
                    Err(format!("JS error: {}", msg))
                }
            }
        }
    }

    /// Build the `<eval-remote>` script for [`Self::evaluate_for_cdp`]: on the awaited path the
    /// result (or thrown error) is captured into `__crawlberg_objects` and its metadata computed
    /// once the promise settles; on the synchronous path the metadata is returned directly.
    fn build_eval_remote_code(cleaned_expr: &str, oid: &str, await_promise: bool) -> String {
        if await_promise {
            format!(
                "(async function() {{\n\
                    try {{\n\
                        var __result = await ({expr});\n\
                        globalThis.__crawlberg_objects['{oid}'] = __result;\n\
                        globalThis.__crawlberg_await_meta = {meta_fn};\n\
                        globalThis.__crawlberg_await_rejected = false;\n\
                    }} catch(e) {{\n\
                        globalThis.__crawlberg_objects['{oid}'] = e;\n\
                        globalThis.__crawlberg_await_meta = {err_meta_fn};\n\
                        globalThis.__crawlberg_await_rejected = true;\n\
                    }}\n\
                }})()",
                expr = cleaned_expr,
                oid = oid,
                meta_fn = Self::meta_extract_js("__result"),
                err_meta_fn = Self::meta_extract_js("e"),
            )
        } else {
            format!(
                "(function() {{\n\
                    var __result;\n\
                    try {{ __result = ({expr}); }} catch(e) {{ __result = undefined; }}\n\
                    globalThis.__crawlberg_objects['{oid}'] = __result;\n\
                    return {meta_fn};\n\
                }})()",
                expr = cleaned_expr,
                oid = oid,
                meta_fn = Self::meta_extract_js("__result"),
            )
        }
    }

    /// Resolve pending promises after an awaited `<eval-remote>` run and read back its
    /// metadata, surfacing a rejection as an `Err` the same way CDP callers expect.
    async fn read_awaited_meta(&mut self, oid: &str) -> Result<deno_core::v8::Global<deno_core::v8::Value>, String> {
        self.resolve_promises().await;
        let rejected = self
            .runtime
            .execute_script("<readRejected>", "globalThis.__crawlberg_await_rejected".to_string())
            .map_err(|e| format!("JS error: {}", e))?;
        if self.v8_to_json(rejected)?.as_bool().unwrap_or(false) {
            let err = self.runtime.execute_script("<readError>", format!("String(globalThis.__crawlberg_objects['{0}'] && (globalThis.__crawlberg_objects['{0}'].message || globalThis.__crawlberg_objects['{0}']))", oid))
                .map_err(|e| format!("JS error: {}", e))?;
            return Err(format!(
                "Promise rejected: {}",
                self.v8_to_json(err)?.as_str().unwrap_or("")
            ));
        }
        self.runtime
            .execute_script("<readMeta>", "globalThis.__crawlberg_await_meta".to_string())
            .map_err(|e| format!("JS error: {}", e))
    }

    pub async fn evaluate_for_cdp(
        &mut self,
        expression: &str,
        return_by_value: bool,
        await_promise: bool,
    ) -> Result<RemoteObjectInfo, String> {
        if !await_promise && return_by_value {
            let val = self.evaluate(expression)?;
            return Ok(Self::info_from_json(&val));
        }

        self.object_counter += 1;
        let oid = self.make_oid(self.object_counter);

        let cleaned_expr = expression
            .trim()
            .trim_end_matches(|c: char| c == ';' || c.is_whitespace());

        let meta_code = Self::build_eval_remote_code(cleaned_expr, &oid, await_promise);

        let result = self
            .runtime
            .execute_script("<eval-remote>", meta_code)
            .map_err(|e| format!("JS error: {}", e))?;

        let meta_str = if await_promise {
            self.read_awaited_meta(&oid).await?
        } else {
            result
        };
        let meta_str = self.v8_to_json(meta_str)?;
        let meta_json = if let serde_json::Value::String(s) = &meta_str {
            serde_json::from_str(s).unwrap_or(meta_str)
        } else {
            meta_str
        };
        self.object_store
            .insert(oid.clone(), format!("globalThis.__crawlberg_objects['{}']", oid));

        if await_promise && return_by_value {
            let read = self
                .runtime
                .execute_script("<readResult>", format!("globalThis.__crawlberg_objects['{}']", oid))
                .map_err(|e| format!("JS error: {}", e))?;
            let json_val = self.v8_to_json(read)?;
            return Ok(Self::info_from_json(&json_val));
        }

        Ok(Self::info_from_meta(&meta_json, Some(oid)))
    }

    /// Awaited branch of [`Self::call_function_on_for_cdp`]: run the call inside an async IIFE,
    /// drain the event loop, then read back either the resolved value or its remote-object meta.
    async fn call_fn_awaited(
        &mut self,
        setup: &str,
        fn_decl: &str,
        this_expr: &str,
        args_list: &str,
        oid: &str,
        return_by_value: bool,
    ) -> Result<RemoteObjectInfo, String> {
        let code = format!(
            "(async function() {{\n\
                {setup}\n\
                var __fn = ({fn_decl});\n\
                var __this = ({this_expr});\n\
                var __result = await __fn.call(__this, {args});\n\
                globalThis.__crawlberg_objects['{oid}'] = __result;\n\
                globalThis.__crawlberg_await_meta = {meta_fn};\n\
            }})()",
            setup = setup,
            fn_decl = fn_decl,
            this_expr = this_expr,
            args = args_list,
            oid = oid,
            meta_fn = Self::meta_extract_js("__result"),
        );

        self.runtime
            .execute_script("<callFnAsync>", code)
            .map_err(|e| format!("JS error: {}", e))?;

        self.resolve_promises().await;

        if return_by_value {
            let read = self
                .runtime
                .execute_script("<readResult>", format!("globalThis.__crawlberg_objects['{}']", oid))
                .map_err(|e| format!("JS error: {}", e))?;
            let json_val = self.v8_to_json(read)?;
            return Ok(Self::info_from_json(&json_val));
        }

        let meta_result = self
            .runtime
            .execute_script("<readMeta>", "globalThis.__crawlberg_await_meta".to_string())
            .map_err(|e| format!("JS error: {}", e))?;
        let meta_str = self.v8_to_json(meta_result)?;
        let meta_json = if let serde_json::Value::String(s) = &meta_str {
            serde_json::from_str(s).unwrap_or(meta_str.clone())
        } else {
            meta_str
        };
        self.object_store
            .insert(oid.to_string(), format!("globalThis.__crawlberg_objects['{}']", oid));
        Ok(Self::info_from_meta(&meta_json, Some(oid.to_string())))
    }

    /// Synchronous, by-value branch of [`Self::call_function_on_for_cdp`].
    fn call_fn_by_value(
        &mut self,
        setup: &str,
        fn_decl: &str,
        this_expr: &str,
        args_list: &str,
    ) -> Result<RemoteObjectInfo, String> {
        let code = format!(
            "(function() {{\n\
                {setup}\n\
                var __fn = ({fn_decl});\n\
                var __this = ({this_expr});\n\
                return __fn.call(__this, {args});\n\
            }})()",
            setup = setup,
            fn_decl = fn_decl,
            this_expr = this_expr,
            args = args_list,
        );
        let result = self
            .runtime
            .execute_script("<callFnByValue>", code)
            .map_err(|e| format!("JS error: {}", e))?;
        let json_val = self.v8_to_json(result)?;
        Ok(Self::info_from_json(&json_val))
    }

    /// Synchronous, by-reference branch of [`Self::call_function_on_for_cdp`]: stores the
    /// result under `oid` and returns its remote-object meta.
    fn call_fn_remote(
        &mut self,
        setup: &str,
        fn_decl: &str,
        this_expr: &str,
        args_list: &str,
        oid: &str,
    ) -> Result<RemoteObjectInfo, String> {
        let code = format!(
            "(function() {{\n\
                {setup}\n\
                var __fn = ({fn_decl});\n\
                var __this = ({this_expr});\n\
                var __result = __fn.call(__this, {args});\n\
                globalThis.__crawlberg_objects['{oid}'] = __result;\n\
                return {meta_fn};\n\
            }})()",
            setup = setup,
            fn_decl = fn_decl,
            this_expr = this_expr,
            args = args_list,
            oid = oid,
            meta_fn = Self::meta_extract_js("__result"),
        );
        let result = self
            .runtime
            .execute_script("<callFnRemote>", code)
            .map_err(|e| format!("JS error: {}", e))?;
        let meta_str = self.v8_to_json(result)?;
        let meta_json = if let serde_json::Value::String(s) = &meta_str {
            serde_json::from_str(s).unwrap_or(meta_str.clone())
        } else {
            meta_str
        };
        self.object_store
            .insert(oid.to_string(), format!("globalThis.__crawlberg_objects['{}']", oid));
        Ok(Self::info_from_meta(&meta_json, Some(oid.to_string())))
    }

    pub async fn call_function_on_for_cdp(
        &mut self,
        function_declaration: &str,
        object_id: Option<&str>,
        arguments: &[serde_json::Value],
        return_by_value: bool,
        await_promise: bool,
    ) -> Result<RemoteObjectInfo, String> {
        let this_expr = self.resolve_this(object_id);
        let (setup, args_list) = self.build_args(arguments);

        self.object_counter += 1;
        let oid = self.make_oid(self.object_counter);

        if await_promise {
            return self
                .call_fn_awaited(
                    &setup,
                    function_declaration,
                    &this_expr,
                    &args_list,
                    &oid,
                    return_by_value,
                )
                .await;
        }

        if return_by_value {
            return self.call_fn_by_value(&setup, function_declaration, &this_expr, &args_list);
        }

        self.call_fn_remote(&setup, function_declaration, &this_expr, &args_list, &oid)
    }

    pub async fn call_function_on(
        &mut self,
        function_declaration: &str,
        object_id: Option<&str>,
        arguments: &[serde_json::Value],
        return_by_value: bool,
    ) -> Result<RemoteObjectInfo, String> {
        self.call_function_on_for_cdp(function_declaration, object_id, arguments, return_by_value, false)
            .await
    }
}
