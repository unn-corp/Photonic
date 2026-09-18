//! Tool catalog helpers for Pattern B search + execute.

use crate::schema_gen::tool_list;
use serde_json::{json, Value};
use std::sync::OnceLock;

/// Promote high-frequency tools so compact listings stay useful.
pub fn promoted_tool_names() -> &'static [&'static str] {
    &[
        "undo",
        "redo",
        "screenshot",
        "get_document_info",
        "get_document_state",
        "list_artboards",
        "create_shape",
        "set_paint",
        "save_document",
        "create_sequence",
        "list_sequences",
        "import_media",
        "insert_clip",
        "split_clip",
        "get_video_capabilities",
        "get_timeline_snapshot",
        "get_engine_status",
        "render_frame_at",
        "export_sequence",
    ]
}

/// Borrow the complete registry, initialized once for the process.
///
/// Search and exact-name lookup borrow this slice so a request only clones the
/// definitions it actually returns, rather than every schema in the catalog.
pub fn cached_tools() -> &'static [Value] {
    static CACHE: OnceLock<Vec<Value>> = OnceLock::new();
    CACHE.get_or_init(|| match tool_list() {
        Value::Array(a) => a,
        other => vec![other],
    })
}

/// All tools from the registry as owned JSON values.
///
/// Prefer [`cached_tools`] when the caller does not need to mutate the catalog.
pub fn all_tools() -> Vec<Value> {
    cached_tools().to_vec()
}

fn tool_name(t: &Value) -> Option<&str> {
    t.get("name").and_then(|n| n.as_str())
}

/// Look up the complete definition (input/output schemas and annotations) by
/// exact, case-sensitive tool name. Unknown names are not synthesized.
pub fn tool_schema(name: &str) -> Option<&'static Value> {
    cached_tools()
        .iter()
        .find(|tool| tool_name(tool) == Some(name))
}

/// Compact list: promoted tools that exist in this build, plus discovery tools.
pub fn compact_tool_list() -> Value {
    let all = cached_tools();
    let mut out = Vec::new();
    for name in ["search_actions", "get_action_schema", "execute_action"] {
        if let Some(t) = all.iter().find(|t| tool_name(t) == Some(name)) {
            out.push(t.clone());
        }
    }
    for name in promoted_tool_names() {
        if let Some(t) = all.iter().find(|t| tool_name(t) == Some(name)) {
            if !out.iter().any(|x| tool_name(x) == Some(name)) {
                out.push(t.clone());
            }
        }
    }
    json!(out)
}

/// Search tools by keyword tokens against name + description.
pub fn search_actions(query: &str, limit: usize) -> Value {
    let q = query.to_ascii_lowercase();
    let tokens: Vec<&str> = q.split_whitespace().filter(|t| !t.is_empty()).collect();
    let mut scored: Vec<(i32, &Value)> = Vec::new();
    for t in cached_tools() {
        let name = tool_name(t).unwrap_or("").to_ascii_lowercase();
        let desc = t
            .get("description")
            .and_then(|d| d.as_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if matches!(
            name.as_str(),
            "search_actions" | "get_action_schema" | "execute_action"
        ) {
            continue;
        }
        let mut score = 0i32;
        if tokens.is_empty() {
            continue;
        }
        for tok in &tokens {
            if name == *tok {
                score += 100;
            } else if name.contains(tok) {
                score += 40;
            }
            if desc.contains(tok) {
                score += 10;
            }
        }
        if score > 0 {
            scored.push((score, t));
        }
    }
    scored.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    let limit = limit.clamp(1, 50);
    let hits: Vec<Value> = scored
        .into_iter()
        .take(limit)
        .map(|(score, tool)| {
            let mut definition = tool.clone();
            definition["score"] = json!(score);
            definition
        })
        .collect();
    json!({ "actions": hits, "count": hits.len(), "query": query })
}
