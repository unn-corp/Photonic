use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

pub mod envelope;
pub use envelope::{
    Dialect, ProtocolMode, RpcError, DEFAULT_BODY_LIMIT, ERR_HEADER_MISMATCH, ERR_INVALID_PARAMS,
    ERR_INVALID_REQUEST, ERR_METHOD_NOT_FOUND, ERR_UNSUPPORTED_PROTOCOL, PROTOCOL_2024_11_05,
    PROTOCOL_2026_07_28,
};

// ─── JSON-RPC 2.0 envelope types ────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    pub params: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    pub fn success(id: Option<Value>, result: impl Serialize) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(serde_json::to_value(result).unwrap()),
            error: None,
        }
    }

    pub fn error(id: Option<Value>, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }

    pub fn error_with_data(
        id: Option<Value>,
        code: i32,
        message: impl Into<String>,
        data: Option<Value>,
    ) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data,
            }),
        }
    }

    pub fn from_rpc_error(id: Option<Value>, err: RpcError) -> Self {
        Self::error_with_data(id, err.code, err.message, err.data)
    }
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

// ─── MCP Initialize ──────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub protocol_version: String,
    pub capabilities: ServerCapabilities,
    pub server_info: ServerInfo,
}

#[derive(Debug, Serialize)]
pub struct ServerCapabilities {
    pub tools: ToolsCapability,
}

#[derive(Debug, Serialize)]
pub struct ToolsCapability {
    pub list_changed: bool,
}

#[derive(Debug, Serialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

pub(crate) mod args;
pub use args::*;

// ─── SSE Events ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DocumentEvent {
    NodeAdded { node_id: Uuid, layer_id: Uuid },
    NodeUpdated { node_id: Uuid },
    NodeRemoved { node_id: Uuid },
    LayerAdded { layer_id: Uuid },
    LayerRemoved { layer_id: Uuid },
    DocumentChanged,
    RenderComplete { frame_ms: f32, node_count: usize },
}

#[cfg(test)]
mod paint_tests {
    use super::*;

    /// #202: a bbox-units linear gradient resolves its 0–1 coords against the
    /// target node's bounding box into absolute user-space coords.
    #[test]
    fn bbox_gradient_resolves_to_node_box() {
        let g = FillArg::Gradient {
            gradient_type: Some("linear".into()),
            colors: vec!["#2f56cf".into(), "#7c3aed".into()],
            offsets: None,
            coords: Some(vec![0.0, 0.0, 1.0, 0.0]),
            units: Some("bbox".into()),
        };
        // Box at (100, 200) sized 50×80.
        let resolved = g.resolved_for_bbox((100.0, 200.0, 50.0, 80.0));
        match resolved {
            FillArg::Gradient { coords, units, .. } => {
                assert_eq!(coords, Some(vec![100.0, 200.0, 150.0, 200.0]));
                assert!(units.is_none(), "units should be cleared after resolution");
            }
            _ => panic!("expected a gradient"),
        }
    }

    /// A user-space (default) gradient is returned unchanged.
    #[test]
    fn userspace_gradient_is_untouched() {
        let g = FillArg::Gradient {
            gradient_type: Some("linear".into()),
            colors: vec!["#000".into(), "#fff".into()],
            offsets: None,
            coords: Some(vec![0.0, 0.0, 100.0, 0.0]),
            units: None,
        };
        let resolved = g.resolved_for_bbox((10.0, 10.0, 5.0, 5.0));
        match resolved {
            FillArg::Gradient { coords, .. } => {
                assert_eq!(coords, Some(vec![0.0, 0.0, 100.0, 0.0]));
            }
            _ => panic!("expected a gradient"),
        }
    }
}
