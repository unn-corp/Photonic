use crate::protocol::{ExportAuditLogArgs, ListAuditLogArgs, ToolResult};
use crate::server::AppState;
use photonic_core::{AuditEntry, MAX_AUDIT_EXPORT_BYTES};

/// Serialize audit entries without allowing formatting to exceed the response
/// budget. The caller supplies entries in the order that should be retained;
/// entries that do not fit are removed from the end of that ordered view.
fn serialize_entries_bounded<F>(entries: &[AuditEntry], header: F) -> Result<String, String>
where
    F: Fn(usize, bool) -> String,
{
    let retained = entries.len();
    let mut first_fitting = 0;
    let mut last_fitting = retained;

    // The serialized size grows monotonically as entries are added. Binary
    // search avoids repeatedly serializing the whole buffer when it is over
    // the response budget.
    while first_fitting < last_fitting {
        let candidate = first_fitting + (last_fitting - first_fitting).div_ceil(2);
        let output = serialize_entries(&entries[..candidate], retained, &header)?;
        if output.len() <= MAX_AUDIT_EXPORT_BYTES {
            first_fitting = candidate;
        } else {
            last_fitting = candidate - 1;
        }
    }

    serialize_entries(&entries[..first_fitting], retained, &header)
}

fn serialize_entries<F>(
    entries: &[AuditEntry],
    retained: usize,
    header: &F,
) -> Result<String, String>
where
    F: Fn(usize, bool) -> String,
{
    let json =
        serde_json::to_string_pretty(entries).map_err(|e| format!("serialization error: {e}"))?;
    Ok(format!(
        "{}\n{}",
        header(entries.len(), entries.len() < retained),
        json
    ))
}

/// Return the most-recent N audit entries (default 50) as formatted JSON text.
pub async fn list_audit_log(state: &AppState, args: ListAuditLogArgs) -> ToolResult {
    let limit = args.limit.unwrap_or(50).min(1000);
    let (entries, total) = match state.audit_log.lock() {
        Ok(log) => {
            let entries: Vec<_> = log.recent(limit).into_iter().cloned().collect();
            let total = log.total_recorded();
            (entries, total)
        }
        Err(_) => return ToolResult::error("audit log lock poisoned"),
    };

    match serialize_entries_bounded(&entries, |returned, byte_limited| {
        if byte_limited {
            format!(
                "Last {} of {} recorded calls (newest first; response capped at {} bytes):",
                returned, total, MAX_AUDIT_EXPORT_BYTES
            )
        } else {
            format!(
                "Last {} of {} recorded calls (newest first):",
                returned, total
            )
        }
    }) {
        Ok(output) => ToolResult::text(output),
        Err(error) => ToolResult::error(error),
    }
}

/// Export the retained audit log as a bounded JSON array (oldest first).
/// Entries that do not fit the response budget are omitted from the end.
pub async fn export_audit_log(state: &AppState, _args: ExportAuditLogArgs) -> ToolResult {
    let (entries, total) = match state.audit_log.lock() {
        Ok(log) => {
            let entries: Vec<_> = log.entries().iter().cloned().collect();
            let total = log.total_recorded();
            (entries, total)
        }
        Err(_) => return ToolResult::error("audit log lock poisoned"),
    };

    let retained = entries.len();
    match serialize_entries_bounded(&entries, |returned, byte_limited| {
        if byte_limited {
            format!(
                "// Photonic MCP audit log — {} total recorded calls, {} in buffer; {} oldest entries exported (response capped at {} bytes)",
                total, retained, returned, MAX_AUDIT_EXPORT_BYTES
            )
        } else {
            format!(
                "// Photonic MCP audit log — {} total recorded calls, {} in buffer",
                total, returned
            )
        }
    }) {
        Ok(output) => ToolResult::text(output),
        Err(error) => ToolResult::error(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(id: u64, payload_bytes: usize) -> AuditEntry {
        AuditEntry {
            id,
            timestamp: "2026-08-28T12:00:00Z".to_string(),
            tool_name: format!("tool_{id}"),
            args: json!({"payload": "x".repeat(payload_bytes)}),
            result_summary: "completed".to_string(),
            duration_ms: 7,
            is_error: id % 2 == 0,
        }
    }

    #[test]
    fn bounded_export_preserves_metadata_and_stays_within_response_budget() {
        let entries: Vec<_> = (1..=400).map(|id| entry(id, 1024)).collect();
        let retained = entries.len();
        let output = serialize_entries_bounded(&entries, |returned, byte_limited| {
            if byte_limited {
                format!(
                    "// audit: {} total, {} retained; {} oldest exported (response capped at {} bytes)",
                    retained, retained, returned, MAX_AUDIT_EXPORT_BYTES
                )
            } else {
                format!("// audit: {retained} total, {returned} retained")
            }
        })
        .expect("audit export should serialize");

        assert!(output.len() <= MAX_AUDIT_EXPORT_BYTES);
        assert!(output.contains("response capped"));

        let json = output
            .split_once('\n')
            .expect("export should have a header")
            .1;
        let exported: Vec<AuditEntry> = serde_json::from_str(json).expect("valid export JSON");
        assert!(!exported.is_empty());
        assert!(exported.len() < entries.len());
        assert_eq!(exported[0].id, 1);
        assert_eq!(exported[0].tool_name, "tool_1");
        assert_eq!(exported[0].args["payload"], "x".repeat(1024));
        assert!(!exported[0].is_error);
        assert_eq!(exported.last().unwrap().id, exported.len() as u64);
    }

    #[tokio::test]
    async fn export_handler_applies_the_response_budget() {
        use crate::handlers::clipboard::new_clipboard_ring;
        use crate::protocol::ContentItem;
        use crate::server::McpServerConfig;
        use photonic_core::{history::CommandHistory, AuditLog, Document};
        use std::sync::{mpsc, Arc, Mutex as StdMutex};
        use tokio::sync::Mutex;

        let (capture_tx, _capture_rx) = mpsc::channel();
        let state = AppState {
            document: Arc::new(Mutex::new(Document::new("audit test", 200.0, 100.0))),
            history: Arc::new(Mutex::new(CommandHistory::new(100))),
            document_path: Arc::new(StdMutex::new(None)),
            capture_tx: Arc::new(StdMutex::new(capture_tx)),
            config: McpServerConfig::default(),
            audit_log: Arc::new(StdMutex::new(AuditLog::new())),
            clipboard_ring: Arc::new(new_clipboard_ring()),
            ..AppState::headless_for_test()
        };

        {
            let mut log = state.audit_log.lock().expect("audit log lock");
            for id in 1..=400 {
                log.record(entry(id, 1024));
            }
        }

        let result = export_audit_log(&state, ExportAuditLogArgs {}).await;
        let ContentItem::Text { text } = result.content.first().expect("text result") else {
            panic!("expected text result");
        };
        assert!(text.len() <= MAX_AUDIT_EXPORT_BYTES);

        let json = text
            .split_once('\n')
            .expect("export should have a header")
            .1;
        let exported: Vec<AuditEntry> = serde_json::from_str(json).expect("valid export JSON");
        assert!(!exported.is_empty());
        assert!(exported.len() < 400);
        assert_eq!(exported[0].id, 1);
        assert_eq!(exported[0].tool_name, "tool_1");
        assert!(text.contains("response capped"));
    }

    #[test]
    fn bounded_list_keeps_newest_entries_and_caps_the_response() {
        let entries: Vec<_> = (1..=400).map(|id| entry(id, 1024)).rev().collect();
        let retained = entries.len();
        let output = serialize_entries_bounded(&entries, |returned, byte_limited| {
            if byte_limited {
                format!(
                    "Last {returned} of {retained} recorded calls (newest first; response capped at {MAX_AUDIT_EXPORT_BYTES} bytes):"
                )
            } else {
                format!("Last {returned} of {retained} recorded calls (newest first):")
            }
        })
        .expect("audit list should serialize");

        assert!(output.len() <= MAX_AUDIT_EXPORT_BYTES);
        assert!(output.contains("response capped"));
        let json = output
            .split_once('\n')
            .expect("list should have a header")
            .1;
        let listed: Vec<AuditEntry> = serde_json::from_str(json).expect("valid list JSON");
        assert!(!listed.is_empty());
        assert!(listed.len() < entries.len());
        assert_eq!(listed[0].id, 400);
        assert_eq!(listed.last().unwrap().id, 400 - listed.len() as u64 + 1);
    }
}
