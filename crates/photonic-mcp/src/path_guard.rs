//! Thin MCP wrapper around `photonic_core::PathPolicy`.

use crate::protocol::ToolResult;
use crate::server::AppState;
use photonic_core::{PathAccess, PathPolicyError};
use std::path::{Path, PathBuf};

/// Check a path for the given access; on deny return a tool error.
pub fn check_path(
    state: &AppState,
    path: impl AsRef<Path>,
    access: PathAccess,
) -> Result<PathBuf, ToolResult> {
    state
        .path_policy
        .require(path, access)
        .map_err(|e: PathPolicyError| {
            ToolResult::error(e.to_string()).with_data(serde_json::json!({
                "error_code": "PathNotPermitted",
                "path": e.path,
                "reason": e.reason.to_string(),
            }))
        })
}
