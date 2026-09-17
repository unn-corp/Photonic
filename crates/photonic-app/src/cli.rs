//! CLI client — talks to a running Photonic instance via MCP JSON-RPC over HTTP.
//!
//! Uses only std (TcpStream + HTTP/1.0), no extra HTTP-client dependencies.

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose, Engine as _};
use photonic_mcp::MCP_SECRET_HEADER;
use serde_json::{json, Value};
use std::path::PathBuf;

use crate::args::CliCommand;

// ─── Public entry point ───────────────────────────────────────────────────────

pub fn run(server: &str, secret: Option<&str>, command: CliCommand) -> Result<()> {
    let host_port = parse_host_port(server);
    match command {
        CliCommand::McpProxy => crate::mcp_proxy::run(&host_port, secret),
        CliCommand::Run { script } => crate::script::run_script(&script),
        CliCommand::Status => cmd_status(&host_port, secret),
        CliCommand::List => cmd_list(&host_port, secret),
        CliCommand::Screenshot { output } => cmd_screenshot(&host_port, secret, &output),
        CliCommand::Clear => cmd_clear(&host_port, secret),
        CliCommand::Undo { steps } => cmd_undo_redo(&host_port, secret, "undo", steps.unwrap_or(1)),
        CliCommand::Redo { steps } => cmd_undo_redo(&host_port, secret, "redo", steps.unwrap_or(1)),
        CliCommand::Rect {
            x,
            y,
            w,
            h,
            fill,
            name,
        } => cmd_create_shape(
            &host_port,
            secret,
            "rectangle",
            x,
            y,
            w,
            h,
            None,
            None,
            &fill,
            name.as_deref(),
        ),
        CliCommand::Ellipse {
            x,
            y,
            w,
            h,
            fill,
            name,
        } => cmd_create_shape(
            &host_port,
            secret,
            "ellipse",
            x,
            y,
            w,
            h,
            None,
            None,
            &fill,
            name.as_deref(),
        ),
        CliCommand::Polygon {
            x,
            y,
            w,
            h,
            sides,
            fill,
            name,
        } => cmd_create_shape(
            &host_port,
            secret,
            "polygon",
            x,
            y,
            w,
            h,
            Some(sides),
            None,
            &fill,
            name.as_deref(),
        ),
        CliCommand::Star {
            x,
            y,
            w,
            h,
            points,
            inner_ratio,
            fill,
            name,
        } => cmd_create_shape(
            &host_port,
            secret,
            "star",
            x,
            y,
            w,
            h,
            Some(points),
            Some(inner_ratio),
            &fill,
            name.as_deref(),
        ),
        CliCommand::Path {
            data,
            fill,
            stroke,
            stroke_width,
            name,
        } => cmd_create_path(
            &host_port,
            secret,
            &data,
            &fill,
            stroke.as_deref(),
            stroke_width,
            name.as_deref(),
        ),
        CliCommand::Layer { name } => cmd_create_layer(&host_port, secret, &name),
        CliCommand::Node { id_or_name } => cmd_get_node(&host_port, secret, &id_or_name),
        CliCommand::Update {
            id,
            name,
            fill,
            opacity,
            show,
            hide,
        } => cmd_update_node(
            &host_port,
            secret,
            &id,
            name.as_deref(),
            fill.as_deref(),
            opacity,
            show,
            hide,
        ),
        CliCommand::Delete { ids } => cmd_delete_nodes(&host_port, secret, &ids),
        CliCommand::Move { id, dx, dy } => {
            cmd_transform(&host_port, secret, &id, "translate", dx, dy, 0.0, 0.0, 0.0)
        }
        CliCommand::Rotate { id, angle, cx, cy } => {
            cmd_transform(&host_port, secret, &id, "rotate", 0.0, 0.0, angle, cx, cy)
        }
        CliCommand::Scale { id, sx, sy, cx, cy } => {
            cmd_transform(&host_port, secret, &id, "scale", sx, sy, 0.0, cx, cy)
        }
    }
}

// ─── Command handlers ─────────────────────────────────────────────────────────

fn cmd_status(host: &str, secret: Option<&str>) -> Result<()> {
    let result = send_rpc(
        host,
        secret,
        &json!({
            "jsonrpc": "2.0", "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "photonic-cli", "version": "0.1.0" }
            }
        }),
    )?;
    let name = result["server_info"]["name"].as_str().unwrap_or("?");
    let version = result["server_info"]["version"].as_str().unwrap_or("?");
    let proto = result["protocol_version"].as_str().unwrap_or("?");
    println!("✓  Photonic MCP server is running");
    println!("   Server:   {} v{}", name, version);
    println!("   Protocol: {}", proto);
    Ok(())
}

fn cmd_list(host: &str, secret: Option<&str>) -> Result<()> {
    let result = call_tool(
        host,
        secret,
        "get_document_state",
        json!({ "include_path_data": false }),
    )?;

    // The first text item is a human summary; the second contains the pretty-printed JSON state.
    let doc = extract_doc_state(&result).context("Could not parse document state from response")?;

    println!(
        "Document: {} ({} × {})",
        doc["name"].as_str().unwrap_or("?"),
        doc["width"].as_f64().unwrap_or(0.0),
        doc["height"].as_f64().unwrap_or(0.0),
    );
    println!();

    if let Some(layers) = doc["layers"].as_array() {
        for layer in layers {
            let vis = if layer["visible"].as_bool().unwrap_or(true) {
                "●"
            } else {
                "○"
            };
            println!("{} Layer: {}", vis, layer["name"].as_str().unwrap_or("?"));
            if let Some(nodes) = layer["nodes"].as_array() {
                if nodes.is_empty() {
                    println!("    (empty)");
                } else {
                    for node in nodes {
                        let name = node["name"].as_str().unwrap_or("?");
                        let id = node["id"].as_str().unwrap_or("?");
                        let extra = if node["visible"].as_bool().unwrap_or(true) {
                            ""
                        } else {
                            " (hidden)"
                        };
                        println!("    • {}{}", name, extra);
                        println!("      id: {}", id);
                    }
                }
            }
        }
    }

    println!();
    // Print the summary line
    print_tool_text(&result);
    Ok(())
}

fn cmd_screenshot(host: &str, secret: Option<&str>, output: &PathBuf) -> Result<()> {
    println!("Requesting screenshot…");
    let result = call_tool(host, secret, "screenshot", json!({}))?;

    // Find the image content item
    let content = result["content"]
        .as_array()
        .context("No content in response")?;

    let b64 = content
        .iter()
        .find(|item| item["type"].as_str() == Some("image"))
        .and_then(|item| item["data"].as_str())
        .context("No image data in screenshot response")?;

    let bytes = general_purpose::STANDARD
        .decode(b64)
        .context("Failed to decode base64 image")?;

    std::fs::write(output, &bytes).with_context(|| format!("Failed to write to {:?}", output))?;

    println!(
        "✓  Screenshot saved to {:?} ({} bytes)",
        output,
        bytes.len()
    );
    Ok(())
}

fn cmd_clear(host: &str, secret: Option<&str>) -> Result<()> {
    // Get all node IDs first
    let state = call_tool(
        host,
        secret,
        "get_document_state",
        json!({ "include_path_data": false }),
    )?;
    let doc = extract_doc_state(&state).context("Could not parse document state")?;
    let layers = doc["layers"].as_array().context("No layers")?;

    let mut all_ids: Vec<String> = Vec::new();
    for layer in layers {
        if let Some(nodes) = layer["nodes"].as_array() {
            for node in nodes {
                if let Some(id) = node["id"].as_str() {
                    all_ids.push(id.to_string());
                }
            }
        }
    }

    if all_ids.is_empty() {
        println!("Canvas is already empty.");
        return Ok(());
    }

    let count = all_ids.len();
    let result = call_tool(host, secret, "delete_nodes", json!({ "node_ids": all_ids }))?;
    println!("✓  Cleared {} node(s) from canvas", count);
    print_tool_text(&result);
    Ok(())
}

fn cmd_undo_redo(host: &str, secret: Option<&str>, op: &str, steps: u32) -> Result<()> {
    let result = call_tool(host, secret, op, json!({ "steps": steps }))?;
    print_tool_text(&result);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_create_shape(
    host: &str,
    secret: Option<&str>,
    shape_type: &str,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    sides: Option<u32>,
    inner_ratio: Option<f64>,
    fill: &str,
    name: Option<&str>,
) -> Result<()> {
    let mut args = json!({
        "shape_type": shape_type,
        "x": x, "y": y,
        "width": w, "height": h,
        "fill": { "type": "solid", "color": fill },
    });

    if let Some(s) = sides {
        args["sides"] = json!(s);
    }
    if let Some(r) = inner_ratio {
        args["inner_radius"] = json!(r);
    }
    if let Some(n) = name {
        args["name"] = json!(n);
    }

    let result = call_tool(host, secret, "create_shape", args)?;
    println!("✓  Created {}", shape_type);
    print_tool_text(&result);
    Ok(())
}

// ─── HTTP / JSON-RPC helpers ──────────────────────────────────────────────────

/// Extract "host:port" from a URL like "http://127.0.0.1:7842".
fn parse_host_port(server: &str) -> String {
    server
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .split('/')
        .next()
        .unwrap_or("127.0.0.1:7842")
        .to_string()
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

fn build_http_request(host_port: &str, secret: Option<&str>, body: &Value) -> Result<String> {
    let secret = require_secret(secret)?;
    let body_str = body.to_string();
    Ok(format!(
        "POST /mcp HTTP/1.0\r\nHost: {host_port}\r\nContent-Type: application/json\r\n{MCP_SECRET_HEADER}: {secret}\r\nContent-Length: {}\r\n\r\n{body_str}",
        body_str.len()
    ))
}

/// Send a raw JSON-RPC request and return the `result` field.
fn send_rpc(host_port: &str, secret: Option<&str>, body: &Value) -> Result<Value> {
    use std::io::{Read, Write};

    let request = build_http_request(host_port, secret, body)?;

    let mut stream = std::net::TcpStream::connect(host_port).map_err(|e| {
        anyhow::anyhow!(
            "Cannot connect to Photonic at {host_port}: {e}\n\
             Is Photonic running? Launch it with:  photonic\n\
             Or in headless mode:                  photonic --headless"
        )
    })?;

    stream
        .write_all(request.as_bytes())
        .context("Failed to send request")?;
    stream.flush()?;

    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .context("Failed to read response")?;

    let body_start = response
        .find("\r\n\r\n")
        .context("Invalid HTTP response (no header/body separator)")?
        + 4;

    let json: Value =
        serde_json::from_str(&response[body_start..]).context("Invalid JSON in response")?;

    if let Some(err) = json.get("error") {
        bail!("MCP server error: {}", err);
    }

    Ok(json["result"].clone())
}

/// Call a tool via `tools/call` and return the full `ToolResult` value.
fn call_tool(host_port: &str, secret: Option<&str>, tool: &str, args: Value) -> Result<Value> {
    send_rpc(
        host_port,
        secret,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": tool, "arguments": args }
        }),
    )
}

/// Print the first (summary) text content item from a ToolResult value.
fn print_tool_text(result: &Value) {
    if let Some(items) = result["content"].as_array() {
        if let Some(first_text) = items
            .iter()
            .find(|item| item["type"].as_str() == Some("text"))
            .and_then(|item| item["text"].as_str())
        {
            println!("{}", first_text);
        }
    }
}

/// Extract the JSON document state embedded as the second text content item.
///
/// `get_document_state` stores the state JSON via `ContentItem::json()` which
/// serialises the value to a pretty-printed string and wraps it as a `Text` item.
fn extract_doc_state(result: &Value) -> Option<Value> {
    let content = result["content"].as_array()?;
    content
        .iter()
        .filter(|item| item["type"].as_str() == Some("text"))
        .nth(1) // skip first (human-readable summary)
        .and_then(|item| item["text"].as_str())
        .and_then(|text| serde_json::from_str(text).ok())
}

// ─── New command handlers ─────────────────────────────────────────────────────

fn cmd_create_path(
    host: &str,
    secret: Option<&str>,
    data: &str,
    fill: &str,
    stroke: Option<&str>,
    stroke_width: f64,
    name: Option<&str>,
) -> Result<()> {
    let mut args = json!({
        "path_data": data,
        "fill": { "type": "solid", "color": fill },
    });
    if let Some(s) = stroke {
        args["stroke"] = json!({ "color": s, "width": stroke_width, "enabled": true });
    }
    if let Some(n) = name {
        args["name"] = json!(n);
    }
    let result = call_tool(host, secret, "create_path", args)?;
    print_tool_text(&result);
    Ok(())
}

fn cmd_create_layer(host: &str, secret: Option<&str>, name: &str) -> Result<()> {
    let result = call_tool(host, secret, "create_layer", json!({ "name": name }))?;
    print_tool_text(&result);
    Ok(())
}

fn cmd_get_node(host: &str, secret: Option<&str>, id_or_name: &str) -> Result<()> {
    // Try as UUID first, then fall back to name search.
    let args = if uuid::Uuid::parse_str(id_or_name).is_ok() {
        json!({ "node_id": id_or_name })
    } else {
        json!({ "name": id_or_name })
    };
    let result = call_tool(host, secret, "get_node", args)?;
    print_tool_text(&result);
    Ok(())
}

fn cmd_update_node(
    host: &str,
    secret: Option<&str>,
    id: &str,
    name: Option<&str>,
    fill: Option<&str>,
    opacity: Option<f32>,
    show: bool,
    hide: bool,
) -> Result<()> {
    let mut args = json!({ "node_id": id });
    if let Some(n) = name {
        args["name"] = json!(n);
    }
    if let Some(f) = fill {
        args["fill"] = json!({ "type": "solid", "color": f });
    }
    if let Some(o) = opacity {
        args["opacity"] = json!(o);
    }
    if show {
        args["visible"] = json!(true);
    }
    if hide {
        args["visible"] = json!(false);
    }
    let result = call_tool(host, secret, "update_node", args)?;
    print_tool_text(&result);
    Ok(())
}

fn cmd_delete_nodes(host: &str, secret: Option<&str>, ids: &[String]) -> Result<()> {
    let result = call_tool(host, secret, "delete_nodes", json!({ "node_ids": ids }))?;
    print_tool_text(&result);
    Ok(())
}

fn cmd_transform(
    host: &str,
    secret: Option<&str>,
    id: &str,
    operation: &str,
    x_or_sx: f64,
    y_or_sy: f64,
    angle: f64,
    cx: f64,
    cy: f64,
) -> Result<()> {
    let args = match operation {
        "translate" => json!({
            "node_ids": [id],
            "operation": "translate",
            "translate": { "x": x_or_sx, "y": y_or_sy }
        }),
        "rotate" => json!({
            "node_ids": [id],
            "operation": "rotate",
            "rotate": { "angle_degrees": angle, "origin_x": cx, "origin_y": cy }
        }),
        "scale" => json!({
            "node_ids": [id],
            "operation": "scale",
            "scale": { "sx": x_or_sx, "sy": y_or_sy, "origin_x": cx, "origin_y": cy }
        }),
        _ => unreachable!(),
    };
    let result = call_tool(host, secret, "apply_transform", args)?;
    print_tool_text(&result);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::build_http_request;
    use serde_json::json;

    #[test]
    fn cli_requests_include_the_mcp_secret_header() {
        let request = build_http_request(
            "127.0.0.1:7842",
            Some("test-secret"),
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize"
            }),
        )
        .unwrap();

        assert!(request.contains("x-mcp-secret: test-secret\r\n"));
    }

    #[test]
    fn cli_requests_require_a_non_empty_secret() {
        assert!(build_http_request("127.0.0.1:7842", None, &json!({})).is_err());
        assert!(build_http_request("127.0.0.1:7842", Some("  "), &json!({})).is_err());
    }
}
