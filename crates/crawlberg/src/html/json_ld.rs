//! JSON-LD structured data extraction from HTML documents.

use serde_json::Value;
use tl::VDom;

use crate::types::JsonLdEntry;

use super::selectors::SEL_JSON_LD;

/// Extract JSON-LD structured data entries from a parsed HTML document.
///
/// A `<script type="application/ld+json">` block may contain a single object, an
/// array of objects, or an object wrapping its nodes in `@graph`. Each case is
/// expanded into one [`JsonLdEntry`] per node so `@type`/`name` are preserved
/// instead of collapsing the whole block into a single, mostly-empty entry.
pub(crate) fn extract_json_ld(dom: &VDom<'_>) -> Vec<JsonLdEntry> {
    let parser = dom.parser();
    let mut entries = Vec::new();

    if let Some(iter) = dom.query_selector(SEL_JSON_LD) {
        for handle in iter {
            if let Some(node) = handle.get(parser) {
                let raw = node.inner_text(parser).to_string();
                if let Ok(val) = serde_json::from_str::<Value>(&raw) {
                    match &val {
                        // A bare top-level object (the common case) keeps the original,
                        // unreformatted raw text for the single entry it produces.
                        Value::Object(map) if !map.contains_key("@graph") => push_entry(&val, raw, &mut entries),
                        _ => collect_nodes(&val, &mut entries),
                    }
                }
            }
        }
    }
    entries
}

/// Recursively expand JSON-LD arrays and `@graph` blocks into individual entries.
fn collect_nodes(val: &Value, entries: &mut Vec<JsonLdEntry>) {
    match val {
        Value::Array(items) => {
            for item in items {
                collect_nodes(item, entries);
            }
        }
        Value::Object(_) => {
            if let Some(graph) = val.get("@graph").and_then(Value::as_array) {
                for item in graph {
                    collect_nodes(item, entries);
                }
                return;
            }
            push_entry(val, val.to_string(), entries);
        }
        _ => {}
    }
}

fn push_entry(val: &Value, raw: String, entries: &mut Vec<JsonLdEntry>) {
    let schema_type = val.get("@type").and_then(Value::as_str).unwrap_or("").to_owned();
    let name = val.get("name").and_then(Value::as_str).map(String::from);
    entries.push(JsonLdEntry { schema_type, name, raw });
}

#[cfg(test)]
mod tests {
    use tl::ParserOptions;

    use super::*;

    fn extract(html: &str) -> Vec<JsonLdEntry> {
        let dom = tl::parse(html, ParserOptions::default()).expect("valid HTML");
        extract_json_ld(&dom)
    }

    #[test]
    fn single_object_block_keeps_original_raw_text() {
        let html = r#"<script type="application/ld+json">
        {"@context":"https://schema.org","@type":"Article","name":"Hello"}
        </script>"#;
        let entries = extract(html);
        assert_eq!(entries.len(), 1, "expected one entry, got {entries:?}");
        assert_eq!(entries[0].schema_type, "Article", "schema_type mismatch: {entries:?}");
        assert_eq!(entries[0].name.as_deref(), Some("Hello"), "name mismatch: {entries:?}");
    }

    #[test]
    fn array_wrapped_block_yields_one_entry_per_item() {
        let html = r#"<script type="application/ld+json">
        [
            {"@type":"Organization","name":"Acme Corp"},
            {"@type":"WebSite","name":"Acme Site"}
        ]
        </script>"#;
        let entries = extract(html);
        assert_eq!(entries.len(), 2, "expected two entries, got {entries:?}");
        assert_eq!(entries[0].schema_type, "Organization", "entry 0 type: {entries:?}");
        assert_eq!(
            entries[0].name.as_deref(),
            Some("Acme Corp"),
            "entry 0 name: {entries:?}"
        );
        assert_eq!(entries[1].schema_type, "WebSite", "entry 1 type: {entries:?}");
        assert_eq!(
            entries[1].name.as_deref(),
            Some("Acme Site"),
            "entry 1 name: {entries:?}"
        );
    }

    #[test]
    fn graph_wrapped_block_expands_each_node() {
        let html = r#"<script type="application/ld+json">
        {
            "@context": "https://schema.org",
            "@graph": [
                {"@type":"Person","name":"Jane Doe"},
                {"@type":"Product","name":"Widget"}
            ]
        }
        </script>"#;
        let entries = extract(html);
        assert_eq!(entries.len(), 2, "expected two entries, got {entries:?}");
        assert_eq!(entries[0].schema_type, "Person", "entry 0 type: {entries:?}");
        assert_eq!(
            entries[0].name.as_deref(),
            Some("Jane Doe"),
            "entry 0 name: {entries:?}"
        );
        assert_eq!(entries[1].schema_type, "Product", "entry 1 type: {entries:?}");
        assert_eq!(entries[1].name.as_deref(), Some("Widget"), "entry 1 name: {entries:?}");
    }

    #[test]
    fn multiple_script_blocks_each_contribute_entries() {
        let html = r#"
        <script type="application/ld+json">{"@type":"Article","name":"First"}</script>
        <script type="application/ld+json">{"@type":"Recipe","name":"Second"}</script>
        "#;
        let entries = extract(html);
        assert_eq!(entries.len(), 2, "expected two entries, got {entries:?}");
        assert_eq!(entries[0].schema_type, "Article", "entry 0: {entries:?}");
        assert_eq!(entries[1].schema_type, "Recipe", "entry 1: {entries:?}");
    }
}
