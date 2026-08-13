//! MCP error mapping.
//!
//! This module provides functions to map CrawlError variants to MCP responses.

use crate::error::CrawlError;
use rmcp::ErrorData as McpError;
use rmcp::model::{CallToolResult, ContentBlock};

/// Map a [`CrawlError`] to the MCP outcome a tool handler should return.
///
/// MCP distinguishes two failure channels (see `rmcp::model::CallToolResult::error`'s own
/// doc comment for the upstream framing this mirrors):
///
/// - **Protocol error** — `Err(ErrorData)`. The server cannot route or fulfil the request
///   because of the *request itself*: a malformed/policy-rejected parameter, or a named
///   resource that doesn't exist. MCP clients typically render this opaquely (e.g. "Tool
///   result missing due to internal error") — the caller's LLM/agent does not see the
///   message.
/// - **Tool-level error** — `Ok(CallToolResult::error(...))`. The request was valid and
///   crawlberg attempted it, but execution failed for a reason outside the caller's control.
///   The content reaches the caller like a normal result, so an agent can actually read it
///   and react (retry, try a different URL, report to its user) instead of it vanishing into
///   an opaque protocol failure.
///
/// Almost every `CrawlError` variant describes the *crawl target's* behavior — it
/// rate-limited us, its certificate expired, its server 500'd — which is never a malformed
/// MCP request, so it belongs on the tool-result channel:
///
/// - `InvalidConfig`, `Unsupported`, `SsrfPolicyViolation` → `Err(INVALID_PARAMS)`: the
///   caller's request itself is malformed or policy-rejected.
/// - `NotFound`, `Gone` → `Err(RESOURCE_NOT_FOUND)`: the target resource does not (or no
///   longer) exists. This is the JSON-RPC/MCP-reserved code for exactly this case, distinct
///   from a generic server failure.
/// - Everything else (upstream auth/rate-limit/server responses, transport/browser failures)
///   → `Ok(CallToolResult::error(...))`: the request was well-formed but execution failed for
///   a reason the caller cannot resolve by changing their input — that's a fact about the
///   crawl, not the MCP call, and the caller should see it.
///
/// The tool-result text keeps `CrawlError`'s `Display` output verbatim, including its stable
/// lowercase tag prefix (`"rate_limited: ..."`, `"browser_timeout: ..."`, ...) — the same
/// codes-plus-message convention `NetworkErrorKind::tag` documents, so downstream consumers
/// can pattern-match on the prefix instead of parsing free text.
pub(super) fn crawl_error_to_tool_result(error: CrawlError) -> Result<CallToolResult, McpError> {
    match error {
        CrawlError::InvalidConfig { .. }
        | CrawlError::Unsupported { .. }
        | CrawlError::SsrfPolicyViolation { .. }
        | CrawlError::NotFound { .. }
        | CrawlError::Gone { .. } => Err(map_crawl_error(error)),
        other => Ok(CallToolResult::error(vec![ContentBlock::text(other.to_string())])),
    }
}

/// Map CrawlError variants to MCP error responses with appropriate error codes.
///
/// This is the protocol-level half of [`crawl_error_to_tool_result`]; tool handlers should
/// call that function rather than this one directly. It remains public (if hidden from docs)
/// because it fully classifies every variant and is useful in isolation for tests and for any
/// caller that genuinely wants the JSON-RPC code for a `CrawlError` regardless of MCP's
/// tool-result/protocol-error split.
///
/// The error message is preserved to aid debugging.
#[doc(hidden)]
pub fn map_crawl_error(error: CrawlError) -> McpError {
    match error {
        CrawlError::InvalidConfig { message: msg, .. } => {
            McpError::invalid_params(format!("Invalid configuration: {msg}"), None)
        }

        CrawlError::NotFound { message: msg, .. } => McpError::resource_not_found(format!("Not found: {msg}"), None),

        CrawlError::Unauthorized { message: msg, .. } => McpError::internal_error(format!("Unauthorized: {msg}"), None),

        CrawlError::Forbidden { message: msg, .. } => McpError::internal_error(format!("Forbidden: {msg}"), None),

        CrawlError::WafBlocked { message, .. } => {
            McpError::internal_error(format!("Blocked by WAF/bot protection: {message}"), None)
        }

        CrawlError::Timeout { message: msg, .. } => McpError::internal_error(format!("Request timed out: {msg}"), None),

        CrawlError::RateLimited { message: msg, .. } => McpError::internal_error(format!("Rate limited: {msg}"), None),

        CrawlError::ServerError { message: msg, .. } => McpError::internal_error(format!("Server error: {msg}"), None),

        CrawlError::BadGateway { message: msg, .. } => McpError::internal_error(format!("Bad gateway: {msg}"), None),

        CrawlError::Gone { message: msg, .. } => McpError::resource_not_found(format!("Resource gone: {msg}"), None),

        CrawlError::Connection { message: msg, .. } => {
            McpError::internal_error(format!("Connection error: {msg}"), None)
        }

        CrawlError::Dns { message: msg, .. } => McpError::internal_error(format!("DNS resolution failed: {msg}"), None),

        CrawlError::Ssl { message: msg, .. } => McpError::internal_error(format!("SSL/TLS error: {msg}"), None),

        CrawlError::DataLoss { message: msg, .. } => {
            McpError::internal_error(format!("Data loss during transfer: {msg}"), None)
        }

        CrawlError::BrowserError { message: msg, .. } => {
            McpError::internal_error(format!("Browser error: {msg}"), None)
        }

        CrawlError::BrowserTimeout { message: msg, .. } => {
            McpError::internal_error(format!("Browser timeout: {msg}"), None)
        }

        CrawlError::Unsupported { message: msg, .. } => {
            McpError::invalid_params(format!("Unsupported operation: {msg}"), None)
        }

        CrawlError::SsrfPolicyViolation { url, reason, .. } => {
            McpError::invalid_params(format!("SSRF policy violation for {url}: {reason}"), None)
        }

        CrawlError::Other { message: msg, .. } => McpError::internal_error(msg, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_map_invalid_config_to_invalid_params() {
        let error = CrawlError::invalid_config("bad value".to_string());
        let mcp_error = map_crawl_error(error);

        assert_eq!(mcp_error.code.0, -32602);
        assert!(mcp_error.message.contains("Invalid configuration"));
        assert!(mcp_error.message.contains("bad value"));
    }

    #[test]
    fn test_map_not_found_to_resource_not_found() {
        let error = CrawlError::not_found("https://example.com/missing".to_string());
        let mcp_error = map_crawl_error(error);

        assert_eq!(
            mcp_error.code.0, -32002,
            "NotFound must map to the RESOURCE_NOT_FOUND code, got {}",
            mcp_error.code.0
        );
        assert!(mcp_error.message.contains("Not found"));
    }

    #[test]
    fn test_map_gone_to_resource_not_found() {
        let error = CrawlError::gone("https://example.com/retired".to_string());
        let mcp_error = map_crawl_error(error);

        assert_eq!(
            mcp_error.code.0, -32002,
            "Gone must map to the RESOURCE_NOT_FOUND code, got {}",
            mcp_error.code.0
        );
        assert!(mcp_error.message.contains("Resource gone"));
    }

    #[test]
    fn test_map_unauthorized_to_internal_error() {
        let error = CrawlError::unauthorized("no credentials".to_string());
        let mcp_error = map_crawl_error(error);

        assert_eq!(
            mcp_error.code.0, -32603,
            "Unauthorized is an upstream condition, not a malformed request"
        );
    }

    #[test]
    fn test_map_rate_limited_to_internal_error() {
        let error = CrawlError::rate_limited("too many requests".to_string());
        let mcp_error = map_crawl_error(error);

        assert_eq!(
            mcp_error.code.0, -32603,
            "RateLimited is an upstream condition, not a malformed request"
        );
    }

    #[test]
    fn test_map_unsupported_to_invalid_params() {
        let error = CrawlError::unsupported("feature X".to_string());
        let mcp_error = map_crawl_error(error);

        assert_eq!(
            mcp_error.code.0, -32602,
            "Unsupported reflects a caller request we cannot fulfil"
        );
    }

    #[test]
    fn test_map_ssrf_policy_violation_to_invalid_params() {
        let error = CrawlError::SsrfPolicyViolation {
            url: "http://169.254.169.254/".to_string(),
            reason: "link-local address blocked".to_string(),
            source: None,
        };
        let mcp_error = map_crawl_error(error);

        assert_eq!(
            mcp_error.code.0, -32602,
            "SsrfPolicyViolation rejects the caller-supplied URL"
        );
        assert!(mcp_error.message.contains("169.254.169.254"));
    }

    #[test]
    fn test_map_timeout_to_internal_error() {
        let error = CrawlError::timeout("request exceeded 30s".to_string());
        let mcp_error = map_crawl_error(error);

        assert_eq!(mcp_error.code.0, -32603);
        assert!(mcp_error.message.contains("timed out"));
    }

    #[test]
    fn test_map_dns_to_internal_error() {
        let error = CrawlError::dns("dns: could not resolve".to_string());
        let mcp_error = map_crawl_error(error);

        assert_eq!(mcp_error.code.0, -32603);
        assert!(mcp_error.message.contains("DNS"));
    }

    #[test]
    fn test_map_ssl_to_internal_error() {
        let error = CrawlError::ssl("ssl: certificate expired".to_string());
        let mcp_error = map_crawl_error(error);

        assert_eq!(mcp_error.code.0, -32603);
        assert!(mcp_error.message.contains("SSL/TLS"));
    }

    #[test]
    fn test_map_other_to_internal_error() {
        let error = CrawlError::other("unexpected failure".to_string());
        let mcp_error = map_crawl_error(error);

        assert_eq!(mcp_error.code.0, -32603);
        assert!(mcp_error.message.contains("unexpected failure"));
    }

    #[test]
    fn test_error_type_differentiation() {
        let config_err = CrawlError::invalid_config("test".to_string());
        let network_err = CrawlError::connection("test".to_string());

        let config_mcp = map_crawl_error(config_err);
        let network_mcp = map_crawl_error(network_err);

        assert_eq!(config_mcp.code.0, -32602);
        assert_eq!(network_mcp.code.0, -32603);
        assert_ne!(config_mcp.code.0, network_mcp.code.0);
    }

    #[test]
    fn test_all_error_variants_have_mappings() {
        let errors = vec![
            CrawlError::not_found("test".to_string()),
            CrawlError::unauthorized("test".to_string()),
            CrawlError::forbidden("test".to_string()),
            CrawlError::WafBlocked {
                vendor: "unknown".to_string(),
                message: "test".to_string(),
            },
            CrawlError::timeout("test".to_string()),
            CrawlError::rate_limited("test".to_string()),
            CrawlError::server_error("test".to_string()),
            CrawlError::bad_gateway("test".to_string()),
            CrawlError::gone("test".to_string()),
            CrawlError::connection("test".to_string()),
            CrawlError::dns("test".to_string()),
            CrawlError::ssl("test".to_string()),
            CrawlError::data_loss("test".to_string()),
            CrawlError::browser_error("test".to_string()),
            CrawlError::browser_timeout("test".to_string()),
            CrawlError::invalid_config("test".to_string()),
            CrawlError::unsupported("test".to_string()),
            CrawlError::other("test".to_string()),
        ];

        for error in errors {
            let mcp_error = map_crawl_error(error);
            assert!(mcp_error.code.0 < 0, "Error code should be negative");
            assert!(!mcp_error.message.is_empty());
        }
    }

    #[test]
    fn protocol_level_variants_route_to_err_via_crawl_error_to_tool_result() {
        let cases: Vec<(CrawlError, i32)> = vec![
            (CrawlError::invalid_config("bad value".to_string()), -32602),
            (CrawlError::unsupported("feature X".to_string()), -32602),
            (
                CrawlError::SsrfPolicyViolation {
                    url: "http://169.254.169.254/".to_string(),
                    reason: "link-local address blocked".to_string(),
                    source: None,
                },
                -32602,
            ),
            (CrawlError::not_found("https://example.com/missing".to_string()), -32002),
            (CrawlError::gone("https://example.com/retired".to_string()), -32002),
        ];

        for (error, expected_code) in cases {
            let description = error.to_string();
            let mcp_error = crawl_error_to_tool_result(error).unwrap_err();
            assert_eq!(
                mcp_error.code.0, expected_code,
                "{description} must map to protocol code {expected_code}, got {}",
                mcp_error.code.0
            );
        }
    }

    #[test]
    fn runtime_domain_variants_route_to_ok_tool_result_with_is_error_true() {
        let cases: Vec<CrawlError> = vec![
            CrawlError::unauthorized("no credentials".to_string()),
            CrawlError::forbidden("blocked".to_string()),
            CrawlError::WafBlocked {
                vendor: "cloudflare".to_string(),
                message: "challenge page".to_string(),
            },
            CrawlError::timeout("request exceeded 30s".to_string()),
            CrawlError::rate_limited("too many requests".to_string()),
            CrawlError::server_error("upstream 500".to_string()),
            CrawlError::bad_gateway("upstream 502".to_string()),
            CrawlError::connection("refused".to_string()),
            CrawlError::dns("could not resolve".to_string()),
            CrawlError::ssl("certificate expired".to_string()),
            CrawlError::data_loss("truncated body".to_string()),
            CrawlError::browser_error("failed to launch".to_string()),
            CrawlError::browser_timeout("page never loaded".to_string()),
            CrawlError::other("unexpected failure".to_string()),
        ];

        for error in cases {
            let expected_text = error.to_string();
            let result = crawl_error_to_tool_result(error).unwrap_or_else(|mcp_error| {
                panic!("{expected_text} must route to the tool-result channel, got protocol error {mcp_error:?}")
            });

            assert_eq!(
                result.is_error,
                Some(true),
                "{expected_text} must produce a tool result with is_error: true, got {:?}",
                result.is_error
            );

            let text = result
                .content
                .iter()
                .find_map(|block| block.as_text().map(|content| content.text.clone()))
                .unwrap_or_default();
            assert_eq!(
                text, expected_text,
                "tool-result content must preserve CrawlError's Display text (including its stable tag prefix)"
            );
        }
    }

    #[test]
    fn rate_limited_is_a_tool_result_not_a_protocol_error() {
        // ~keep Named regression for the exact bug this fixes: RateLimited (an upstream
        // ~keep condition, not a malformed request) used to map to INTERNAL_ERROR (-32603), an
        // ~keep opaque protocol failure most MCP clients never surface to the caller.
        let error = CrawlError::rate_limited("too many requests".to_string());
        let result =
            crawl_error_to_tool_result(error).expect("RateLimited must be Ok(CallToolResult), not a protocol error");
        assert_eq!(
            result.is_error,
            Some(true),
            "RateLimited tool result must set is_error: true"
        );
    }
}
