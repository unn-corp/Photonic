//! Photonic MCP server: JSON-RPC tools for AI-assisted editing.
//!
//! Protocol versioning, stdio, PathPolicy integration, and Pattern B catalog
//! helpers are documented in **`docs/specs/mcp-2026-07-28.md`**.
//!
#![recursion_limit = "2048"]
// Pairwise path operations retain indexed loops so removal and replacement
// decisions share the original ordering. Keep the exception self-checking.
#![expect(clippy::needless_range_loop)]
// `pub` (29 §3 / CAP-019): the acceptance-story harness in
// `crates/photonic-app/tests/` must drive the real tool-call path — the
// same `dispatch_tool` entry that carries document-snapshot/undo/audit-log
// side effects — from outside this crate. `dispatch_tool_inner` stays
// `pub(crate)`.
pub mod dispatch;

pub mod auth;
pub mod catalog;
pub mod handlers;
pub mod path_guard;
pub mod protocol;
pub mod schema_gen;
pub mod server;
pub mod stdio;

pub use handlers::doc_export::register_export_gpu;
pub use server::{McpServer, McpServerConfig, MCP_SECRET_HEADER};
