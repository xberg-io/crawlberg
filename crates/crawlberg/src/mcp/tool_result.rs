//! Input validation and result shaping shared by the MCP tool methods.

use rmcp::model::{CallToolResult, ContentBlock};

/// Validate that a URL is non-empty and uses http(s) scheme.
pub(super) fn validate_url(url: &str) -> Result<(), rmcp::ErrorData> {
    if url.is_empty() {
        return Err(rmcp::ErrorData::invalid_params("url is required", None));
    }
    if !crate::net::has_http_scheme(url) {
        return Err(rmcp::ErrorData::invalid_params(
            "url must start with http:// or https://",
            None,
        ));
    }
    Ok(())
}

/// Parse the optional format parameter, defaulting to "markdown".
pub(super) fn parse_format(format: &Option<String>) -> &str {
    match format.as_deref() {
        Some(f) if f.eq_ignore_ascii_case("json") => "json",
        _ => "markdown",
    }
}

/// Build a tool result carrying both a human-readable text block and the
/// machine-readable `structuredContent` (SEP-2106).
///
/// `structured` is the JSON value clients can consume programmatically; `text`
/// is the same information rendered for humans (markdown or pretty JSON). The
/// `format` parameter selects the text representation only — `structuredContent`
/// is always populated so schema-aware clients get typed output regardless.
pub(super) fn structured_success(structured: serde_json::Value, text: String) -> CallToolResult {
    let mut result = CallToolResult::success(vec![ContentBlock::text(text)]);
    result.structured_content = Some(structured);
    result
}

/// Serialize a value into `structuredContent`, mapping failures to an MCP error.
pub(super) fn to_structured<T: serde::Serialize>(value: &T) -> Result<serde_json::Value, rmcp::ErrorData> {
    serde_json::to_value(value)
        .map_err(|e| rmcp::ErrorData::internal_error(format!("failed to serialize structured content: {e}"), None))
}
