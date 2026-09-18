//! MCP `2026-07-28` request/response envelope helpers.
//!
//! Dual-mode servers also accept legacy `2024-11-05` initialize + bare calls
//! (no `_meta`). Strict mode requires per-request protocol metadata.

use serde_json::{json, Map, Value};

/// Primary protocol version this build advertises and prefers.
pub const PROTOCOL_2026_07_28: &str = "2026-07-28";
/// Legacy version still accepted under [`ProtocolMode::Dual`].
pub const PROTOCOL_2024_11_05: &str = "2024-11-05";

/// Reserved MCP error codes (2026-07-28).
pub const ERR_HEADER_MISMATCH: i32 = -32020;
pub const ERR_UNSUPPORTED_PROTOCOL: i32 = -32022;
pub const ERR_INVALID_PARAMS: i32 = -32602;
pub const ERR_INVALID_REQUEST: i32 = -32600;
pub const ERR_METHOD_NOT_FOUND: i32 = -32601;

/// How the server negotiates protocol versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProtocolMode {
    /// Accept 2026-07-28 and legacy initialize / bare tools/* calls.
    #[default]
    Dual,
    /// Only 2026-07-28 with required `_meta` fields.
    Strict,
}

impl ProtocolMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "dual" => Some(Self::Dual),
            "strict" => Some(Self::Strict),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dual => "dual",
            Self::Strict => "strict",
        }
    }
}

/// Effective dialect for one request after negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    V2026,
    Legacy,
}

/// Structured dispatch error (JSON-RPC code + message + optional data).
#[derive(Debug, Clone)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    pub data: Option<Value>,
    /// When set, HTTP status should be this (e.g. 400 for invalid params).
    pub http_status: Option<u16>,
}

impl RpcError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
            http_status: None,
        }
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    pub fn http(mut self, status: u16) -> Self {
        self.http_status = Some(status);
        self
    }
}

/// Photonic server identity for discover / response `_meta`.
pub fn server_info_value() -> Value {
    json!({
        "name": "photonic",
        "version": env!("CARGO_PKG_VERSION"),
    })
}

/// `server/discover` result body.
pub fn discover_result(mode: ProtocolMode) -> Value {
    let mut versions = vec![PROTOCOL_2026_07_28.to_string()];
    if mode == ProtocolMode::Dual {
        versions.push(PROTOCOL_2024_11_05.to_string());
    }
    json!({
        "resultType": "complete",
        "supportedVersions": versions,
        "capabilities": {
            "tools": { "listChanged": false }
        },
        "ttlMs": 3_600_000,
        "cacheScope": "public",
        "_meta": {
            "io.modelcontextprotocol/serverInfo": server_info_value(),
        }
    })
}

/// Wrap `tools/list` payload.
pub fn tools_list_result(tools: Value) -> Value {
    json!({
        "resultType": "complete",
        "tools": tools,
        "ttlMs": 300_000,
        "cacheScope": "public",
        "_meta": {
            "io.modelcontextprotocol/serverInfo": server_info_value(),
        }
    })
}

/// Wrap a serialized `ToolResult` with `resultType: complete`.
pub fn wrap_tool_call_result(tool_result: Value) -> Value {
    let mut map = Map::new();
    map.insert("resultType".into(), json!("complete"));
    if let Value::Object(obj) = tool_result {
        for (k, v) in obj {
            map.insert(k, v);
        }
    } else {
        map.insert("content".into(), tool_result);
    }
    map.insert(
        "_meta".into(),
        json!({
            "io.modelcontextprotocol/serverInfo": server_info_value(),
        }),
    );
    Value::Object(map)
}

/// Legacy initialize result (dual mode only).
pub fn legacy_initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_2024_11_05,
        "capabilities": {
            "tools": { "listChanged": false }
        },
        "serverInfo": {
            "name": "photonic",
            "version": env!("CARGO_PKG_VERSION"),
        }
    })
}

fn meta_object(params: &Value) -> Option<&Map<String, Value>> {
    params.get("_meta").and_then(|m| m.as_object())
}

fn meta_str<'a>(meta: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    meta.get(key).and_then(|v| v.as_str())
}

/// Negotiate dialect from params `_meta` and server mode.
pub fn negotiate_dialect(mode: ProtocolMode, params: &Value) -> Result<Dialect, RpcError> {
    let meta = meta_object(params);
    let version = meta.and_then(|m| {
        meta_str(m, "io.modelcontextprotocol/protocolVersion")
            .or_else(|| meta_str(m, "protocolVersion"))
    });

    match mode {
        ProtocolMode::Strict => {
            let Some(v) = version else {
                return Err(RpcError::new(
                    ERR_INVALID_PARAMS,
                    "missing params._meta.io.modelcontextprotocol/protocolVersion",
                )
                .http(400));
            };
            if v != PROTOCOL_2026_07_28 {
                return Err(unsupported_version(mode, v));
            }
            let caps = meta.and_then(|m| m.get("io.modelcontextprotocol/clientCapabilities"));
            if caps.is_none() {
                return Err(RpcError::new(
                    ERR_INVALID_PARAMS,
                    "missing params._meta.io.modelcontextprotocol/clientCapabilities",
                )
                .http(400));
            }
            Ok(Dialect::V2026)
        }
        ProtocolMode::Dual => match version {
            None => Ok(Dialect::Legacy),
            Some(v) if v == PROTOCOL_2026_07_28 => Ok(Dialect::V2026),
            Some(v) if v == PROTOCOL_2024_11_05 => Ok(Dialect::Legacy),
            Some(v) => Err(unsupported_version(mode, v)),
        },
    }
}

fn unsupported_version(mode: ProtocolMode, got: &str) -> RpcError {
    let mut versions = vec![PROTOCOL_2026_07_28];
    if mode == ProtocolMode::Dual {
        versions.push(PROTOCOL_2024_11_05);
    }
    RpcError::new(
        ERR_UNSUPPORTED_PROTOCOL,
        format!("unsupported protocol version: {got}"),
    )
    .with_data(json!({ "supportedVersions": versions }))
    .http(400)
}

/// Validate Streamable HTTP routing headers against the JSON-RPC body.
pub fn validate_mcp_headers(
    mode: ProtocolMode,
    header_method: Option<&str>,
    header_name: Option<&str>,
    rpc_method: &str,
    tool_name: Option<&str>,
) -> Result<(), RpcError> {
    match mode {
        ProtocolMode::Strict => {
            let Some(hm) = header_method.filter(|s| !s.is_empty()) else {
                return Err(
                    RpcError::new(ERR_HEADER_MISMATCH, "missing Mcp-Method header").http(400),
                );
            };
            if hm != rpc_method {
                return Err(header_mismatch("Mcp-Method", hm, rpc_method));
            }
            if rpc_method == "tools/call" {
                let expected = tool_name.unwrap_or("");
                let Some(hn) = header_name.filter(|s| !s.is_empty()) else {
                    return Err(RpcError::new(
                        ERR_HEADER_MISMATCH,
                        "missing Mcp-Name header for tools/call",
                    )
                    .http(400));
                };
                if hn != expected {
                    return Err(header_mismatch("Mcp-Name", hn, expected));
                }
            }
            Ok(())
        }
        ProtocolMode::Dual => {
            if let Some(hm) = header_method.filter(|s| !s.is_empty()) {
                if hm != rpc_method {
                    return Err(header_mismatch("Mcp-Method", hm, rpc_method));
                }
            }
            if let Some(hn) = header_name.filter(|s| !s.is_empty()) {
                if rpc_method == "tools/call" {
                    let expected = tool_name.unwrap_or("");
                    if hn != expected {
                        return Err(header_mismatch("Mcp-Name", hn, expected));
                    }
                }
            }
            Ok(())
        }
    }
}

fn header_mismatch(which: &str, got: &str, expected: &str) -> RpcError {
    RpcError::new(
        ERR_HEADER_MISMATCH,
        format!("{which} header mismatch: got {got:?}, body expects {expected:?}"),
    )
    .with_data(json!({
        "header": which,
        "got": got,
        "expected": expected,
    }))
    .http(400)
}

/// Default max JSON-RPC body size (8 MiB).
pub const DEFAULT_BODY_LIMIT: usize = 8 * 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dual_missing_meta_is_legacy() {
        let d = negotiate_dialect(ProtocolMode::Dual, &json!({})).unwrap();
        assert_eq!(d, Dialect::Legacy);
    }

    #[test]
    fn strict_missing_meta_errors() {
        let e = negotiate_dialect(ProtocolMode::Strict, &json!({})).unwrap_err();
        assert_eq!(e.code, ERR_INVALID_PARAMS);
    }

    #[test]
    fn dual_2026_meta_ok() {
        let params = json!({
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {}
            }
        });
        assert_eq!(
            negotiate_dialect(ProtocolMode::Dual, &params).unwrap(),
            Dialect::V2026
        );
    }

    #[test]
    fn unsupported_version_lists_supported() {
        let params = json!({
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "1999-01-01",
                "io.modelcontextprotocol/clientCapabilities": {}
            }
        });
        let e = negotiate_dialect(ProtocolMode::Dual, &params).unwrap_err();
        assert_eq!(e.code, ERR_UNSUPPORTED_PROTOCOL);
        let data = e.data.unwrap();
        assert!(data["supportedVersions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str() == Some(PROTOCOL_2026_07_28)));
    }

    #[test]
    fn dual_headers_optional_but_must_match_when_present() {
        validate_mcp_headers(ProtocolMode::Dual, None, None, "tools/list", None).unwrap();
        validate_mcp_headers(
            ProtocolMode::Dual,
            Some("tools/list"),
            None,
            "tools/list",
            None,
        )
        .unwrap();
        let e = validate_mcp_headers(
            ProtocolMode::Dual,
            Some("tools/call"),
            None,
            "tools/list",
            None,
        )
        .unwrap_err();
        assert_eq!(e.code, ERR_HEADER_MISMATCH);
    }

    #[test]
    fn strict_requires_method_header() {
        let e =
            validate_mcp_headers(ProtocolMode::Strict, None, None, "tools/list", None).unwrap_err();
        assert_eq!(e.code, ERR_HEADER_MISMATCH);
    }

    #[test]
    fn wrap_tool_preserves_is_error() {
        let raw = json!({ "content": [{"type":"text","text":"x"}], "isError": true });
        let w = wrap_tool_call_result(raw);
        assert_eq!(w["resultType"], "complete");
        assert_eq!(w["isError"], true);
        assert!(w["_meta"]["io.modelcontextprotocol/serverInfo"]["name"] == "photonic");
    }

    #[test]
    fn discover_dual_lists_both_versions() {
        let d = discover_result(ProtocolMode::Dual);
        let vers = d["supportedVersions"].as_array().unwrap();
        assert_eq!(vers.len(), 2);
        assert_eq!(d["resultType"], "complete");
    }
}
