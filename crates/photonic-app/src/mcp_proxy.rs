//! Stdio → HTTP MCP proxy (attach mode).
//!
//! Speaks Content-Length framed JSON-RPC on stdin/stdout (MCP SDK style),
//! with a dual-mode fallback that also accepts NDJSON lines on stdin.
//! Forwards each request to a running Photonic HTTP MCP server with:
//! - `Authorization: Bearer <token>` from `PHOTONIC_MCP_TOKEN` or the session
//!   token file
//! - `Mcp-Method` / `Mcp-Name` derived from the JSON-RPC body
//!
//! This is the attach path for MCPB / Claude Desktop: host spawns this proxy
//! (or `photonic mcp-proxy`) which talks to the already-running GUI.

use anyhow::{bail, Context, Result};
use photonic_mcp::{
    stdio::{read_message, write_json_value},
    MCP_SECRET_HEADER,
};
use serde_json::Value;
use std::io::{BufReader, Write};

pub fn run(host_port: &str, secret: Option<&str>) -> Result<()> {
    let secret = require_secret(secret)?;
    let url = format!("http://{host_port}/mcp");
    let token = std::env::var("PHOTONIC_MCP_TOKEN")
        .ok()
        .or_else(|| photonic_mcp::auth::read_token().ok());

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()?;

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut out = stdout.lock();

    // Emit mode line on stderr for attach diagnostics (never the token).
    eprintln!(
        "photonic mcp-proxy: attach -> {url} (bearer={})",
        if token.is_some() { "yes" } else { "no" }
    );

    loop {
        let Some(line) = read_message(&mut reader).context("read stdio message")? else {
            break;
        };
        if line.trim().is_empty() {
            continue;
        }

        let parsed: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let err = serde_json::json!({
                    "jsonrpc": "2.0",
                    "error": { "code": -32700, "message": format!("Parse error: {e}") }
                });
                write_json_value(&mut out, &err)?;
                continue;
            }
        };

        let is_notification = !parsed
            .as_object()
            .map(|o| o.contains_key("id") && !o["id"].is_null())
            .unwrap_or(false);

        let method = parsed.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let tool_name = parsed.pointer("/params/name").and_then(|n| n.as_str());

        let mut req = client
            .post(&url)
            .header("content-type", "application/json")
            .header(MCP_SECRET_HEADER, secret)
            .header("Mcp-Method", method);
        if let Some(name) = tool_name {
            req = req.header("Mcp-Name", name);
        }
        if let Some(ref tok) = token {
            req = req.header("authorization", format!("Bearer {tok}"));
        }

        let body = match req.body(line).send() {
            Ok(resp) => {
                let status = resp.status();
                let text = resp.text().unwrap_or_default();
                if status.as_u16() == 401 {
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "error": {
                            "code": -32000,
                            "message": "MCP proxy: 401 Unauthorized — set PHOTONIC_MCP_TOKEN or ensure mcp_token file exists"
                        }
                    })
                    .to_string()
                } else if text.is_empty() {
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "error": {
                            "code": -32603,
                            "message": format!("MCP proxy: empty body (HTTP {status})")
                        }
                    })
                    .to_string()
                } else {
                    text
                }
            }
            Err(e) => serde_json::json!({
                "jsonrpc": "2.0",
                "error": { "code": -32603, "message": format!("Proxy error: {e}") }
            })
            .to_string(),
        };

        if !is_notification {
            // Re-frame as Content-Length (hosts expect that on stdio).
            if let Ok(v) = serde_json::from_str::<Value>(&body) {
                write_json_value(&mut out, &v)?;
            } else {
                // Pass through raw if not JSON (shouldn't happen).
                writeln!(out, "{body}")?;
                out.flush()?;
            }
        }
    }

    Ok(())
}

fn require_secret(secret: Option<&str>) -> Result<&str> {
    let Some(secret) = secret.filter(|secret| !secret.trim().is_empty()) else {
        bail!("MCP authentication requires --mcp-secret or PHOTONIC_MCP_SECRET");
    };
    if secret.bytes().any(|byte| matches!(byte, b'\r' | b'\n')) {
        bail!("MCP secret cannot contain CR or LF characters");
    }
    Ok(secret)
}
