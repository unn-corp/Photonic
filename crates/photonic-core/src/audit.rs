use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::VecDeque;

/// Maximum combined compact-serialized size of retained argument values.
pub const MAX_AUDIT_ARGUMENT_BYTES: usize = 1024 * 1024;
/// Maximum size of a formatted audit list or export response.
pub const MAX_AUDIT_EXPORT_BYTES: usize = 256 * 1024;

const MAX_AUDIT_ARGUMENT_BYTES_PER_ENTRY: usize = 8 * 1024;
const MAX_AUDIT_ENTRIES: usize = 1000;
const MAX_AUDIT_SUMMARY_DEPTH: usize = 4;
const MAX_AUDIT_SUMMARY_ITEMS: usize = 16;
const MAX_AUDIT_SUMMARY_NODES: usize = 256;
const MAX_AUDIT_STRING_PREVIEW_BYTES: usize = 256;
const MAX_AUDIT_KEY_BYTES: usize = 128;
const AUDIT_SUMMARY_MARKER: &str = "_photonic_audit";

/// A single recorded MCP tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    /// Sequential entry ID (1-based, monotonically increasing).
    pub id: u64,
    /// ISO 8601 UTC timestamp of when the call started.
    pub timestamp: String,
    /// The MCP tool name (e.g. `"create_shape"`).
    pub tool_name: String,
    /// Arguments passed to the tool, or a bounded structural summary.
    pub args: Value,
    /// First 200 characters of the result text, or `"error: <msg>"` on failure.
    pub result_summary: String,
    /// Wall-clock duration of the tool call in milliseconds.
    pub duration_ms: u64,
    /// `true` when the tool returned an MCP error result.
    pub is_error: bool,
}

/// In-memory ring buffer of recent MCP tool calls.
///
/// Stored in `AppState` and shared (via `Arc<StdMutex<AuditLog>>`) between the
/// MCP server and the GUI Audit panel. In-memory only — not persisted across
/// server restarts. Argument values are summarized when needed and are kept
/// within [`MAX_AUDIT_ARGUMENT_BYTES`] of compact-serialized data.
pub struct AuditLog {
    entries: VecDeque<AuditEntry>,
    next_id: u64,
    max_entries: usize,
    argument_bytes: usize,
}

impl AuditLog {
    pub fn new() -> Self {
        Self {
            entries: VecDeque::new(),
            next_id: 1,
            max_entries: MAX_AUDIT_ENTRIES,
            argument_bytes: 0,
        }
    }

    /// Record a new entry. Assigns `entry.id`, bounds its arguments, and trims
    /// the oldest entries if either retention limit is exceeded.
    pub fn record(&mut self, mut entry: AuditEntry) {
        entry.id = self.next_id;
        self.next_id += 1;
        entry.args = bounded_arguments(entry.args);
        self.argument_bytes = self
            .argument_bytes
            .saturating_add(serialized_json_bytes(&entry.args));
        self.entries.push_back(entry);

        while self.entries.len() > self.max_entries
            || self.argument_bytes > MAX_AUDIT_ARGUMENT_BYTES
        {
            let Some(evicted) = self.entries.pop_front() else {
                break;
            };
            self.argument_bytes = self
                .argument_bytes
                .saturating_sub(serialized_json_bytes(&evicted.args));
        }
    }

    /// Borrow all stored entries (oldest first).
    pub fn entries(&self) -> &VecDeque<AuditEntry> {
        &self.entries
    }

    /// Return up to `limit` most-recent entries (newest first).
    pub fn recent(&self, limit: usize) -> Vec<&AuditEntry> {
        self.entries.iter().rev().take(limit).collect()
    }

    /// Total number of entries recorded since the server started (includes
    /// entries that have been evicted from the buffer).
    pub fn total_recorded(&self) -> u64 {
        self.next_id - 1
    }

    /// Compact-serialized size of all retained argument values.
    pub fn argument_bytes(&self) -> usize {
        self.argument_bytes
    }
}

impl Default for AuditLog {
    fn default() -> Self {
        Self::new()
    }
}

fn bounded_arguments(args: Value) -> Value {
    let original_type = value_type(&args);
    let original_measure = value_measure(&args);
    let mut nodes = 0;
    let (summary, truncated) = summarize_value(&args, 0, &mut nodes);
    let candidate = if truncated { summary } else { args };

    if serialized_json_bytes(&candidate) <= MAX_AUDIT_ARGUMENT_BYTES_PER_ENTRY {
        candidate
    } else {
        minimal_summary(original_type, original_measure)
    }
}

fn summarize_value(value: &Value, depth: usize, nodes: &mut usize) -> (Value, bool) {
    if *nodes >= MAX_AUDIT_SUMMARY_NODES {
        return (
            minimal_summary(value_type(value), value_measure(value)),
            true,
        );
    }
    *nodes += 1;

    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => (value.clone(), false),
        Value::String(text) => {
            if text.len() <= MAX_AUDIT_STRING_PREVIEW_BYTES {
                (value.clone(), false)
            } else {
                let mut summary = Map::new();
                summary.insert("preview".to_string(), Value::String(string_preview(text)));
                insert_marker(&mut summary, retention_marker("string", text.len()));
                (Value::Object(summary), true)
            }
        }
        Value::Array(items) => {
            if depth >= MAX_AUDIT_SUMMARY_DEPTH {
                return (
                    minimal_summary(value_type(value), value_measure(value)),
                    true,
                );
            }

            let mut truncated = items.len() > MAX_AUDIT_SUMMARY_ITEMS;
            let mut summarized_items = Vec::with_capacity(items.len().min(MAX_AUDIT_SUMMARY_ITEMS));
            for item in items.iter().take(MAX_AUDIT_SUMMARY_ITEMS) {
                let (summary, item_truncated) = summarize_value(item, depth + 1, nodes);
                summarized_items.push(summary);
                truncated |= item_truncated;
            }

            if !truncated {
                (value.clone(), false)
            } else {
                let mut summary = Map::new();
                summary.insert("items".to_string(), Value::Array(summarized_items));
                insert_marker(&mut summary, retention_marker("array", items.len()));
                (Value::Object(summary), true)
            }
        }
        Value::Object(fields) => {
            if depth >= MAX_AUDIT_SUMMARY_DEPTH {
                return (
                    minimal_summary(value_type(value), value_measure(value)),
                    true,
                );
            }

            let mut truncated = fields.len() > MAX_AUDIT_SUMMARY_ITEMS;
            let mut summary = Map::new();
            for (key, field) in fields.iter().take(MAX_AUDIT_SUMMARY_ITEMS) {
                if key.len() > MAX_AUDIT_KEY_BYTES {
                    truncated = true;
                    continue;
                }
                let (field_summary, field_truncated) = summarize_value(field, depth + 1, nodes);
                summary.insert(key.clone(), field_summary);
                truncated |= field_truncated;
            }

            if !truncated {
                (value.clone(), false)
            } else {
                insert_marker(&mut summary, retention_marker("object", fields.len()));
                (Value::Object(summary), true)
            }
        }
    }
}

fn minimal_summary(type_name: &str, measure: usize) -> Value {
    let mut summary = Map::new();
    summary.insert(
        AUDIT_SUMMARY_MARKER.to_string(),
        retention_marker(type_name, measure),
    );
    Value::Object(summary)
}

fn retention_marker(type_name: &str, measure: usize) -> Value {
    let mut marker = Map::new();
    marker.insert("truncated".to_string(), Value::Bool(true));
    marker.insert("type".to_string(), Value::String(type_name.to_string()));
    marker.insert(
        "size".to_string(),
        Value::Number(serde_json::Number::from(measure as u64)),
    );
    Value::Object(marker)
}

fn insert_marker(summary: &mut Map<String, Value>, marker: Value) {
    let mut key = AUDIT_SUMMARY_MARKER.to_string();
    let mut suffix = 0;
    while summary.contains_key(&key) {
        suffix += 1;
        key = format!("{AUDIT_SUMMARY_MARKER}_{suffix}");
    }
    summary.insert(key, marker);
}

fn string_preview(text: &str) -> String {
    let mut preview = String::new();
    for character in text.chars() {
        if preview.len() + character.len_utf8() > MAX_AUDIT_STRING_PREVIEW_BYTES {
            break;
        }
        preview.push(character);
    }
    preview
}

fn serialized_json_bytes(value: &Value) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len())
}

fn value_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn value_measure(value: &Value) -> usize {
    match value {
        Value::String(text) => text.len(),
        Value::Array(items) => items.len(),
        Value::Object(fields) => fields.len(),
        Value::Null | Value::Bool(_) | Value::Number(_) => 0,
    }
}

// ── Timestamp helper ─────────────────────────────────────────────────────────

/// Current UTC time as an ISO 8601 string. Uses only `std::time` — no external
/// datetime crate required.
pub fn audit_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => {
            let secs = d.as_secs();
            let (y, mo, day, h, min, s) = epoch_to_ymd(secs);
            format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
                y, mo, day, h, min, s
            )
        }
        Err(_) => "1970-01-01T00:00:00Z".to_string(),
    }
}

fn epoch_to_ymd(secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let s = secs % 60;
    let total_min = secs / 60;
    let min = total_min % 60;
    let total_h = total_min / 60;
    let h = total_h % 24;
    let mut days = total_h / 24;

    let mut year = 1970u32;
    loop {
        let dy = days_in_year(year);
        if days < dy as u64 {
            break;
        }
        days -= dy as u64;
        year += 1;
    }
    let mut month = 1u32;
    loop {
        let dm = days_in_month(year, month);
        if days < dm as u64 {
            break;
        }
        days -= dm as u64;
        month += 1;
    }
    (year, month, days as u32 + 1, h as u32, min as u32, s as u32)
}

fn is_leap(y: u32) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}
fn days_in_year(y: u32) -> u32 {
    if is_leap(y) {
        366
    } else {
        365
    }
}
fn days_in_month(y: u32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap(y) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(args: Value, is_error: bool) -> AuditEntry {
        AuditEntry {
            id: 0,
            timestamp: "2026-08-25T12:00:00Z".to_string(),
            tool_name: "test_tool".to_string(),
            args,
            result_summary: "test result".to_string(),
            duration_ms: 12,
            is_error,
        }
    }

    #[test]
    fn preserves_small_arguments_and_audit_fields() {
        let args = serde_json::json!({
            "node_id": "node-1",
            "visible": true,
        });
        let mut log = AuditLog::new();
        log.record(entry(args.clone(), true));

        let retained = log.entries().front().expect("entry retained");
        assert_eq!(retained.id, 1);
        assert_eq!(retained.args, args);
        assert_eq!(retained.timestamp, "2026-08-25T12:00:00Z");
        assert_eq!(retained.tool_name, "test_tool");
        assert_eq!(retained.result_summary, "test result");
        assert_eq!(retained.duration_ms, 12);
        assert!(retained.is_error);
    }

    #[test]
    fn summarizes_oversized_arguments_while_preserving_small_fields() {
        let args = serde_json::json!({
            "small_field": "keep me",
            "payload": "x".repeat(MAX_AUDIT_ARGUMENT_BYTES_PER_ENTRY * 4),
        });
        let mut log = AuditLog::new();
        log.record(entry(args, false));

        let retained = log.entries().front().expect("entry retained");
        assert_eq!(retained.args["small_field"], "keep me");
        assert_eq!(
            retained.args["payload"][AUDIT_SUMMARY_MARKER]["truncated"],
            true
        );
        assert!(serde_json::to_vec(&retained.args).is_ok());
        assert!(log.argument_bytes() <= MAX_AUDIT_ARGUMENT_BYTES);
    }

    #[test]
    fn summarizes_binary_like_string_arguments() {
        let args = serde_json::json!({
            "image_data": format!(
                "data:image/png;base64,{}",
                "A".repeat(MAX_AUDIT_ARGUMENT_BYTES_PER_ENTRY * 4)
            ),
        });
        let mut log = AuditLog::new();
        log.record(entry(args, false));

        let retained = log.entries().front().expect("entry retained");
        assert_eq!(
            retained.args["image_data"][AUDIT_SUMMARY_MARKER]["truncated"],
            true
        );
        assert_eq!(
            retained.args["image_data"][AUDIT_SUMMARY_MARKER]["type"],
            "string"
        );
        assert!(retained.args["image_data"]["preview"]
            .as_str()
            .expect("string preview")
            .starts_with("data:image/png;base64,"));
        assert!(log.argument_bytes() <= MAX_AUDIT_ARGUMENT_BYTES);
    }

    #[test]
    fn enforces_total_serialized_argument_budget() {
        let mut fields = Map::new();
        for index in 0..MAX_AUDIT_SUMMARY_ITEMS {
            fields.insert(
                format!("field_{index}"),
                Value::String("x".repeat(MAX_AUDIT_STRING_PREVIEW_BYTES)),
            );
        }
        let args = Value::Object(fields);
        let mut log = AuditLog::new();

        for _ in 0..1000 {
            log.record(entry(args.clone(), false));
        }

        let serialized_bytes: usize = log
            .entries()
            .iter()
            .map(|retained| {
                serde_json::to_vec(&retained.args)
                    .expect("valid JSON")
                    .len()
            })
            .sum();
        assert_eq!(serialized_bytes, log.argument_bytes());
        assert!(serialized_bytes <= MAX_AUDIT_ARGUMENT_BYTES);
        assert!(log.entries().len() < 1000);
        assert_eq!(log.total_recorded(), 1000);
    }
}
