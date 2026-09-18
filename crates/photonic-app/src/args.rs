#![cfg_attr(test, expect(clippy::items_after_test_module))]

use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Photonic — AI-native vector graphics editor
#[derive(Parser, Debug)]
#[command(name = "photonic", version, about)]
pub struct Args {
    /// Run without a GUI window (MCP server only)
    #[arg(long)]
    pub headless: bool,

    /// Speak MCP on stdin/stdout (Content-Length framing). Implies headless.
    /// Prefer attach via `mcp-proxy` when a GUI is already listening.
    #[arg(long)]
    pub mcp_stdio: bool,

    /// Force the X11/XWayland backend on Linux (enables winit file drag-and-drop)
    #[arg(long)]
    pub x11: bool,

    /// MCP server port
    #[arg(long, default_value_t = 7842)]
    pub mcp_port: u16,

    /// Shared secret for MCP authentication. Required when running the MCP
    /// server; clients must send it in the X-MCP-Secret header on every request.
    #[arg(long)]
    pub mcp_secret: Option<String>,

    /// MCP protocol negotiation: `dual` (default) or `strict` (2026-07-28 only)
    #[arg(long, default_value = "dual", value_parser = parse_mcp_protocol)]
    pub mcp_protocol: photonic_mcp::protocol::ProtocolMode,

    /// Address of a running Photonic instance (for CLI commands)
    #[arg(long, global = true, default_value = "127.0.0.1:7842")]
    pub server: String,

    /// Document to open on launch (GUI / headless mode)
    #[arg(value_name = "FILE")]
    pub file: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<CliCommand>,
}

/// CLI commands that connect to a running Photonic instance.
#[derive(Subcommand, Debug)]
pub enum CliCommand {
    /// Show status of the running Photonic instance
    Status,

    /// List all layers and nodes in the document
    List,

    /// Capture a screenshot and save it to a file
    Screenshot {
        /// Output file path (PNG)
        #[arg(short, long, default_value = "screenshot.png")]
        output: PathBuf,
    },

    /// Remove all nodes from the canvas
    Clear,

    /// Undo the last operation(s)
    Undo {
        /// Number of steps to undo
        #[arg(short, long)]
        steps: Option<u32>,
    },

    /// Redo the last undone operation(s)
    Redo {
        /// Number of steps to redo
        #[arg(short, long)]
        steps: Option<u32>,
    },

    /// Create a rectangle
    Rect {
        #[arg(long, default_value_t = 0.0)]
        x: f64,
        #[arg(long, default_value_t = 0.0)]
        y: f64,
        #[arg(long, default_value_t = 100.0)]
        w: f64,
        #[arg(long, default_value_t = 100.0)]
        h: f64,
        #[arg(long, default_value = "#2277ff")]
        fill: String,
        #[arg(long)]
        name: Option<String>,
    },

    /// Create an ellipse
    Ellipse {
        #[arg(long, default_value_t = 0.0)]
        x: f64,
        #[arg(long, default_value_t = 0.0)]
        y: f64,
        #[arg(long, default_value_t = 100.0)]
        w: f64,
        #[arg(long, default_value_t = 100.0)]
        h: f64,
        #[arg(long, default_value = "#2277ff")]
        fill: String,
        #[arg(long)]
        name: Option<String>,
    },

    /// Create a regular polygon
    Polygon {
        #[arg(long, default_value_t = 0.0)]
        x: f64,
        #[arg(long, default_value_t = 0.0)]
        y: f64,
        #[arg(long, default_value_t = 100.0)]
        w: f64,
        #[arg(long, default_value_t = 100.0)]
        h: f64,
        #[arg(long, default_value_t = 6)]
        sides: u32,
        #[arg(long, default_value = "#2277ff")]
        fill: String,
        #[arg(long)]
        name: Option<String>,
    },

    /// Create a star shape
    Star {
        #[arg(long, default_value_t = 0.0)]
        x: f64,
        #[arg(long, default_value_t = 0.0)]
        y: f64,
        #[arg(long, default_value_t = 100.0)]
        w: f64,
        #[arg(long, default_value_t = 100.0)]
        h: f64,
        #[arg(long, default_value_t = 5)]
        points: u32,
        #[arg(long, default_value_t = 0.4)]
        inner_ratio: f64,
        #[arg(long, default_value = "#ffcc00")]
        fill: String,
        #[arg(long)]
        name: Option<String>,
    },

    /// Create a vector path from SVG path data
    Path {
        /// SVG path data, e.g. "M 0 0 L 100 0 L 100 100 Z"
        data: String,
        #[arg(long, default_value = "#2277ff")]
        fill: String,
        #[arg(long)]
        stroke: Option<String>,
        #[arg(long, default_value_t = 1.0)]
        stroke_width: f64,
        #[arg(long)]
        name: Option<String>,
    },

    /// Create a new layer
    Layer { name: String },

    /// Get details of a node by ID or name
    Node {
        /// Node UUID or display name
        id_or_name: String,
    },

    /// Update an existing node's properties
    Update {
        /// Node UUID
        id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        fill: Option<String>,
        #[arg(long)]
        opacity: Option<f32>,
        /// Show the node
        #[arg(long)]
        show: bool,
        /// Hide the node
        #[arg(long)]
        hide: bool,
    },

    /// Delete one or more nodes by ID
    Delete {
        /// One or more node UUIDs
        ids: Vec<String>,
    },

    /// Move a node by a relative offset
    Move {
        id: String,
        #[arg(long, default_value_t = 0.0)]
        dx: f64,
        #[arg(long, default_value_t = 0.0)]
        dy: f64,
    },

    /// Rotate a node around a centre point
    Rotate {
        id: String,
        /// Angle in degrees (clockwise)
        #[arg(long)]
        angle: f64,
        #[arg(long, default_value_t = 0.0)]
        cx: f64,
        #[arg(long, default_value_t = 0.0)]
        cy: f64,
    },

    /// Scale a node
    Scale {
        id: String,
        #[arg(long, default_value_t = 1.0)]
        sx: f64,
        #[arg(long, default_value_t = 1.0)]
        sy: f64,
        #[arg(long, default_value_t = 0.0)]
        cx: f64,
        #[arg(long, default_value_t = 0.0)]
        cy: f64,
    },

    /// Execute a Lua script (headless — no window required)
    Run { script: std::path::PathBuf },

    /// Proxy stdin MCP messages to the running Photonic HTTP MCP server (internal)
    #[command(name = "mcp-proxy", hide = true)]
    McpProxy,
}

#[cfg(test)]
mod tests {
    use super::Args;
    use clap::Parser;

    #[test]
    fn x11_flag_is_opt_in() {
        let default_args = Args::try_parse_from(["photonic"]).expect("default CLI arguments parse");
        assert!(!default_args.x11);

        let x11_args = Args::try_parse_from(["photonic", "--x11"]).expect("--x11 parses");
        assert!(x11_args.x11);
    }
}

fn parse_mcp_protocol(s: &str) -> Result<photonic_mcp::protocol::ProtocolMode, String> {
    photonic_mcp::protocol::ProtocolMode::parse(s)
        .ok_or_else(|| format!("invalid --mcp-protocol {s:?}; expected dual|strict"))
}
