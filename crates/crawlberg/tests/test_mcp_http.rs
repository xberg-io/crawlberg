//! Integration tests for the Streamable HTTP MCP transport.
//!
//! Regression guard: the MCP [`ServerHandler`] must delegate `tools/list` and
//! `tools/call` to the generated tool router, advertise the SEP-2663 tasks
//! extension, and serve statelessly (SEP-2567). These tests drive the real
//! JSON-RPC protocol over the mounted HTTP service rather than inspecting the
//! router directly.

#![cfg(all(feature = "api", feature = "mcp"))]

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use crawlberg::CrawlConfig;
use tower::ServiceExt;

const ACCEPT: &str = "application/json, text/event-stream";
const PROTOCOL_VERSION: &str = "2026-07-28";

fn app() -> Router {
    Router::new().nest_service("/mcp", crawlberg::streamable_http_service(CrawlConfig::default()))
}

async fn post(app: &Router, body: &str) -> (StatusCode, HeaderMap, String) {
    let request = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("host", "localhost")
        .header("content-type", "application/json")
        .header("accept", ACCEPT)
        .body(Body::from(body.to_string()))
        .expect("valid request");

    let response = app.clone().oneshot(request).await.expect("service responds");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body collects");
    (status, headers, String::from_utf8_lossy(&bytes).into_owned())
}

/// Extract the JSON-RPC `result`/`error` frame from a response body, tolerating
/// both a plain `application/json` body (stateless `json_response`) and an SSE
/// `text/event-stream` body (`data:` frames).
fn json_rpc_result(body: &str) -> serde_json::Value {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body.trim())
        && (value.get("result").is_some() || value.get("error").is_some())
    {
        return value;
    }
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("data: ")
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(rest)
            && (value.get("result").is_some() || value.get("error").is_some())
        {
            return value;
        }
    }
    panic!("no JSON-RPC result frame found in body: {body}");
}

/// A `tools/call` request body carrying `meta` in its `_meta` field.
fn tools_call_body(id: u32, name: &str, meta: serde_json::Value) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": { "name": name, "arguments": {}, "_meta": meta },
    })
    .to_string()
}

/// Perform the stateless initialize handshake and assert no session is issued.
async fn initialize(app: &Router) {
    let init = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"{PROTOCOL_VERSION}","capabilities":{{"extensions":{{"io.modelcontextprotocol/tasks":{{}}}}}},"clientInfo":{{"name":"test","version":"0"}}}}}}"#
    );
    let (status, headers, _) = post(app, &init).await;
    assert_eq!(status, StatusCode::OK, "initialize must return 200");
    assert!(
        headers.get("mcp-session-id").is_none(),
        "SEP-2567 stateless transport must not issue a session id"
    );
}

fn tools_of(value: &serde_json::Value) -> Vec<serde_json::Value> {
    value["result"]["tools"]
        .as_array()
        .expect("tools/list returns a tools array")
        .clone()
}

#[tokio::test]
async fn http_mcp_lists_all_nine_tools() {
    let app = app();
    initialize(&app).await;

    let (status, _headers, body) = post(&app, r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#).await;
    assert_eq!(status, StatusCode::OK);

    let tools = tools_of(&json_rpc_result(&body));
    assert_eq!(
        tools.len(),
        9,
        "ServerHandler must delegate tools/list to the router (got: {tools:?})"
    );
}

#[tokio::test]
async fn http_mcp_serves_safety_annotations() {
    let app = app();
    initialize(&app).await;

    let (_status, _headers, body) = post(&app, r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#).await;
    let tools = tools_of(&json_rpc_result(&body));

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

/// SEP-2106: a `tools/call` result carries machine-readable `structuredContent`
/// alongside the human-readable text block.
#[tokio::test]
async fn http_mcp_returns_structured_content() {
    let app = app();
    initialize(&app).await;

    let (status, _headers, response) = post(
        &app,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"get_version","arguments":{}}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let value = json_rpc_result(&response);
    let structured = &value["result"]["structuredContent"];
    assert!(
        structured.get("version").and_then(serde_json::Value::as_str).is_some(),
        "result must carry structuredContent with a version field, got: {value}"
    );
    // The human-readable text block is still present for non-structured clients.
    assert!(
        value["result"]["content"][0]["text"]
            .as_str()
            .is_some_and(|t| t.contains("version")),
        "result must still carry a text block: {value}"
    );
}

/// SEP-2663 over the SEP-2567 stateless transport: task-augmented `tools/call`
/// degrades gracefully to inline execution.
///
/// rmcp's stateless Streamable HTTP path does not propagate per-request client
/// capabilities (it reconstructs `peer_info` with default capabilities), so the
/// server cannot see the client's tasks capability and must not return a task
/// handle (which the protocol would reject as a missing-capability error).
/// Instead the tool runs inline and returns its result directly. Full task
/// support requires the capability-negotiating stdio transport
/// (see the `crawlberg-cli` stdio task test).
#[tokio::test]
async fn http_mcp_task_augmentation_degrades_to_inline() {
    let app = app();
    initialize(&app).await;

    let body = tools_call_body(
        2,
        "get_version",
        serde_json::json!({
            "io.modelcontextprotocol/tasks": { "ttl": 60000 }
        }),
    );
    let (status, _headers, response) = post(&app, &body).await;
    assert_eq!(status, StatusCode::OK, "task-augmented call must return 200");

    let value = json_rpc_result(&response);
    assert!(
        value.get("error").is_none(),
        "stateless task augmentation must not error, got: {value}"
    );
    assert!(
        value["result"].get("taskId").is_none(),
        "stateless transport must run inline, not return a task handle, got: {value}"
    );
    let text = value["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        text.contains("version"),
        "inline result must carry the get_version output: {value}"
    );
}

/// Error taxonomy boundary case: a malformed client request (bad URL scheme)
/// must surface as JSON-RPC `INVALID_PARAMS` (-32602), not `INTERNAL_ERROR`
/// (-32603). Proven end-to-end through the real HTTP/JSON-RPC pipeline, not
/// just the internal mapping function.
#[tokio::test]
async fn http_mcp_rejects_malformed_scrape_url_as_invalid_params() {
    let app = app();
    initialize(&app).await;

    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": { "name": "scrape", "arguments": { "url": "not-a-url" } },
    })
    .to_string();
    let (status, _headers, response) = post(&app, &body).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "JSON-RPC errors are still transported over HTTP 200"
    );

    let value = json_rpc_result(&response);
    assert_eq!(
        value["error"]["code"],
        serde_json::json!(-32602),
        "a malformed url must be INVALID_PARAMS, not INTERNAL_ERROR: {value}"
    );
}

/// Error taxonomy boundary case: an unknown tool name must not be reported as
/// an internal server error.
#[tokio::test]
async fn http_mcp_unknown_tool_name_is_not_internal_error() {
    let app = app();
    initialize(&app).await;

    let body = tools_call_body(2, "not_a_real_tool", serde_json::json!({}));
    let (status, _headers, response) = post(&app, &body).await;
    assert_eq!(status, StatusCode::OK);

    let value = json_rpc_result(&response);
    let code = value["error"]["code"].as_i64().unwrap_or(0);
    assert_ne!(
        code, -32603,
        "an unknown tool name must not surface as INTERNAL_ERROR: {value}"
    );
}

/// Error taxonomy boundary case: an out-of-range `max_pages` must be rejected
/// as a client request problem, not executed or reported as an internal error.
#[tokio::test]
async fn http_mcp_crawl_rejects_zero_max_pages_as_invalid_params() {
    let app = app();
    initialize(&app).await;

    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": { "name": "crawl", "arguments": { "url": "https://example.com", "max_pages": 0 } },
    })
    .to_string();
    let (status, _headers, response) = post(&app, &body).await;
    assert_eq!(status, StatusCode::OK);

    let value = json_rpc_result(&response);
    assert_eq!(
        value["error"]["code"],
        serde_json::json!(-32602),
        "max_pages: 0 must be rejected as INVALID_PARAMS: {value}"
    );
}
