//! Transport-level coverage for the MCP hardening in
//! docs/specs/video-editor/28-security-model.md §4 points 1–2 (§8 acceptance
//! rows 1–2).
//!
//! Unlike `mcp_parity.rs`, which drives handler fns directly and bypasses
//! axum, these tests build the real `Router` and push requests through it with
//! `tower::ServiceExt::oneshot` — the auth middleware and the absence of a
//! CORS layer only exist at that layer, so nothing below it can prove them.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use photonic_core::history::CommandHistory;
use photonic_core::{AuditLog, Document};
use photonic_mcp::handlers;
use photonic_mcp::server::{build_router, AppState, McpServerConfig};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::Mutex;
use tower::ServiceExt;

const TEST_TOKEN: &str = "test-token-aaaa";

/// Mirrors `mcp_parity.rs::test_state`, but pins a known secret so the auth
/// middleware is actually engaged.
fn test_state(secret: Option<&str>) -> AppState {
    let (tx, _rx) = std::sync::mpsc::channel();
    AppState {
        document: Arc::new(Mutex::new(Document::new("t", 1920.0, 1080.0))),
        history: Arc::new(Mutex::new(CommandHistory::new(200))),
        document_path: Arc::new(StdMutex::new(None)),
        capture_tx: Arc::new(StdMutex::new(tx)),
        config: McpServerConfig {
            port: 0,
            secret: secret.map(|s| s.to_string()),
            protocol_mode: Default::default(),
        },
        path_policy: photonic_core::PathPolicy::desktop_default(),
        audit_log: Arc::new(StdMutex::new(AuditLog::new())),
        clipboard_ring: Arc::new(handlers::clipboard::new_clipboard_ring()),
        video_engine: Arc::new(handlers::video_jobs::VideoEngineHandle::new()),
        video_jobs: Arc::new(StdMutex::new(handlers::video_jobs::JobRegistry::new())),
    }
}

fn initialize_body() -> Body {
    Body::from(
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        })
        .to_string(),
    )
}

/// `POST /mcp` carrying whatever headers the caller supplies.
fn mcp_post(headers: &[(&str, &str)]) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder.body(initialize_body()).unwrap()
}

async fn send(secret: Option<&str>, req: Request<Body>) -> axum::response::Response {
    build_router(test_state(secret)).oneshot(req).await.unwrap()
}

// ── §8 row 1: unauthenticated requests get a bare 401 ────────────────────────

#[tokio::test]
async fn missing_authorization_is_401_with_empty_body() {
    let res = send(Some(TEST_TOKEN), mcp_post(&[])).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert!(
        res.headers().get("www-authenticate").is_none(),
        "401 must not hint at the scheme"
    );
    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert!(body.is_empty(), "401 must carry no detail, got {body:?}");
}

#[tokio::test]
async fn wrong_token_is_401() {
    let res = send(
        Some(TEST_TOKEN),
        mcp_post(&[("authorization", "Bearer not-the-token")]),
    )
    .await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn wrong_scheme_is_401() {
    let res = send(
        Some(TEST_TOKEN),
        mcp_post(&[("authorization", &format!("Basic {TEST_TOKEN}"))]),
    )
    .await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn correct_token_reaches_handler() {
    let res = send(
        Some(TEST_TOKEN),
        mcp_post(&[("authorization", &format!("Bearer {TEST_TOKEN}"))]),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);

    let body = res.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["result"]["serverInfo"]["name"], "photonic");
}

#[tokio::test]
async fn bearer_scheme_is_case_insensitive() {
    let res = send(
        Some(TEST_TOKEN),
        mcp_post(&[("authorization", &format!("bearer {TEST_TOKEN}"))]),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn no_secret_configured_rejects_request() {
    let res = send(None, mcp_post(&[])).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

// ── §8 row 2: no CORS surface at all ────────────────────────────────────────

#[tokio::test]
async fn no_cors_headers_on_any_response() {
    let res = send(
        Some(TEST_TOKEN),
        mcp_post(&[
            ("authorization", &format!("Bearer {TEST_TOKEN}")),
            ("origin", "https://evil.example"),
        ]),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    for header in [
        "access-control-allow-origin",
        "access-control-allow-credentials",
    ] {
        assert!(
            res.headers().get(header).is_none(),
            "{header} must never be emitted"
        );
    }
}

#[tokio::test]
async fn preflight_is_not_answered() {
    // With no CORS layer, `OPTIONS /mcp` hits no route method and axum returns
    // 405 — the browser therefore blocks the real request.
    let req = Request::builder()
        .method("OPTIONS")
        .uri("/mcp")
        .header("origin", "https://evil.example")
        .header("access-control-request-method", "POST")
        .header("access-control-request-headers", "authorization")
        .body(Body::empty())
        .unwrap();

    let res = send(Some(TEST_TOKEN), req).await;
    assert!(
        !res.status().is_success(),
        "preflight must not be answered 2xx, got {}",
        res.status()
    );
    assert!(res.headers().get("access-control-allow-origin").is_none());
}
