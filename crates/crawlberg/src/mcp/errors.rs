//! MCP error mapping.
//!
//! This module provides functions to map CrawlError variants to MCP error responses.

use crate::error::CrawlError;
use rmcp::ErrorData as McpError;

/// Map CrawlError variants to MCP error responses with appropriate error codes.
///
/// This function ensures different error types are properly differentiated in MCP responses:
/// - `InvalidConfig` → `INVALID_PARAMS` (-32602)
/// - Network/connection errors → `INTERNAL_ERROR` (-32603) with descriptive context
/// - All other errors → `INTERNAL_ERROR` (-32603)
///
/// The error message is preserved to aid debugging.
#[doc(hidden)]
pub fn map_crawl_error(error: CrawlError) -> McpError {
    match error {
        CrawlError::InvalidConfig(msg) => McpError::invalid_params(format!("Invalid configuration: {msg}"), None),

        CrawlError::NotFound(msg) => McpError::internal_error(format!("Not found: {msg}"), None),

        CrawlError::Unauthorized(msg) => McpError::internal_error(format!("Unauthorized: {msg}"), None),

        CrawlError::Forbidden(msg) => McpError::internal_error(format!("Forbidden: {msg}"), None),

        CrawlError::WafBlocked { message, .. } => {
            McpError::internal_error(format!("Blocked by WAF/bot protection: {message}"), None)
        }

        CrawlError::Timeout(msg) => McpError::internal_error(format!("Request timed out: {msg}"), None),

        CrawlError::RateLimited(msg) => McpError::internal_error(format!("Rate limited: {msg}"), None),

        CrawlError::ServerError(msg) => McpError::internal_error(format!("Server error: {msg}"), None),

        CrawlError::BadGateway(msg) => McpError::internal_error(format!("Bad gateway: {msg}"), None),

        CrawlError::Gone(msg) => McpError::internal_error(format!("Resource gone: {msg}"), None),

        CrawlError::Connection(msg) => McpError::internal_error(format!("Connection error: {msg}"), None),

        CrawlError::Dns(msg) => McpError::internal_error(format!("DNS resolution failed: {msg}"), None),

        CrawlError::Ssl(msg) => McpError::internal_error(format!("SSL/TLS error: {msg}"), None),

        CrawlError::DataLoss(msg) => McpError::internal_error(format!("Data loss during transfer: {msg}"), None),

        CrawlError::BrowserError(msg) => McpError::internal_error(format!("Browser error: {msg}"), None),

        CrawlError::BrowserTimeout(msg) => McpError::internal_error(format!("Browser timeout: {msg}"), None),

        CrawlError::Unsupported(msg) => McpError::invalid_params(format!("Unsupported operation: {msg}"), None),

        CrawlError::SsrfPolicyViolation { url, reason } => {
            McpError::invalid_params(format!("SSRF policy violation for {url}: {reason}"), None)
        }

        CrawlError::Other(msg) => McpError::internal_error(msg, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_map_invalid_config_to_invalid_params() {
        let error = CrawlError::InvalidConfig("bad value".to_string());
        let mcp_error = map_crawl_error(error);

        assert_eq!(mcp_error.code.0, -32602);
        assert!(mcp_error.message.contains("Invalid configuration"));
        assert!(mcp_error.message.contains("bad value"));
    }

    #[test]
    fn test_map_not_found_to_internal_error() {
        let error = CrawlError::NotFound("https://example.com/missing".to_string());
        let mcp_error = map_crawl_error(error);

        assert_eq!(mcp_error.code.0, -32603);
        assert!(mcp_error.message.contains("Not found"));
    }

    #[test]
    fn test_map_timeout_to_internal_error() {
        let error = CrawlError::Timeout("request exceeded 30s".to_string());
        let mcp_error = map_crawl_error(error);

        assert_eq!(mcp_error.code.0, -32603);
        assert!(mcp_error.message.contains("timed out"));
    }

    #[test]
    fn test_map_dns_to_internal_error() {
        let error = CrawlError::Dns("dns: could not resolve".to_string());
        let mcp_error = map_crawl_error(error);

        assert_eq!(mcp_error.code.0, -32603);
        assert!(mcp_error.message.contains("DNS"));
    }

    #[test]
    fn test_map_ssl_to_internal_error() {
        let error = CrawlError::Ssl("ssl: certificate expired".to_string());
        let mcp_error = map_crawl_error(error);

        assert_eq!(mcp_error.code.0, -32603);
        assert!(mcp_error.message.contains("SSL/TLS"));
    }

    #[test]
    fn test_map_other_to_internal_error() {
        let error = CrawlError::Other("unexpected failure".to_string());
        let mcp_error = map_crawl_error(error);

        assert_eq!(mcp_error.code.0, -32603);
        assert!(mcp_error.message.contains("unexpected failure"));
    }

    #[test]
    fn test_error_type_differentiation() {
        let config_err = CrawlError::InvalidConfig("test".to_string());
        let network_err = CrawlError::Connection("test".to_string());

        let config_mcp = map_crawl_error(config_err);
        let network_mcp = map_crawl_error(network_err);

        assert_eq!(config_mcp.code.0, -32602);
        assert_eq!(network_mcp.code.0, -32603);
        assert_ne!(config_mcp.code.0, network_mcp.code.0);
    }

    #[test]
    fn test_all_error_variants_have_mappings() {
        let errors = vec![
            CrawlError::NotFound("test".to_string()),
            CrawlError::Unauthorized("test".to_string()),
            CrawlError::Forbidden("test".to_string()),
            CrawlError::WafBlocked {
                vendor: "unknown".to_string(),
                message: "test".to_string(),
            },
            CrawlError::Timeout("test".to_string()),
            CrawlError::RateLimited("test".to_string()),
            CrawlError::ServerError("test".to_string()),
            CrawlError::BadGateway("test".to_string()),
            CrawlError::Gone("test".to_string()),
            CrawlError::Connection("test".to_string()),
            CrawlError::Dns("test".to_string()),
            CrawlError::Ssl("test".to_string()),
            CrawlError::DataLoss("test".to_string()),
            CrawlError::BrowserError("test".to_string()),
            CrawlError::BrowserTimeout("test".to_string()),
            CrawlError::InvalidConfig("test".to_string()),
            CrawlError::Unsupported("test".to_string()),
            CrawlError::Other("test".to_string()),
        ];

        for error in errors {
            let mcp_error = map_crawl_error(error);
            assert!(mcp_error.code.0 < 0, "Error code should be negative");
            assert!(!mcp_error.message.is_empty());
        }
    }
}
