//! Protocol `2026-07-28` envelope + dual-mode lifecycle tests.
//!
//! Drive the real axum router (same as `transport_auth`) so headers and body
//! limits are exercised. Requests use a fixed test secret so authentication does
//! not obscure protocol failures.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use photonic_core::history::CommandHistory;
use photonic_core::{AuditLog, Document};
use photonic_mcp::handlers;
use photonic_mcp::protocol::{ProtocolMode, PROTOCOL_2026_07_28};
use photonic_mcp::server::{build_router, AppState, McpServerConfig};
use photonic_mcp::MCP_SECRET_HEADER;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::Mutex;
use tower::ServiceExt;

const TEST_SECRET: &str = "protocol-test-secret";

fn test_state(mode: ProtocolMode) -> AppState {
    let (tx, _rx) = std::sync::mpsc::channel();
    AppState {
        document: Arc::new(Mutex::new(Document::new("t", 1920.0, 1080.0))),
        history: Arc::new(Mutex::new(CommandHistory::new(200))),
        document_path: Arc::new(StdMutex::new(None)),
        capture_tx: Arc::new(StdMutex::new(tx)),
        config: McpServerConfig {
            port: 0,
            secret: Some(TEST_SECRET.to_string()),
            protocol_mode: mode,
        },
        path_policy: photonic_core::PathPolicy::desktop_default(),
        audit_log: Arc::new(StdMutex::new(AuditLog::new())),
        clipboard_ring: Arc::new(handlers::clipboard::new_clipboard_ring()),
        video_engine: Arc::new(handlers::video_jobs::VideoEngineHandle::new()),
        video_jobs: Arc::new(StdMutex::new(handlers::video_jobs::JobRegistry::new())),
    }
}

fn post(body: Value, headers: &[(&str, &str)]) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json")
        .header(MCP_SECRET_HEADER, TEST_SECRET);
    for (n, v) in headers {
        builder = builder.header(*n, *v);
    }
    builder.body(Body::from(body.to_string())).unwrap()
}

async fn call(mode: ProtocolMode, body: Value, headers: &[(&str, &str)]) -> (StatusCode, Value) {
    let res = build_router(test_state(mode))
        .oneshot(post(body, headers))
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let v: Value =
        serde_json::from_slice(&bytes).unwrap_or(json!({ "raw": String::from_utf8_lossy(&bytes) }));
    (status, v)
}

fn meta_2026() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": PROTOCOL_2026_07_28,
        "io.modelcontextprotocol/clientCapabilities": {},
        "io.modelcontextprotocol/clientInfo": { "name": "protocol_2026_test", "version": "0" }
    })
}

#[tokio::test]
async fn discover_dual_lists_2026_and_legacy() {
    let (st, v) = call(
        ProtocolMode::Dual,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "server/discover",
            "params": { "_meta": meta_2026() }
        }),
        &[("Mcp-Method", "server/discover")],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let r = &v["result"];
    assert_eq!(r["resultType"], "complete");
    let vers = r["supportedVersions"].as_array().unwrap();
    assert!(vers.iter().any(|x| x == PROTOCOL_2026_07_28));
    assert!(vers.iter().any(|x| x == "2024-11-05"));
    assert_eq!(
        r["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "photonic"
    );
}

#[tokio::test]
async fn tools_list_has_result_type() {
    let (st, v) = call(
        ProtocolMode::Dual,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        }),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let r = &v["result"];
    assert_eq!(r["resultType"], "complete");
    let tools = r["tools"].as_array().unwrap();
    // Compact default (Pattern B): promoted + search/execute only.
    assert!(
        tools.len() < 40,
        "compact tools/list should be small, got {}",
        tools.len()
    );
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
        .collect();
    assert!(names.contains(&"search_actions"), "{names:?}");
    assert!(names.contains(&"execute_action"), "{names:?}");
}

#[tokio::test]
async fn tools_list_full_returns_large_catalog() {
    let (st, v) = call(
        ProtocolMode::Dual,
        json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "tools/list",
            "params": { "full": true }
        }),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let tools = v["result"]["tools"].as_array().unwrap();
    assert!(
        tools.len() > 50,
        "full tools/list should be large, got {}",
        tools.len()
    );
}

#[tokio::test]
async fn dual_initialize_still_works() {
    let (st, v) = call(
        ProtocolMode::Dual,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "initialize",
            "params": {}
        }),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["result"]["protocolVersion"], "2024-11-05");
}

#[tokio::test]
async fn strict_rejects_initialize() {
    let (st, v) = call(
        ProtocolMode::Strict,
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "initialize",
            "params": {
                "_meta": meta_2026()
            }
        }),
        &[("Mcp-Method", "initialize")],
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(v["error"]["code"], -32601);
}

#[tokio::test]
async fn strict_requires_meta() {
    let (st, v) = call(
        ProtocolMode::Strict,
        json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/list",
            "params": {}
        }),
        &[("Mcp-Method", "tools/list")],
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(v["error"]["code"], -32602);
}

#[tokio::test]
async fn unsupported_protocol_version() {
    let (st, v) = call(
        ProtocolMode::Dual,
        json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "tools/list",
            "params": {
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": "1999-01-01",
                    "io.modelcontextprotocol/clientCapabilities": {}
                }
            }
        }),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(v["error"]["code"], -32022);
    assert!(v["error"]["data"]["supportedVersions"].is_array());
}

#[tokio::test]
async fn header_method_mismatch() {
    let (st, v) = call(
        ProtocolMode::Dual,
        json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/list",
            "params": {}
        }),
        &[("Mcp-Method", "tools/call")],
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(v["error"]["code"], -32020);
}

#[tokio::test]
async fn tools_call_wraps_result_type() {
    // undo is a safe no-op-ish call on empty history
    let (st, v) = call(
        ProtocolMode::Dual,
        json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "tools/call",
            "params": {
                "name": "undo",
                "arguments": {},
                "_meta": meta_2026()
            }
        }),
        &[("Mcp-Method", "tools/call"), ("Mcp-Name", "undo")],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["result"]["resultType"], "complete");
    assert!(v["result"]["content"].is_array());
}
