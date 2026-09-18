use crate::dispatch;
use crate::handlers;
use crate::handlers::clipboard::ClipboardRing;
use crate::protocol::envelope::{self, Dialect, ProtocolMode, RpcError};
use crate::protocol::*;
pub use crate::schema_gen::tool_list;
use axum::{
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use photonic_core::{document::Document, history::CommandHistory, AuditLog, PathPolicy};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use subtle::ConstantTimeEq;
use tokio::sync::{oneshot, Mutex};
use tracing::info;

/// Header used to authenticate requests when `McpServerConfig::secret` is set.
pub const MCP_SECRET_HEADER: &str = "x-mcp-secret";

/// Maximum encoded JSON-RPC request size accepted by the MCP endpoint.
const MCP_REQUEST_BODY_LIMIT: usize = 2 * 1024 * 1024;

/// Configuration for the MCP server.
#[derive(Debug, Clone)]
pub struct McpServerConfig {
    pub port: u16,
    /// Shared secret expected in the [`MCP_SECRET_HEADER`] header, when set.
    pub secret: Option<String>,
    /// Protocol negotiation policy (`dual` default — see `ProtocolMode`).
    pub protocol_mode: crate::protocol::ProtocolMode,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            port: 7842,
            secret: None,
            protocol_mode: crate::protocol::ProtocolMode::Dual,
        }
    }
}

/// Wraps a handler result with its mutation intent so `dispatch_tool` can
/// decide whether to schedule an auto-checkpoint without consulting a
/// separate list.
pub(crate) struct ToolOutput {
    pub(crate) result: ToolResult,
    pub(crate) mutates: bool,
}

impl ToolOutput {
    pub(crate) fn mutating(result: ToolResult) -> Self {
        Self {
            result,
            mutates: true,
        }
    }
    pub(crate) fn readonly(result: ToolResult) -> Self {
        Self {
            result,
            mutates: false,
        }
    }
}

/// Shared application state injected into all axum handlers.
#[derive(Clone)]
pub struct AppState {
    pub document: Arc<Mutex<Document>>,
    pub history: Arc<Mutex<CommandHistory>>,
    /// Last native document path used by MCP save/save-as.
    pub document_path: Arc<StdMutex<Option<PathBuf>>>,
    /// Sends screenshot requests to the render thread.
    /// Uses std::sync::mpsc so the render thread can poll synchronously.
    pub capture_tx: Arc<StdMutex<std::sync::mpsc::Sender<oneshot::Sender<Vec<u8>>>>>,
    pub config: McpServerConfig,
    /// Path containment for MCP filesystem args (28 §3).
    pub path_policy: PathPolicy,
    /// In-memory audit log of every MCP tool call. Shared with the GUI Audit panel
    /// via a std::sync::Mutex so the sync GUI thread can read it without async.
    pub audit_log: Arc<StdMutex<AuditLog>>,
    /// Session-scoped clipboard ring — stores up to 20 copied node snapshots.
    /// Uses StdMutex so the GUI thread can also read it without async.
    pub clipboard_ring: Arc<StdMutex<ClipboardRing>>,
    /// Video engine slot (10-mcp-tools.md §2, lazy-headless variant): the
    /// engine + session are created on the first engine-backed tool call via
    /// an own headless adapter, so both run modes work with no `main.rs`
    /// wiring (sharing the GUI's winit `GpuContext` is the app-integration
    /// story's seam — see `handlers/video_jobs.rs` module docs).
    pub video_engine: Arc<handlers::video_jobs::VideoEngineHandle>,
    /// Async-job registry (10 §6): export/probe/transcode workers publish
    /// status here; `get_job_status`/`cancel_job` poll it; the background
    /// checkpoint-flush task GCs terminal entries after 10 minutes.
    pub video_jobs: Arc<StdMutex<handlers::video_jobs::JobRegistry>>,
}

impl AppState {
    /// Builds a fully in-memory `AppState` for out-of-crate integration tests
    /// (29 §3 / CAP-019 acceptance-story harness) so callers need not hand-roll
    /// all nine fields. Shape mirrors `mcp_parity.rs`'s `test_state()`:
    /// a fresh 1920×1080 document, a 200-entry history, no document path,
    /// `McpServerConfig::default()` (no bearer secret → the transport auth
    /// layer is open, but the harness bypasses the transport anyway), an empty
    /// audit log, and freshly constructed clipboard/video registries.
    ///
    /// The `capture_tx` receiver is deliberately leaked: no screenshot is ever
    /// requested in a headless test, and leaking keeps the channel from
    /// reporting a disconnected receiver if some path ever does send.
    pub fn headless_for_test() -> Self {
        let (capture_tx, capture_rx) = std::sync::mpsc::channel();
        std::mem::forget(capture_rx);
        AppState {
            document: Arc::new(Mutex::new(Document::new("t", 1920.0, 1080.0))),
            history: Arc::new(Mutex::new(CommandHistory::new(200))),
            document_path: Arc::new(StdMutex::new(None)),
            capture_tx: Arc::new(StdMutex::new(capture_tx)),
            config: McpServerConfig::default(),
            path_policy: PathPolicy::desktop_default(),
            audit_log: Arc::new(StdMutex::new(AuditLog::new())),
            clipboard_ring: Arc::new(handlers::clipboard::new_clipboard_ring()),
            video_engine: Arc::new(handlers::video_jobs::VideoEngineHandle::new()),
            video_jobs: Arc::new(StdMutex::new(handlers::video_jobs::JobRegistry::new())),
        }
    }
}

/// The MCP server — wraps axum and owns shared state.
pub struct McpServer {
    pub state: AppState,
    /// Set to `true` once the server is successfully bound and listening.
    pub running: Arc<AtomicBool>,
}

impl McpServer {
    pub fn new(
        document: Arc<Mutex<Document>>,
        history: Arc<Mutex<CommandHistory>>,
        capture_tx: std::sync::mpsc::Sender<oneshot::Sender<Vec<u8>>>,
        config: McpServerConfig,
        running: Arc<AtomicBool>,
        audit_log: Arc<StdMutex<AuditLog>>,
    ) -> Self {
        Self {
            state: AppState {
                document,
                history,
                document_path: Arc::new(StdMutex::new(None)),
                capture_tx: Arc::new(StdMutex::new(capture_tx)),
                config,
                path_policy: PathPolicy::desktop_default(),
                audit_log,
                clipboard_ring: Arc::new(handlers::clipboard::new_clipboard_ring()),
                video_engine: Arc::new(handlers::video_jobs::VideoEngineHandle::new()),
                video_jobs: Arc::new(StdMutex::new(handlers::video_jobs::JobRegistry::new())),
            },
            running,
        }
    }

    /// Share the host application's current native document path with MCP so a
    /// pathless `save_document` has the same save target as an opened file.
    pub fn with_document_path(mut self, document_path: Arc<StdMutex<Option<PathBuf>>>) -> Self {
        self.state.document_path = document_path;
        self
    }

    /// Start listening. This blocks the current task.
    pub async fn run(self) -> anyhow::Result<()> {
        if usable_secret(self.state.config.secret.as_deref()).is_none() {
            anyhow::bail!(
                "MCP server requires a non-empty shared secret; pass --mcp-secret or set PHOTONIC_MCP_SECRET"
            );
        }

        let port = self.state.config.port;

        // Background task: flush debounced MCP checkpoint every 10 s.
        // A checkpoint is actually written once 60 s have elapsed with no new
        // mutations — so rapid tool calls coalesce into a single snapshot.
        let bg_state = self.state.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
            loop {
                interval.tick().await;
                {
                    let doc = bg_state.document.lock().await;
                    let mut history = bg_state.history.lock().await;
                    history.tick_mcp_checkpoint(&doc);
                }
                // Second timer arm (10 §6): evict terminal video jobs after
                // the 10-minute retention window.
                if let Ok(mut jobs) = bg_state.video_jobs.lock() {
                    jobs.gc(handlers::video_jobs::JOB_RETENTION);
                }
            }
        });

        let router = build_router(self.state);
        let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{}", port)).await?;
        // Mark the server as successfully bound before blocking on serve.
        self.running.store(true, Ordering::Relaxed);
        info!("Photonic MCP server listening on http://127.0.0.1:{}", port);
        axum::serve(listener, router).await?;
        Ok(())
    }
}

/// Builds the MCP router.
///
/// Deliberately carries NO CORS layer (28 §4 point 1): the server has no
/// legitimate browser client, so it emits no `access-control-allow-origin`
/// and answers no preflight. Combined with the configured-secret requirement below —
/// which forces a preflight for any cross-origin caller — that closes the
/// "any page you visit can drive your editor" vector. Neither half suffices
/// alone.
///
/// `pub` so integration tests can drive it without binding a socket.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/mcp", post(handle_mcp))
        // Authenticate the request metadata before any body extractor runs.
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_authentication,
        ))
        .layer(DefaultBodyLimit::max(MCP_REQUEST_BODY_LIMIT))
        .with_state(state)
}

/// Reject every request that does not present the configured session secret.
/// Authentication runs before JSON body extraction, so unauthenticated callers
/// cannot probe the JSON-RPC parser or force large request allocations.
async fn require_authentication(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if !is_authorized(request.headers(), state.config.secret.as_deref()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    next.run(request).await
}

fn is_authorized(headers: &HeaderMap, secret: Option<&str>) -> bool {
    let Some(secret) = usable_secret(secret) else {
        return false;
    };

    let header_secret = headers
        .get(MCP_SECRET_HEADER)
        .and_then(|value| value.to_str().ok());
    let bearer_secret = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split_once(' '))
        // The scheme is case-insensitive per RFC 7235; the token is not.
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("Bearer"))
        .map(|(_, token)| token);

    [header_secret, bearer_secret]
        .into_iter()
        .flatten()
        .any(|provided| bool::from(secret.as_bytes().ct_eq(provided.as_bytes())))
}

fn usable_secret(secret: Option<&str>) -> Option<&str> {
    secret.filter(|secret| !secret.trim().is_empty())
}

/// Main MCP JSON-RPC handler (HTTP).
async fn handle_mcp(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<JsonRpcRequest>,
) -> Response {
    let header_method = headers
        .get("mcp-method")
        .or_else(|| headers.get("Mcp-Method"))
        .and_then(|v| v.to_str().ok());
    let header_name = headers
        .get("mcp-name")
        .or_else(|| headers.get("Mcp-Name"))
        .and_then(|v| v.to_str().ok());
    let is_notification = req.id.is_none();
    match process_rpc_request(state, req, header_method, header_name).await {
        Ok(resp) if is_notification => {
            let _ = resp;
            StatusCode::NO_CONTENT.into_response()
        }
        Err((_, resp)) if is_notification => {
            let _ = resp;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(resp) => Json(resp).into_response(),
        Err((status, resp)) => (status, Json(resp)).into_response(),
    }
}

/// Shared JSON-RPC entry used by HTTP and stdio transports.
///
/// Returns `Ok(response)` for JSON-RPC success/error payloads that should use
/// HTTP 200, or `Err((status, response))` when the protocol demands a non-200
/// status (invalid params / header mismatch / etc.).
pub async fn process_rpc_request(
    state: AppState,
    req: JsonRpcRequest,
    header_method: Option<&str>,
    header_name: Option<&str>,
) -> Result<JsonRpcResponse, (axum::http::StatusCode, JsonRpcResponse)> {
    if req.jsonrpc != "2.0" {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            JsonRpcResponse::error(
                req.id,
                crate::protocol::ERR_INVALID_REQUEST,
                "Invalid JSON-RPC version",
            ),
        ));
    }

    let mode = state.config.protocol_mode;
    let params = req.params.clone().unwrap_or_else(|| json!({}));
    let tool_name = if req.method == "tools/call" {
        params.get("name").and_then(|v| v.as_str())
    } else {
        None
    };

    if let Err(e) =
        envelope::validate_mcp_headers(mode, header_method, header_name, &req.method, tool_name)
    {
        return Err(rpc_to_http(req.id, e));
    }

    if req.method == "initialize" {
        return match mode {
            ProtocolMode::Dual => Ok(JsonRpcResponse::success(
                req.id,
                envelope::legacy_initialize_result(),
            )),
            ProtocolMode::Strict => Err(rpc_to_http(
                req.id,
                RpcError::new(
                    crate::protocol::ERR_METHOD_NOT_FOUND,
                    "initialize removed in 2026-07-28; use server/discover",
                )
                .http(400),
            )),
        };
    }
    if req.method == "notifications/initialized" {
        return match mode {
            ProtocolMode::Dual => Ok(JsonRpcResponse::success(req.id, json!({ "status": "ok" }))),
            ProtocolMode::Strict => Err(rpc_to_http(
                req.id,
                RpcError::new(
                    crate::protocol::ERR_METHOD_NOT_FOUND,
                    "notifications/initialized is not used in 2026-07-28",
                )
                .http(400),
            )),
        };
    }

    let dialect = match envelope::negotiate_dialect(mode, &params) {
        Ok(d) => d,
        Err(e) => return Err(rpc_to_http(req.id, e)),
    };

    match dispatch_method(state, &req.method, params, dialect).await {
        Ok(value) => Ok(JsonRpcResponse::success(req.id, value)),
        Err(e) => {
            if e.http_status.is_some() {
                Err(rpc_to_http(req.id, e))
            } else {
                Ok(JsonRpcResponse::from_rpc_error(req.id, e))
            }
        }
    }
}

fn rpc_to_http(id: Option<Value>, err: RpcError) -> (axum::http::StatusCode, JsonRpcResponse) {
    let status = axum::http::StatusCode::from_u16(err.http_status.unwrap_or(400))
        .unwrap_or(axum::http::StatusCode::BAD_REQUEST);
    (status, JsonRpcResponse::from_rpc_error(id, err))
}

async fn dispatch_method(
    state: AppState,
    method: &str,
    params: Value,
    dialect: Dialect,
) -> Result<Value, RpcError> {
    match method {
        "server/discover" => Ok(envelope::discover_result(state.config.protocol_mode)),

        "tools/list" => {
            let _ = dialect;
            // Compact listing (Pattern B): promoted + search/execute. Full list via
            // params.full=true or PHOTONIC_MCP_FULL_TOOLS=1.
            let want_full = params
                .get("full")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
                || std::env::var("PHOTONIC_MCP_FULL_TOOLS")
                    .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                    .unwrap_or(false);
            let tools = if want_full {
                tool_list()
            } else {
                crate::catalog::compact_tool_list()
            };
            Ok(envelope::tools_list_result(tools))
        }

        "tools/call" => {
            let tool_name = params
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    RpcError::new(crate::protocol::ERR_INVALID_PARAMS, "Missing tool name")
                        .http(400)
                })?
                .to_string();
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            let tool_result = dispatch::dispatch_tool(&state, &tool_name, args)
                .await
                .map_err(|msg| RpcError::new(-32000, msg))?;
            let raw = serde_json::to_value(tool_result).unwrap_or(json!({}));
            Ok(envelope::wrap_tool_call_result(raw))
        }

        other => Err(RpcError::new(
            crate::protocol::ERR_METHOD_NOT_FOUND,
            format!("Unknown method: {other}"),
        )),
    }
}

pub(crate) use crate::dispatch::dispatch_tool_inner;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::clipboard::new_clipboard_ring;
    use axum::{
        body::{to_bytes, Body},
        http::{header, Method, Request},
    };
    use photonic_core::{history::CommandHistory, AuditLog, Document};
    use serde_json::json;
    use std::sync::{mpsc, Arc, Mutex as StdMutex};
    use tokio::sync::Mutex;
    use tower::util::ServiceExt;

    fn test_state(secret: Option<&str>) -> AppState {
        let (tx, _rx) = mpsc::channel();
        AppState {
            document: Arc::new(Mutex::new(Document::new("auth test", 200.0, 100.0))),
            history: Arc::new(Mutex::new(CommandHistory::new(100))),
            document_path: Arc::new(StdMutex::new(None)),
            capture_tx: Arc::new(StdMutex::new(tx)),
            config: McpServerConfig {
                port: 7842,
                secret: secret.map(str::to_owned),
                protocol_mode: ProtocolMode::Dual,
            },
            path_policy: PathPolicy::test_default(),
            audit_log: Arc::new(StdMutex::new(AuditLog::new())),
            clipboard_ring: Arc::new(new_clipboard_ring()),
            video_engine: Arc::new(handlers::video_jobs::VideoEngineHandle::new()),
            video_jobs: Arc::new(StdMutex::new(handlers::video_jobs::JobRegistry::new())),
        }
    }

    fn save_request(path: &std::path::Path, secret: Option<&str>) -> axum::http::Request<Body> {
        let mut builder = axum::http::Request::builder()
            .method(Method::POST)
            .uri("/mcp")
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(secret) = secret {
            builder = builder.header(MCP_SECRET_HEADER, secret);
        }
        builder
            .body(Body::from(
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/call",
                    "params": {
                        "name": "save_document",
                        "arguments": { "path": path }
                    }
                })
                .to_string(),
            ))
            .unwrap()
    }

    #[tokio::test]
    async fn configured_secret_rejects_missing_and_incorrect_headers_before_dispatch() {
        let base = std::env::temp_dir().join(format!("photonic-mcp-auth-{}", uuid::Uuid::new_v4()));
        let path = base.join("missing.photon");
        let state = test_state(Some("correct-secret"));
        let app = build_router(state);

        let response = app
            .clone()
            .oneshot(save_request(&path, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(!path.exists(), "unauthenticated request must not dispatch");

        let response = app
            .clone()
            .oneshot(save_request(&path, Some("wrong-secret")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(!path.exists(), "incorrect secret must not dispatch");

        let response = app
            .oneshot(save_request(&path, Some("correct-secret")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(path.exists(), "authenticated request should dispatch");

        std::fs::remove_dir_all(base).unwrap();
    }

    #[tokio::test]
    async fn unauthenticated_oversized_body_is_rejected_before_body_extraction() {
        let app = build_router(test_state(Some("correct-secret")));
        let oversized_body = vec![b' '; MCP_REQUEST_BODY_LIMIT + 1];
        let request = axum::http::Request::builder()
            .method(Method::POST)
            .uri("/mcp")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(oversized_body))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn authenticated_oversized_body_is_capped() {
        let app = build_router(test_state(Some("correct-secret")));
        let oversized_body = vec![b' '; MCP_REQUEST_BODY_LIMIT + 1];
        let request = axum::http::Request::builder()
            .method(Method::POST)
            .uri("/mcp")
            .header(header::CONTENT_TYPE, "application/json")
            .header(MCP_SECRET_HEADER, "correct-secret")
            .body(Body::from(oversized_body))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn missing_secret_rejects_before_dispatch() {
        let base =
            std::env::temp_dir().join(format!("photonic-mcp-no-auth-{}", uuid::Uuid::new_v4()));
        let path = base.join("rejected.photon");
        let app = build_router(test_state(None));

        let response = app.oneshot(save_request(&path, None)).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(!path.exists(), "missing server secret must not dispatch");

        let _ = std::fs::remove_dir_all(base);
    }

    async fn post_json(request: Value) -> (StatusCode, Vec<u8>) {
        let response = build_router(test_state(Some("test-secret")))
            .oneshot(
                Request::post("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(MCP_SECRET_HEADER, "test-secret")
                    .body(Body::from(request.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, body.to_vec())
    }

    #[tokio::test]
    async fn initialized_notification_returns_no_content() {
        let (status, body) = post_json(json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }))
        .await;

        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn notification_errors_emit_no_response_bytes() {
        let (status, body) = post_json(json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled"
        }))
        .await;

        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn request_returns_id_matched_json_rpc_response() {
        let (status, body) = post_json(json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "initialize"
        }))
        .await;

        assert_eq!(status, StatusCode::OK);
        let response: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 7);
        assert!(response["result"].is_object());
        assert!(response.get("error").is_none());
    }

    #[tokio::test]
    async fn mcp_endpoint_does_not_enable_cross_origin_requests() {
        let app = build_router(test_state(Some("test-secret")));
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::OPTIONS)
                    .uri("/mcp")
                    .header(header::ORIGIN, "https://evil.example")
                    .header(MCP_SECRET_HEADER, "test-secret")
                    .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                    .header(
                        header::ACCESS_CONTROL_REQUEST_HEADERS,
                        "content-type, x-mcp-secret",
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert!(!response
            .headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN));
    }
}
