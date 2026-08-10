#![cfg(feature = "mcp")]

//! MCP contract tests.
//!
//! These tests verify that types used by the MCP server maintain
//! serialization stability (JSON round-trip), and that the MCP error
//! taxonomy contract (`CrawlError` variant -> JSON-RPC error code) holds
//! end-to-end through the real HTTP/JSON-RPC pipeline.

use crawlberg::CrawlConfig;

#[test]
fn test_crawl_config_json_roundtrip() {
    let config = CrawlConfig {
        max_depth: Some(5),
        max_pages: Some(100),
        max_concurrent: Some(10),
        respect_robots_txt: true,
        user_agent: Some("TestBot/1.0".to_string()),
        stay_on_domain: true,
        allow_subdomains: true,
        cookies_enabled: true,
        retry_count: 3,
        max_redirects: 5,
        ..Default::default()
    };

    let json = serde_json::to_string_pretty(&config).expect("serialize should succeed");
    let deserialized: CrawlConfig = serde_json::from_str(&json).expect("deserialize should succeed");

    assert_eq!(deserialized.max_depth, Some(5));
    assert_eq!(deserialized.max_pages, Some(100));
    assert_eq!(deserialized.max_concurrent, Some(10));
    assert!(deserialized.respect_robots_txt);
    assert_eq!(deserialized.user_agent.as_deref(), Some("TestBot/1.0"));
    assert!(deserialized.stay_on_domain);
    assert!(deserialized.allow_subdomains);
    assert!(deserialized.cookies_enabled);
    assert_eq!(deserialized.retry_count, 3);
    assert_eq!(deserialized.max_redirects, 5);
}

/// End-to-end contract: a `CrawlError::NotFound` (produced here by a real
/// HTTP 404 response from a local mock server) must surface through the whole
/// MCP pipeline as `RESOURCE_NOT_FOUND` (-32002), not `INTERNAL_ERROR`
/// (-32603). This exercises the public `streamable_http_service` against a
/// real server, proving the taxonomy fix in `mcp::errors::map_crawl_error`
/// survives the full engine -> tool -> JSON-RPC round trip, not just the
/// mapping function in isolation.
#[cfg(all(feature = "api", feature = "mcp"))]
#[tokio::test]
async fn scrape_tool_maps_404_to_resource_not_found_over_mcp() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use crawlberg::HostMatcher;
    use tower::ServiceExt;

    let mock = axum::Router::new().route("/missing", get(|| async { StatusCode::NOT_FOUND }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock server");
    let mock_addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, mock).await.expect("mock server runs");
    });

    // ~keep The default SSRF policy denies loopback targets; explicitly
    // ~keep allowlist 127.0.0.0/8 so this test reaches its own mock server
    // ~keep instead of being rejected as SsrfPolicyViolation before the request.
    let mut config = CrawlConfig::default();
    config
        .ssrf
        .allowlist
        .push(HostMatcher::cidr("127.0.0.0/8").expect("literal CIDR is valid"));

    let mcp_app = axum::Router::new().nest_service("/mcp", crawlberg::streamable_http_service(config));

    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2026-07-28","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}"#;
    let init_response = mcp_app
        .clone()
        .oneshot(mcp_request(init.to_string()))
        .await
        .expect("initialize succeeds");
    assert_eq!(
        init_response.status(),
        StatusCode::OK,
        "initialize must succeed before tools/call"
    );

    let url = format!("http://{mock_addr}/missing");
    let call_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": { "name": "scrape", "arguments": { "url": url } },
    })
    .to_string();
    let response = mcp_app
        .oneshot(mcp_request(call_body))
        .await
        .expect("tools/call responds");
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body collects");
    let value = parse_json_rpc_frame(&String::from_utf8_lossy(&bytes));

    assert_eq!(
        value["error"]["code"],
        serde_json::json!(-32002),
        "a 404 from the crawl target must map to RESOURCE_NOT_FOUND, not INTERNAL_ERROR: {value}"
    );

    fn mcp_request(body: String) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("host", "localhost")
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body(Body::from(body))
            .expect("valid request")
    }

    /// Extract the JSON-RPC frame from a plain-JSON or SSE (`data: ...`) body.
    fn parse_json_rpc_frame(body: &str) -> serde_json::Value {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(body.trim())
            && (value.get("result").is_some() || value.get("error").is_some())
        {
            return value;
        }
        body.lines()
            .find_map(|line| {
                line.strip_prefix("data: ")
                    .and_then(|rest| serde_json::from_str(rest).ok())
            })
            .unwrap_or_else(|| panic!("no JSON-RPC result frame found in body: {body}"))
    }
}
