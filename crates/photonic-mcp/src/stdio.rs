//! Content-Length framed stdio MCP transport (MCP SDK / Streamable stdio style).
//!
//! Hosts (Claude Desktop MCPB, Inspector) typically speak:
//! ```text
//! Content-Length: <n>\r\n\r\n<body>
//! ```
//! Dual-mode also accepts newline-delimited JSON (legacy `mcp_proxy` clients).

use crate::protocol::{JsonRpcRequest, JsonRpcResponse};
use crate::server::{process_rpc_request, AppState};
use serde_json::Value;
use std::io::{self, BufRead, BufReader, Write};
use tracing::{debug, warn};

/// Run the MCP server on stdin/stdout until EOF.
pub async fn run_stdio(state: AppState) -> io::Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();

    loop {
        let Some(raw) = read_message(&mut reader)? else {
            debug!("stdio MCP: EOF");
            break;
        };
        if raw.trim().is_empty() {
            continue;
        }

        let req: JsonRpcRequest = match serde_json::from_str(&raw) {
            Ok(r) => r,
            Err(e) => {
                warn!("stdio MCP: invalid JSON-RPC: {e}");
                let err = JsonRpcResponse::error(None, -32700, format!("Parse error: {e}"));
                write_message(&mut writer, &err)?;
                continue;
            }
        };

        // Notifications: process but do not reply if id is null/absent.
        let is_notification =
            req.id.is_none() || req.id.as_ref().map(|id| id.is_null()).unwrap_or(false);

        // Derive routing headers from body for dual/strict checks (stdio has no HTTP headers).
        let method = req.method.clone();
        let tool_name = req
            .params
            .as_ref()
            .and_then(|p| p.get("name"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // For dual mode, headers are optional — pass body-derived values so strict mode works.
        let result = process_rpc_request(
            state.clone(),
            req,
            Some(method.as_str()),
            tool_name.as_deref(),
        )
        .await;

        if is_notification {
            continue;
        }

        let response = match result {
            Ok(r) => r,
            Err((_status, r)) => r,
        };
        write_message(&mut writer, &response)?;
    }
    Ok(())
}

/// Read one JSON-RPC message: Content-Length framed, or a single NDJSON line.
pub fn read_message(reader: &mut impl BufRead) -> io::Result<Option<String>> {
    let mut first_line = String::new();

    // Peek first non-empty line to decide framing.
    loop {
        first_line.clear();
        let n = reader.read_line(&mut first_line)?;
        if n == 0 {
            return Ok(None);
        }
        if !first_line.trim().is_empty() {
            break;
        }
    }

    let trimmed = first_line.trim_end_matches(['\r', '\n']);
    let mut content_length = if let Some(rest) = trimmed
        .strip_prefix("Content-Length:")
        .or_else(|| trimmed.strip_prefix("content-length:"))
    {
        rest.trim().parse().map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidData, format!("Content-Length: {e}"))
        })?
    } else if trimmed.starts_with('{') {
        // NDJSON legacy: one JSON object per line.
        return Ok(Some(trimmed.to_string()));
    } else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("expected Content-Length header or JSON line, got: {trimmed:?}"),
        ));
    };

    // Consume remaining headers until blank line.
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Ok(None);
        }
        let t = line.trim_end_matches(['\r', '\n']);
        if t.is_empty() {
            break;
        }
        if let Some(rest) = t
            .strip_prefix("Content-Length:")
            .or_else(|| t.strip_prefix("content-length:"))
        {
            content_length = rest.trim().parse().map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidData, format!("Content-Length: {e}"))
            })?;
        }
    }

    let mut buf = vec![0u8; content_length];
    reader.read_exact(&mut buf)?;
    let s = String::from_utf8(buf)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("utf-8: {e}")))?;
    Ok(Some(s))
}

/// Write a JSON-RPC response with Content-Length framing.
pub fn write_message(writer: &mut impl Write, response: &JsonRpcResponse) -> io::Result<()> {
    let body = serde_json::to_string(response)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    write!(writer, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
    writer.flush()?;
    Ok(())
}

/// Write raw JSON value (for tests).
pub fn write_json_value(writer: &mut impl Write, value: &Value) -> io::Result<()> {
    let body =
        serde_json::to_string(value).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    write!(writer, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn read_content_length_frame() {
        let payload = r#"{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{}}"#;
        let raw = format!("Content-Length: {}\r\n\r\n{}", payload.len(), payload);
        let mut cur = Cursor::new(raw.into_bytes());
        let msg = read_message(&mut cur).unwrap().unwrap();
        assert_eq!(msg, payload);
    }

    #[test]
    fn read_ndjson_line() {
        let payload = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let raw = format!("{payload}\n");
        let mut cur = Cursor::new(raw.into_bytes());
        let msg = read_message(&mut cur).unwrap().unwrap();
        assert_eq!(msg, payload);
    }

    #[test]
    fn write_roundtrip_frame() {
        let resp =
            JsonRpcResponse::success(Some(serde_json::json!(1)), serde_json::json!({"ok": true}));
        let mut buf = Vec::new();
        write_message(&mut buf, &resp).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.starts_with("Content-Length: "));
        assert!(s.contains("\r\n\r\n"));
        let mut cur = Cursor::new(s.into_bytes());
        let msg = read_message(&mut cur).unwrap().unwrap();
        let v: Value = serde_json::from_str(&msg).unwrap();
        assert_eq!(v["result"]["ok"], true);
    }
}
