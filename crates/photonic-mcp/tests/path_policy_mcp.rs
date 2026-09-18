//! PathPolicy at the MCP save boundary.
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use photonic_core::history::CommandHistory;
use photonic_core::{AuditLog, Document, PathAccess, PathPolicy};
use photonic_mcp::handlers;
use photonic_mcp::protocol::ProtocolMode;
use photonic_mcp::server::{build_router, AppState, McpServerConfig};
use photonic_mcp::MCP_SECRET_HEADER;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::Mutex;
use tower::ServiceExt;

fn state_with_policy(pol: PathPolicy) -> AppState {
    let (tx, _rx) = std::sync::mpsc::channel();
    AppState {
        document: Arc::new(Mutex::new(Document::new("t", 1920.0, 1080.0))),
        history: Arc::new(Mutex::new(CommandHistory::new(200))),
        document_path: Arc::new(StdMutex::new(None)),
        capture_tx: Arc::new(StdMutex::new(tx)),
        config: McpServerConfig {
            port: 0,
            secret: Some("test-secret".to_string()),
            protocol_mode: ProtocolMode::Dual,
        },
        path_policy: pol,
        audit_log: Arc::new(StdMutex::new(AuditLog::new())),
        clipboard_ring: Arc::new(handlers::clipboard::new_clipboard_ring()),
        video_engine: Arc::new(handlers::video_jobs::VideoEngineHandle::new()),
        video_jobs: Arc::new(StdMutex::new(handlers::video_jobs::JobRegistry::new())),
    }
}

#[tokio::test]
async fn save_document_outside_roots_denied() {
    let root = std::env::temp_dir().join(format!("pp-mcp-root-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&root);
    let outside = std::env::temp_dir().join(format!("pp-mcp-out-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&outside);
    let bad = outside.join("evil.photon");

    let pol = PathPolicy::new([root.clone()], true);
    // Sanity: write denied by policy itself
    assert!(matches!(
        pol.check(&bad, PathAccess::Write),
        photonic_core::PathVerdict::Denied { .. }
    ));

    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "save_document",
            "arguments": { "path": bad.to_string_lossy() }
        }
    });
    let req = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json")
        .header(MCP_SECRET_HEADER, "test-secret")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = build_router(state_with_policy(pol))
        .oneshot(req)
        .await
        .unwrap();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["result"]["resultType"], "complete");
    assert_eq!(v["result"]["isError"], true);
    let text = v["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert!(text.contains("PathNotPermitted"), "{text}");

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&outside);
}
