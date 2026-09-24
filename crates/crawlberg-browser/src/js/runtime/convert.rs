//! V8-value <-> JSON conversion helpers shared by the evaluation and object-store surfaces.

use super::{BrowserJsRuntime, RemoteObjectInfo};

impl BrowserJsRuntime {
    pub(super) fn wrap_expression(expression: &str) -> String {
        let trimmed = expression.trim();

        let is_multi_statement = trimmed.starts_with("var ")
            || trimmed.starts_with("let ")
            || trimmed.starts_with("const ")
            || trimmed.starts_with("if ")
            || trimmed.starts_with("for ")
            || trimmed.starts_with("while ")
            || trimmed.starts_with("return ");

        if is_multi_statement {
            format!("(function() {{ {} }})()", expression)
        } else {
            let cleaned = trimmed.trim_end_matches(|c: char| c == ';' || c.is_whitespace());
            format!("(function() {{ return ({}); }})()", cleaned)
        }
    }

    pub(super) fn meta_extract_js(var_name: &str) -> String {
        format!(
            r#"(function(v) {{
                var t = typeof v;
                var st = null, cn = '', desc = '';
                if (v === null) {{ t = 'object'; st = 'null'; }}
                else if (v === undefined) {{ t = 'undefined'; }}
                else if (Array.isArray(v)) {{
                    st = 'array'; cn = 'Array';
                    desc = 'Array(' + v.length + ')';
                }}
                else if (t === 'object' && typeof v._nid === 'number') {{
                    st = 'node';
                    cn = v.constructor ? v.constructor.name : 'Node';
                    if (v.nodeType === 9) cn = 'HTMLDocument';
                    else if (v.nodeType === 1) cn = 'HTML' + (v.tagName || 'Element').charAt(0) + (v.tagName || 'Element').slice(1).toLowerCase() + 'Element';
                    desc = v.tagName ? v.tagName.toLowerCase() : (v.nodeName || 'node');
                }}
                else if (t === 'function') {{
                    cn = 'Function';
                    desc = v.name ? 'function ' + v.name + '()' : 'function()';
                }}
                else if (t === 'object') {{
                    cn = (v.constructor && v.constructor.name) || 'Object';
                    desc = cn;
                }}
                else {{ desc = String(v); }}
                return JSON.stringify({{type:t,subtype:st,className:cn,description:desc}});
            }})({var_name})"#,
            var_name = var_name,
        )
    }

    pub(super) fn v8_to_json(
        &mut self,
        result: deno_core::v8::Global<deno_core::v8::Value>,
    ) -> Result<serde_json::Value, String> {
        use deno_core::v8;
        let context = self.runtime.main_context();
        let isolate = &mut *self.runtime.v8_isolate();
        v8::scope_with_context!(scope, isolate, context);
        let local = v8::Local::new(scope, result);

        if local.is_undefined() || local.is_null() {
            return Ok(serde_json::Value::Null);
        }
        if local.is_boolean() {
            return Ok(serde_json::Value::Bool(local.boolean_value(scope)));
        }
        if local.is_number() {
            let n = local.number_value(scope).unwrap_or(0.0);
            return Ok(serde_json::json!(n));
        }
        if local.is_string() {
            let s = local.to_rust_string_lossy(scope);
            return Ok(serde_json::Value::String(s));
        }

        let global = scope.get_current_context().global(scope);
        let json_obj_str = deno_core::v8::String::new(scope, "JSON").unwrap();
        if let Some(json_obj) = global.get(scope, json_obj_str.into())
            && let Some(json_obj) = json_obj.to_object(scope)
        {
            let stringify_str = deno_core::v8::String::new(scope, "stringify").unwrap();
            if let Some(stringify_fn) = json_obj.get(scope, stringify_str.into())
                && let Ok(stringify_fn) = deno_core::v8::Local::<deno_core::v8::Function>::try_from(stringify_fn)
            {
                let args = [local];
                if let Some(result) = stringify_fn.call(scope, json_obj.into(), &args)
                    && let Ok(val) = serde_json::from_str(&result.to_rust_string_lossy(scope))
                {
                    return Ok(val);
                }
            }
        }

        let s = local.to_rust_string_lossy(scope);
        Ok(serde_json::Value::String(s))
    }

    pub(super) fn info_from_json(value: &serde_json::Value) -> RemoteObjectInfo {
        match value {
            serde_json::Value::Null => RemoteObjectInfo {
                js_type: "object".into(),
                subtype: Some("null".into()),
                class_name: String::new(),
                description: "null".into(),
                object_id: None,
                value: Some(serde_json::Value::Null),
            },
            serde_json::Value::Bool(b) => RemoteObjectInfo {
                js_type: "boolean".into(),
                subtype: None,
                class_name: String::new(),
                description: b.to_string(),
                object_id: None,
                value: Some(value.clone()),
            },
            serde_json::Value::Number(n) => RemoteObjectInfo {
                js_type: "number".into(),
                subtype: None,
                class_name: String::new(),
                description: n.to_string(),
                object_id: None,
                value: Some(value.clone()),
            },
            serde_json::Value::String(s) => RemoteObjectInfo {
                js_type: "string".into(),
                subtype: None,
                class_name: String::new(),
                description: s.clone(),
                object_id: None,
                value: Some(value.clone()),
            },
            serde_json::Value::Array(arr) => RemoteObjectInfo {
                js_type: "object".into(),
                subtype: Some("array".into()),
                class_name: "Array".into(),
                description: format!("Array({})", arr.len()),
                object_id: None,
                value: Some(value.clone()),
            },
            serde_json::Value::Object(_) => RemoteObjectInfo {
                js_type: "object".into(),
                subtype: None,
                class_name: "Object".into(),
                description: "Object".into(),
                object_id: None,
                value: Some(value.clone()),
            },
        }
    }

    pub(super) fn info_from_meta(meta: &serde_json::Value, object_id: Option<String>) -> RemoteObjectInfo {
        let js_type = meta
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("undefined")
            .to_string();
        let subtype = meta.get("subtype").and_then(|v| v.as_str()).map(|s| s.to_string());
        let class_name = meta.get("className").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let description = meta
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let value = if js_type != "object" && js_type != "function" {
            meta.get("description")
                .and_then(|v| v.as_str())
                .map(|s| serde_json::Value::String(s.to_string()))
        } else {
            None
        };

        RemoteObjectInfo {
            js_type,
            subtype,
            class_name,
            description,
            object_id,
            value,
        }
    }
}
