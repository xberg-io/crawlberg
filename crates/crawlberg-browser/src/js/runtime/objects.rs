//! The `__crawlberg_objects` remote-object store: CDP-style object ids, argument
//! marshalling, and `this` resolution for stored references.

use super::{BrowserJsRuntime, RemoteObjectInfo};

impl BrowserJsRuntime {
    pub fn store_object(&mut self, js_expression: &str) -> Result<String, String> {
        self.object_counter += 1;
        let oid = self.make_oid(self.object_counter);
        let code = format!("globalThis.__crawlberg_objects['{}'] = ({});", oid, js_expression,);
        self.runtime
            .execute_script("<store>", code)
            .map_err(|e| format!("Store error: {}", e))?;
        self.object_store
            .insert(oid.clone(), format!("globalThis.__crawlberg_objects['{}']", oid));
        Ok(oid)
    }

    pub fn store_object_with_meta(&mut self, js_expression: &str) -> Result<RemoteObjectInfo, String> {
        self.object_counter += 1;
        let oid = self.make_oid(self.object_counter);
        let code = format!(
            "(function() {{\n\
                var __result = ({expr});\n\
                globalThis.__crawlberg_objects['{oid}'] = __result;\n\
                return {meta_fn};\n\
            }})()",
            expr = js_expression,
            oid = oid,
            meta_fn = Self::meta_extract_js("__result"),
        );
        let result = self
            .runtime
            .execute_script("<store-meta>", code)
            .map_err(|e| format!("Store error: {}", e))?;
        let meta_str = self.v8_to_json(result)?;
        let meta_json = if let serde_json::Value::String(s) = &meta_str {
            serde_json::from_str(s).unwrap_or(meta_str.clone())
        } else {
            meta_str
        };
        self.object_store
            .insert(oid.clone(), format!("globalThis.__crawlberg_objects['{}']", oid));
        Ok(Self::info_from_meta(&meta_json, Some(oid)))
    }

    pub fn release_object(&mut self, object_id: &str) {
        if self.object_store.remove(object_id).is_some() {
            let code = format!("delete globalThis.__crawlberg_objects['{}'];", object_id,);
            let _ = self.runtime.execute_script("<release>", code);
        }
    }

    pub fn release_object_group(&mut self) {
        let _ = self
            .runtime
            .execute_script("<releaseGroup>", "globalThis.__crawlberg_objects = {};".to_string());
        self.object_store.clear();
    }

    pub(super) fn make_oid(&self, counter: u64) -> String {
        format!("{{\"injectedScriptId\":1,\"id\":{}}}", counter)
    }

    pub(super) fn resolve_this(&self, object_id: Option<&str>) -> String {
        match object_id {
            Some(oid) => {
                if let Some(retrieval) = self.object_store.get(oid) {
                    retrieval.clone()
                } else if oid.starts_with("node-") {
                    let nid = oid.strip_prefix("node-").unwrap_or("0");
                    format!(
                        "(function() {{ \
                            var nid = {}; \
                            var cache = globalThis._cache || new Map(); \
                            if (cache.has(nid)) return cache.get(nid); \
                            return null; \
                        }})()",
                        nid
                    )
                } else {
                    "globalThis".to_string()
                }
            }
            None => "globalThis".to_string(),
        }
    }

    pub(super) fn build_args(&self, arguments: &[serde_json::Value]) -> (String, String) {
        let mut setup_lines = Vec::new();
        let mut arg_names = Vec::new();

        for (i, arg) in arguments.iter().enumerate() {
            let arg_name = format!("__arg{}", i);
            if let Some(value) = arg.get("value") {
                let json_str = serde_json::to_string(value).unwrap_or_else(|_| "undefined".to_string());
                setup_lines.push(format!("var {} = {};", arg_name, json_str));
            } else if let Some(oid) = arg.get("objectId").and_then(|v| v.as_str()) {
                if let Some(retrieval) = self.object_store.get(oid) {
                    setup_lines.push(format!("var {} = {};", arg_name, retrieval));
                } else {
                    setup_lines.push(format!("var {} = undefined;", arg_name));
                }
            } else if let Some(raw) = arg.get("unserializableValue").and_then(|v| v.as_str()) {
                setup_lines.push(format!("var {} = {};", arg_name, raw));
            } else {
                setup_lines.push(format!("var {} = undefined;", arg_name));
            }
            arg_names.push(arg_name);
        }

        (setup_lines.join("\n"), arg_names.join(", "))
    }
}
