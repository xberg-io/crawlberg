//! Helpers for driving the MCP server over its streamable HTTP service.

use axum::body::Body;
use axum::http::Request;

/// A JSON-RPC POST to `/mcp` carrying `body`.
pub fn mcp_request(body: String) -> Request<Body> {
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
pub fn json_rpc_frame(body: &str) -> serde_json::Value {
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
