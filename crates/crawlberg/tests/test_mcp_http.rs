//! Integration tests for the Streamable HTTP MCP transport.
//!
//! Regression guard: the MCP [`ServerHandler`] must delegate `tools/list` and
//! `tools/call` to the generated tool router (via rmcp's `#[tool_handler]`). A
//! missing delegation still compiles but serves an empty tool list, so these
//! tests drive the real JSON-RPC protocol over the mounted HTTP service rather
//! than inspecting the router directly.

#![cfg(all(feature = "api", feature = "mcp"))]

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use crawlberg::CrawlConfig;
use tower::ServiceExt;

const ACCEPT: &str = "application/json, text/event-stream";

fn app() -> Router {
    Router::new().nest_service("/mcp", crawlberg::streamable_http_service(CrawlConfig::default()))
}

async fn post(app: &Router, session: Option<&str>, body: &str) -> (StatusCode, HeaderMap, String) {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("host", "localhost")
        .header("content-type", "application/json")
        .header("accept", ACCEPT);
    if let Some(sid) = session {
        builder = builder.header("mcp-session-id", sid);
    }
    let request = builder.body(Body::from(body.to_string())).expect("valid request");

    let response = app.clone().oneshot(request).await.expect("service responds");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body collects");
    (status, headers, String::from_utf8_lossy(&bytes).into_owned())
}

/// Extract the first JSON-RPC `result`/`error` frame from an SSE response body.
fn sse_result(body: &str) -> serde_json::Value {
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("data: ")
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(rest)
            && (value.get("result").is_some() || value.get("error").is_some())
        {
            return value;
        }
    }
    panic!("no JSON-RPC result frame found in SSE body: {body}");
}

async fn initialize(app: &Router) -> String {
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}"#;
    let (status, headers, _) = post(app, None, init).await;
    assert_eq!(status, StatusCode::OK, "initialize must return 200");
    let sid = headers
        .get("mcp-session-id")
        .expect("initialize must return an Mcp-Session-Id header")
        .to_str()
        .expect("session id is valid ascii")
        .to_string();
    let _ = post(
        app,
        Some(&sid),
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    )
    .await;
    sid
}

#[tokio::test]
async fn http_mcp_lists_all_nine_tools() {
    let app = app();
    let sid = initialize(&app).await;

    let (status, _headers, body) = post(
        &app,
        Some(&sid),
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let value = sse_result(&body);
    let tools = value["result"]["tools"]
        .as_array()
        .expect("tools/list returns a tools array");
    assert_eq!(
        tools.len(),
        9,
        "ServerHandler must delegate tools/list to the router (got: {tools:?})"
    );
}

#[tokio::test]
async fn http_mcp_serves_safety_annotations() {
    let app = app();
    let sid = initialize(&app).await;

    let (_status, _headers, body) = post(
        &app,
        Some(&sid),
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
    )
    .await;
    let value = sse_result(&body);
    let tools = value["result"]["tools"].as_array().expect("tools array");

    let find = |name: &str| {
        tools
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("tool `{name}` missing"))
            .clone()
    };

    let interact = find("interact");
    assert_eq!(interact["annotations"]["readOnlyHint"], serde_json::json!(false));
    assert_eq!(interact["annotations"]["destructiveHint"], serde_json::json!(true));
    assert_eq!(interact["annotations"]["openWorldHint"], serde_json::json!(true));

    let scrape = find("scrape");
    assert_eq!(scrape["annotations"]["readOnlyHint"], serde_json::json!(true));
    assert_eq!(scrape["annotations"]["openWorldHint"], serde_json::json!(true));

    let citations = find("generate_citations");
    assert_eq!(citations["annotations"]["openWorldHint"], serde_json::json!(false));
}
