//! Anthropic API client for the embedded Claude chat panel.
//!
//! Sends user messages to `api.anthropic.com/v1/messages` with Photonic's
//! MCP tools attached.  When Claude uses a tool, we forward the call to the
//! configured local MCP server and loop until Claude produces a final text
//! reply.

use photonic_mcp::MCP_SECRET_HEADER;
use serde_json::{json, Value};

const ANTHROPIC_API: &str = "https://api.anthropic.com/v1/messages";
const MODEL: &str = "claude-opus-4-6";
const MAX_TOOL_ROUNDS: usize = 12;

const SYSTEM_PROMPT: &str = "\
You are an AI design assistant embedded inside Photonic, a vector graphics \
editor for Windows. The user can ask you to create, modify, or describe \
vector graphics and you will use the available tools to make changes \
directly in the document.

Canvas defaults to 800 × 600 px.  Coordinates: (0, 0) = top-left corner.
Colours are hex strings like \"#ff4400\" or \"#3366cc\".
After creating or modifying shapes, briefly describe what you did.";

// ─── Public entry point ───────────────────────────────────────────────────────

/// Send `user_message` to Claude, including the full prior `history`.
///
/// `history` is a slice of `(is_user, text)` pairs in chronological order.
/// `mcp_port` is the port used by Photonic's local MCP server.
/// `mcp_secret` is the shared secret sent with every MCP request.
/// Returns the final assistant text, or an `Err` string to show in the chat.
pub fn send_message(
    api_key: &str,
    history: &[(bool, String)],
    user_message: &str,
    mcp_port: u16,
    mcp_secret: Option<&str>,
) -> Result<String, String> {
    if api_key.trim().is_empty() {
        return Err("No API key — enter your Anthropic API key in the Claude tab.".into());
    }
    let mcp_secret = require_secret(mcp_secret)?;

    // Build the messages array from chat history + new user message.
    let mut messages: Vec<Value> = history
        .iter()
        .map(|(is_user, text)| {
            json!({
                "role": if *is_user { "user" } else { "assistant" },
                "content": text
            })
        })
        .collect();
    messages.push(json!({ "role": "user", "content": user_message }));

    let endpoint = mcp_url(mcp_port);
    let tools = fetch_mcp_tools(&endpoint, mcp_secret)?;
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    // Agentic loop — keep going while Claude uses tools.
    for _round in 0..MAX_TOOL_ROUNDS {
        let body = json!({
            "model": MODEL,
            "max_tokens": 4096,
            "system": SYSTEM_PROMPT,
            "messages": messages,
            "tools": tools,
        });

        let resp = client
            .post(ANTHROPIC_API)
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .map_err(|e| format!("Network error: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp.text().unwrap_or_default();
            // Try to extract a friendly error message.
            if let Ok(v) = serde_json::from_str::<Value>(&body_text) {
                let msg = v["error"]["message"].as_str().unwrap_or(&body_text);
                return Err(format!("API error {status}: {msg}"));
            }
            return Err(format!("API error {status}: {body_text}"));
        }

        let response: Value = resp.json().map_err(|e| format!("Parse error: {e}"))?;
        let stop_reason = response["stop_reason"].as_str().unwrap_or("end_turn");
        let content = response["content"].as_array().cloned().unwrap_or_default();

        // Collect text from this response turn.
        let text: String = content
            .iter()
            .filter(|c| c["type"] == "text")
            .filter_map(|c| c["text"].as_str())
            .collect::<Vec<_>>()
            .join("");

        if stop_reason != "tool_use" {
            return Ok(if text.is_empty() {
                "(empty response)".into()
            } else {
                text
            });
        }

        // Claude wants to use tools — collect them and execute.
        let tool_uses: Vec<&Value> = content.iter().filter(|c| c["type"] == "tool_use").collect();

        // Append assistant turn to history before we send tool results.
        messages.push(json!({ "role": "assistant", "content": &content }));

        let mut tool_results: Vec<Value> = Vec::new();
        for tool_use in &tool_uses {
            let id = tool_use["id"].as_str().unwrap_or("");
            let name = tool_use["name"].as_str().unwrap_or("");
            let input = &tool_use["input"];

            let result = call_mcp_tool(&endpoint, mcp_secret, name, input);
            let (content_text, is_error) = match result {
                Ok(t) => (t, false),
                Err(e) => (format!("Tool error: {e}"), true),
            };

            tool_results.push(json!({
                "type": "tool_result",
                "tool_use_id": id,
                "content": content_text,
                "is_error": is_error,
            }));
        }

        messages.push(json!({ "role": "user", "content": tool_results }));
    }

    Err("Reached maximum tool-use rounds — Claude may be looping.".into())
}

// ─── MCP tool execution ───────────────────────────────────────────────────────

fn call_mcp_tool(
    endpoint: &str,
    secret: &str,
    name: &str,
    input: &Value,
) -> Result<String, String> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": name, "arguments": input }
    });

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;

    let resp = client
        .post(endpoint)
        .header(MCP_SECRET_HEADER, secret)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .map_err(|e| format!("MCP connection error: {e}"))?;

    let result: Value = resp.json().map_err(|e| format!("MCP parse error: {e}"))?;

    if let Some(err) = result.get("error") {
        return Err(err["message"]
            .as_str()
            .unwrap_or("unknown MCP error")
            .to_string());
    }

    let content = &result["result"]["content"];
    if let Some(items) = content.as_array() {
        let text = items
            .iter()
            .filter(|item| item["type"] == "text")
            .filter_map(|item| item["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        Ok(if text.is_empty() { "ok".into() } else { text })
    } else {
        Ok(serde_json::to_string(content).unwrap_or_else(|_| "ok".into()))
    }
}

// ─── Tool list (fetched live from MCP server) ─────────────────────────────────

/// Fetch the tool list from the running MCP server and adapt it for the
/// Anthropic API.  MCP uses camelCase `inputSchema`; Anthropic requires
/// snake_case `input_schema` — we rename the key here.
fn fetch_mcp_tools(endpoint: &str, secret: &str) -> Result<Value, String> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list",
        "params": {}
    });

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;

    let resp = client
        .post(endpoint)
        .header(MCP_SECRET_HEADER, secret)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .map_err(|_| {
            "MCP server not running — start Photonic before using the chat panel.".to_string()
        })?;

    let result: Value = resp.json().map_err(|e| format!("MCP parse error: {e}"))?;

    if let Some(err) = result.get("error") {
        return Err(err["message"].as_str().unwrap_or("MCP error").to_string());
    }

    // Rename "inputSchema" → "input_schema" for Anthropic API compatibility.
    let tools = result["result"]["tools"]
        .as_array()
        .ok_or_else(|| "tools/list returned no tools array".to_string())?
        .iter()
        .map(|t| {
            let mut tool = t.clone();
            if let Some(obj) = tool.as_object_mut() {
                if let Some(schema) = obj.remove("inputSchema") {
                    obj.insert("input_schema".to_string(), schema);
                }
            }
            tool
        })
        .collect::<Vec<_>>();

    Ok(Value::Array(tools))
}

fn mcp_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/mcp")
}

fn require_secret(secret: Option<&str>) -> Result<&str, String> {
    let Some(secret) = secret.filter(|secret| !secret.trim().is_empty()) else {
        return Err("MCP authentication requires --mcp-secret or PHOTONIC_MCP_SECRET".to_string());
    };
    if secret.bytes().any(|byte| matches!(byte, b'\r' | b'\n')) {
        return Err("MCP secret cannot contain CR or LF characters".to_string());
    }
    Ok(secret)
}

#[cfg(test)]
mod tests {
    use super::{call_mcp_tool, fetch_mcp_tools, mcp_url};
    use serde_json::json;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn mcp_url_uses_configured_port() {
        assert_eq!(mcp_url(9000), "http://127.0.0.1:9000/mcp",);
    }

    #[test]
    fn mcp_requests_reach_configured_endpoint() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let mut requests = Vec::new();
            for response_body in [
                r#"{"result":{"tools":[]}}"#,
                r#"{"result":{"content":[{"type":"text","text":"ok"}]}}"#,
            ] {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let mut request_line = String::new();
                reader.read_line(&mut request_line).unwrap();

                let mut header = String::new();
                let mut request = request_line;
                loop {
                    header.clear();
                    reader.read_line(&mut header).unwrap();
                    if header == "\r\n" {
                        break;
                    }
                    request.push_str(&header);
                }
                requests.push(request);

                let mut stream = reader.into_inner();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response_body.len(),
                    response_body,
                )
                .unwrap();
            }
            requests
        });

        let endpoint = mcp_url(port);
        let tools = fetch_mcp_tools(&endpoint, "test-secret");
        let tool_result = call_mcp_tool(&endpoint, "test-secret", "noop", &json!({}));
        let requests = server.join().unwrap();

        assert_eq!(tools.unwrap(), json!([]));
        assert_eq!(tool_result.unwrap(), "ok");
        assert_eq!(
            requests
                .iter()
                .map(|request| request.contains("x-mcp-secret: test-secret\r\n"))
                .collect::<Vec<_>>(),
            vec![true, true]
        );
    }
}
