// CLI color parsing preserves the established NaN behavior; command argument
// lists mirror the public CLI surface. Keep both choices self-checking.
#![expect(clippy::manual_clamp, clippy::too_many_arguments)]

mod args;
#[cfg(test)]
#[allow(dead_code)]
mod claude_client;
mod cli;
mod mcp_proxy;
mod repl;
mod script;

use anyhow::Result;
use args::Args;
use clap::Parser;
use egui_wgpu::ScreenDescriptor;
use photonic_core::{document::Document, history::CommandHistory, AuditLog};
use photonic_gui::{NativeClipboardPaste, PhotonicApp};
use photonic_mcp::server::AppState;
use photonic_mcp::{McpServer, McpServerConfig, MCP_SECRET_HEADER};
use photonic_render::PhotonicRenderer;
use repl::LuaRepl;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{oneshot, Mutex};
use tracing::{error, info};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};
#[cfg(target_os = "linux")]
use winit::platform::x11::EventLoopBuilderExtX11;
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::{ElementState, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{Key, KeyCode, NamedKey, PhysicalKey},
    window::{Window, WindowAttributes, WindowId},
};

// ─── Entry point ─────────────────────────────────────────────────────────────

fn native_project_path(path: &std::path::Path) -> Option<std::path::PathBuf> {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("photon"))
        .then(|| path.to_path_buf())
}

/// Detect a paste shortcut before egui consumes the keyboard event. egui-winit
/// intentionally swallows Ctrl/Cmd+V, including the image-only case where it
/// cannot emit an `Event::Paste` text event.
fn native_clipboard_paste_request(event: &WindowEvent, modifiers: egui::Modifiers) -> Option<bool> {
    let WindowEvent::KeyboardInput {
        event: key_event,
        is_synthetic,
        ..
    } = event
    else {
        return None;
    };
    if *is_synthetic || key_event.state != ElementState::Pressed {
        return None;
    }

    let logical_paste = matches!(&key_event.logical_key, Key::Named(NamedKey::Paste));
    let logical_v = matches!(
        &key_event.logical_key,
        Key::Character(text) if text.as_str().eq_ignore_ascii_case("v")
    );
    let physical_v = matches!(key_event.physical_key, PhysicalKey::Code(KeyCode::KeyV));
    let command_v = modifiers.command || modifiers.ctrl || modifiers.mac_cmd;
    let windows_insert = cfg!(target_os = "windows")
        && modifiers.shift
        && (matches!(&key_event.logical_key, Key::Named(NamedKey::Insert))
            || matches!(key_event.physical_key, PhysicalKey::Code(KeyCode::Insert)));

    (logical_paste || (command_v && (logical_v || physical_v)) || windows_insert)
        .then_some(modifiers.shift)
}

const MAX_NATIVE_CLIPBOARD_PIXELS: u64 = 64_000_000;

/// Read the richest useful clipboard representation. HTML/SVG is considered
/// before raster pixels so vector artwork copied from a design tool remains
/// editable when the clipboard offers both formats.
fn read_native_clipboard() -> Option<NativeClipboardPaste> {
    let mut clipboard = arboard::Clipboard::new().ok()?;
    let html = clipboard
        .get()
        .html()
        .ok()
        .filter(|html| !html.trim().is_empty());
    let image = clipboard.get_image().ok().and_then(|image| {
        let width = u32::try_from(image.width).ok()?;
        let height = u32::try_from(image.height).ok()?;
        let pixels = u64::from(width).checked_mul(u64::from(height))?;
        if pixels == 0 || pixels > MAX_NATIVE_CLIPBOARD_PIXELS {
            return None;
        }
        let rgba = image.bytes.into_owned();
        let expected = pixels
            .checked_mul(4)
            .and_then(|len| usize::try_from(len).ok())?;
        (rgba.len() == expected).then_some(NativeClipboardPaste::Image {
            width,
            height,
            rgba,
        })
    });
    let text = clipboard
        .get_text()
        .ok()
        .filter(|text| !text.trim().is_empty());

    if html.as_deref().is_some_and(looks_like_svg_clipboard_text) {
        return html.map(NativeClipboardPaste::Text);
    }
    if text.as_deref().is_some_and(|text| {
        text.trim() == "photonic:objects" || looks_like_svg_clipboard_text(text)
    }) {
        return text.map(NativeClipboardPaste::Text);
    }
    if image.is_some() {
        return image;
    }
    html.or(text).map(NativeClipboardPaste::Text)
}

fn looks_like_svg_clipboard_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("<svg") && lower.contains("</svg")
}

fn main() -> Result<()> {
    let args = Args::parse();
    let cli_secret = args
        .mcp_secret
        .clone()
        .or_else(|| std::env::var("PHOTONIC_MCP_SECRET").ok())
        .or_else(|| photonic_mcp::auth::read_token().ok());

    // ── CLI client mode: a subcommand was given ───────────────────────────────
    if let Some(command) = args.command {
        tracing_subscriber::registry()
            .with(fmt::layer())
            .with(EnvFilter::new("warn"))
            .init();
        return cli::run(&args.server, cli_secret.as_deref(), command);
    }

    // ── Server / GUI mode: full logging ──────────────────────────────────────
    // Use %APPDATA%\Photonic\ — a real Windows path that always exists.
    // Fall back to the binary's directory if APPDATA is unavailable.
    let log_dir = {
        let candidate = std::env::var("APPDATA")
            .map(|p| std::path::PathBuf::from(p).join("Photonic"))
            .unwrap_or_else(|_| {
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                    .unwrap_or_else(|| std::path::PathBuf::from("."))
            });
        let _ = std::fs::create_dir_all(&candidate);
        candidate
    };
    let log_path = log_dir.join("photonic.log");

    // Panic hook: write to log file before the process dies, then capture a
    // structured crash report (#59). Local capture is unconditional — it is the
    // same non-sensitive crash facts already going to the log; only *sending* a
    // report is gated behind explicit consent in the GUI on the next launch.
    {
        let path = log_path.clone();
        let orig = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            orig(info);
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                let _ = writeln!(f, "[PANIC] {info}");
            }

            // `force_capture` records a backtrace regardless of RUST_BACKTRACE so
            // a report is always actionable. Best-effort: never panic in the hook.
            let backtrace = std::backtrace::Backtrace::force_capture();
            let report = photonic_core::CrashReport::capture(info, &backtrace);
            match report.write() {
                Ok(p) => eprintln!("Photonic crash report written: {}", p.display()),
                Err(e) => eprintln!("Failed to write crash report: {e}"),
            }
        }));
    }

    // Synchronous file appender — writes each event directly to disk so nothing
    // is lost when the process is killed (non_blocking would lose buffered events).
    let file_appender = tracing_appender::rolling::never(&log_dir, "photonic.log");

    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn,photonic=debug"));

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer())
        .with(fmt::layer().with_writer(file_appender).with_ansi(false))
        .init();

    eprintln!("Photonic log: {}", log_path.display());

    register_file_association();

    // Loaded document plus any persistent history embedded in a `.photon` file
    // (so a double-clicked or CLI-opened project restores its undo history too).
    let (document, loaded_history) = if let Some(path) = &args.file {
        let content = std::fs::read_to_string(path)?;
        // Detect format by extension: `.svg` is imported, everything else is
        // treated as a Photonic file (`.photon`). Previously every file argument
        // was parsed as JSON, so opening an SVG via the CLI/file argument failed.
        let is_svg = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("svg"));
        if is_svg && !content.trim_start().starts_with('{') {
            let doc = photonic_core::import_svg(&content)
                .map_err(|e| anyhow::anyhow!("failed to import SVG '{}': {e}", path.display()))?;
            (doc, None)
        } else {
            // `load_photon` accepts both the new wrapper format (document +
            // history) and legacy bare-`Document` files.
            photonic_core::load_photon(&content)
                .map_err(|e| anyhow::anyhow!("failed to open '{}': {e}", path.display()))?
        }
    } else {
        (Document::default_artboard(), None)
    };

    info!("Photonic — document: '{}'", document.name);

    let document_arc = Arc::new(Mutex::new(document));
    // Apply the user's configured history limits BEFORE restoring, so a large
    // size-mode history isn't truncated to the default step ceiling on the
    // launch path (the GUI applies limits per-frame, but restore_state enforces
    // immediately, so the limits must already be set here).
    let preferences = photonic_gui::preferences::AppPreferences::load();
    let (hist_steps, hist_size) = preferences.history_limits();
    let mut initial_history = CommandHistory::new(hist_steps);
    initial_history.set_limits(hist_steps, hist_size);
    if let Some(snap) = loaded_history {
        initial_history.restore_state(snap);
    }
    let history_arc: Arc<Mutex<CommandHistory>> = Arc::new(Mutex::new(initial_history));
    let (capture_tx, capture_rx) = std::sync::mpsc::channel::<oneshot::Sender<Vec<u8>>>();

    // Audit log shared between the MCP server thread and the GUI Audit panel.
    let audit_log = Arc::new(std::sync::Mutex::new(AuditLog::new()));
    let mcp_document_path = Arc::new(std::sync::Mutex::new(
        args.file.as_deref().and_then(native_project_path),
    ));

    let secret = args.mcp_secret.clone().or_else(|| {
        // Generate a session token when not pinned (28 §4 / MCP local profile).
        let tok = photonic_mcp::auth::generate_token();
        match photonic_mcp::auth::write_token(&tok) {
            Ok(path) => tracing::info!("MCP session token written to {}", path.display()),
            Err(e) => tracing::warn!("could not write MCP token file: {e}"),
        }
        Some(tok)
    });
    let mcp_config = McpServerConfig {
        port: args.mcp_port,
        secret,
        protocol_mode: args.mcp_protocol,
    };

    // ── Stdio MCP (MCPB / Inspector) ──────────────────────────────────────────
    if args.mcp_stdio {
        info!("Running MCP on stdio (Content-Length framing)");
        let rt = tokio::runtime::Builder::new_multi_thread()
            // Large match in dispatch_tool_inner + schema_gen need more than
            // the default ~2 MiB worker stack (else search_actions / full list overflow).
            .thread_stack_size(8 * 1024 * 1024)
            .enable_all()
            .build()?;
        let mcp_server = McpServer::new(
            Arc::clone(&document_arc),
            Arc::clone(&history_arc),
            capture_tx,
            mcp_config,
            Arc::new(AtomicBool::new(true)),
            audit_log,
        )
        .with_document_path(Arc::clone(&mcp_document_path));
        rt.block_on(photonic_mcp::stdio::run_stdio(mcp_server.state))?;
        return Ok(());
    }

    // ── Headless mode ─────────────────────────────────────────────────────────
    if args.headless {
        // Video engine: no wiring needed here — `McpServer`'s `AppState` owns
        // a lazy headless `VideoEngine` (own adapter, created on the first
        // engine-backed tool call; see photonic-mcp's `VideoEngineHandle`).
        // GUI mode instead shares the winit renderer's device via
        // `EngineBridge::from_renderer` (see `resumed`). Unifying the GUI
        // process's MCP engine with the GUI's shared-device engine is a
        // follow-up seam (two engines in one process work, but pay double GPU
        // memory for the same media).
        info!("Running in headless mode (MCP server only)");
        let rt = tokio::runtime::Builder::new_multi_thread()
            // Large match in dispatch_tool_inner + schema_gen need more than
            // the default ~2 MiB worker stack (else search_actions / full list overflow).
            .thread_stack_size(8 * 1024 * 1024)
            .enable_all()
            .build()?;
        let mcp_server = McpServer::new(
            Arc::clone(&document_arc),
            Arc::clone(&history_arc),
            capture_tx,
            mcp_config,
            Arc::new(AtomicBool::new(false)),
            audit_log,
        )
        .with_document_path(Arc::clone(&mcp_document_path));
        rt.block_on(mcp_server.run())?;
        return Ok(());
    }

    // ── GUI mode: winit on the main thread; MCP starts after GPU initialization ─
    let mcp_running = Arc::new(AtomicBool::new(false));
    let mcp_restart_requested = Arc::new(AtomicBool::new(false));
    let mcp_state = spawn_mcp_server(
        Arc::clone(&document_arc),
        Arc::clone(&history_arc),
        capture_tx.clone(),
        mcp_config.clone(),
        Arc::clone(&mcp_running),
        Arc::clone(&audit_log),
        Arc::clone(&mcp_document_path),
    );

    // winit 0.30's Wayland backend does not emit file-drop events. Keep the
    // default backend selection unchanged, but let an explicit CLI flag or
    // persisted preference opt into XWayland, where the existing drop handler
    // receives `WindowEvent::DroppedFile`.
    #[allow(unused_mut)]
    let mut event_loop_builder = EventLoop::builder();
    #[cfg(target_os = "linux")]
    if args.x11 || preferences.force_x11_backend {
        info!("Forcing the X11/XWayland winit backend for file drag-and-drop");
        event_loop_builder.with_x11();
    }
    let event_loop = event_loop_builder.build()?;
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut app = PhotonicWinitApp {
        document: document_arc,
        history: history_arc,
        mcp_running,
        mcp_restart_requested,
        mcp_capture_tx: capture_tx,
        mcp_config,
        mcp_document_path,
        mcp_state,
        mcp_result_rx: None,
        capture_rx: Some(capture_rx),
        state: None,
        show_welcome: args.file.is_none(),
        initial_file: args.file.clone(),
        audit_log,
        window_state: WindowState::load(),
        startup_error: None,
    };

    event_loop.run_app(&mut app)?;
    if let Some(error) = app.startup_error {
        return Err(anyhow::anyhow!(error));
    }
    Ok(())
}

// ─── Claude streaming events ─────────────────────────────────────────────────

/// Events streamed from the Claude subprocess thread to the render loop.
enum ClaudeEvent {
    /// A tool was called and returned; show tool name + first line of result.
    ToolResult { name: String, summary: String },
    /// Claude's final text response.
    FinalText(String),
    /// Fatal error (process failed to start, etc.).
    Error(String),
    /// Subprocess exited — no more events will follow.
    Done,
}

// ─── Winit application ───────────────────────────────────────────────────────

struct RenderState {
    window: Arc<Window>,
    renderer: PhotonicRenderer,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
    gui: PhotonicApp,
    lua_repl: LuaRepl,
    /// Receives streaming events from an in-flight `claude` subprocess.
    claude_rx: Option<std::sync::mpsc::Receiver<ClaudeEvent>>,
    /// True after the first message has been sent; subsequent turns use `--continue`.
    claude_session_started: bool,
    /// Undo/redo history shared between the GUI and the MCP server.
    gui_history: Arc<Mutex<CommandHistory>>,
}

struct PhotonicWinitApp {
    document: Arc<Mutex<Document>>,
    history: Arc<Mutex<CommandHistory>>,
    mcp_running: Arc<AtomicBool>,
    /// Set by the GUI's MCP modal to request a server re-spawn; polled each frame
    /// by `maybe_restart_mcp` (#170).
    mcp_restart_requested: Arc<AtomicBool>,
    /// Spawn ingredients retained so the server can be re-created on restart.
    mcp_capture_tx: std::sync::mpsc::Sender<oneshot::Sender<Vec<u8>>>,
    mcp_config: McpServerConfig,
    mcp_document_path: Arc<std::sync::Mutex<Option<std::path::PathBuf>>>,
    mcp_state: AppState,
    mcp_result_rx: Option<std::sync::mpsc::Receiver<(String, Result<(), String>)>>,
    capture_rx: Option<std::sync::mpsc::Receiver<oneshot::Sender<Vec<u8>>>>,
    state: Option<RenderState>,
    show_welcome: bool,
    /// File to mark as `current_file` in the GUI once the window is ready.
    initial_file: Option<std::path::PathBuf>,
    /// Shared audit log — passed to the GUI panel for display.
    audit_log: Arc<std::sync::Mutex<AuditLog>>,
    /// Last normal bounds and maximized state, persisted between launches.
    window_state: WindowState,
    /// Renderer initialization failure reported after the event loop exits.
    startup_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct WindowState {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    maximized: bool,
    has_position: bool,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            x: 0,
            y: 0,
            width: 1280,
            height: 800,
            maximized: true,
            has_position: false,
        }
    }
}

impl WindowState {
    fn path() -> Option<std::path::PathBuf> {
        photonic_core::crash_dir().map(|dir| dir.join("window_state.json"))
    }

    fn load() -> Self {
        Self::path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default()
    }

    fn save(&self) {
        let Some(path) = Self::path() else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            if let Err(error) = std::fs::write(&path, json) {
                tracing::warn!(?path, %error, "Failed to save window state");
            }
        }
    }

    fn update_normal_bounds(&mut self, window: &Window) {
        if window.is_maximized() || window.fullscreen().is_some() {
            return;
        }
        if let Ok(position) = window.outer_position() {
            self.x = position.x;
            self.y = position.y;
            self.has_position = true;
        }
        let size = window.inner_size();
        if size.width > 0 && size.height > 0 {
            self.width = size.width;
            self.height = size.height;
        }
    }

    fn position_is_visible(&self, event_loop: &ActiveEventLoop) -> bool {
        if !self.has_position {
            return false;
        }
        event_loop.available_monitors().any(|monitor| {
            let origin = monitor.position();
            let size = monitor.size();
            let left = i64::from(origin.x);
            let top = i64::from(origin.y);
            let right = left + i64::from(size.width);
            let bottom = top + i64::from(size.height);
            let x = i64::from(self.x);
            let y = i64::from(self.y);
            x < right
                && y < bottom
                && x + i64::from(self.width) > left
                && y + i64::from(self.height) > top
        })
    }
}

/// Spawn the MCP server on a detached background thread with its own tokio
/// runtime. On error (e.g. the port is already bound) the thread logs and exits,
/// leaving `running_flag` false so the GUI can offer a restart (#170).
fn spawn_mcp_server(
    document: Arc<Mutex<Document>>,
    history: Arc<Mutex<CommandHistory>>,
    capture_tx: std::sync::mpsc::Sender<oneshot::Sender<Vec<u8>>>,
    config: McpServerConfig,
    running_flag: Arc<AtomicBool>,
    audit: Arc<std::sync::Mutex<AuditLog>>,
    document_path: Arc<std::sync::Mutex<Option<std::path::PathBuf>>>,
) -> AppState {
    let server = McpServer::new(document, history, capture_tx, config, running_flag, audit)
        .with_document_path(document_path);
    let state = server.state.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            // Large match in dispatch_tool_inner + schema_gen need more than
            // the default ~2 MiB worker stack (else search_actions / full list overflow).
            .thread_stack_size(8 * 1024 * 1024)
            .enable_all()
            .build()
            .expect("tokio runtime");
        if let Err(e) = rt.block_on(server.run()) {
            tracing::error!("MCP server error: {}", e);
        }
    });
    state
}

impl PhotonicWinitApp {
    /// If the GUI requested a restart (via the MCP modal) and the server isn't
    /// already up, re-spawn it (#170). Idempotent per request — the flag is
    /// consumed with a swap so a single click yields a single re-spawn.
    fn maybe_restart_mcp(&mut self) {
        if self.mcp_restart_requested.swap(false, Ordering::Relaxed)
            && !self.mcp_running.load(Ordering::Relaxed)
        {
            info!("Restarting MCP server on user request");
            write_mcp_config(self.mcp_config.port, self.mcp_config.secret.as_deref());
            self.mcp_state = spawn_mcp_server(
                Arc::clone(&self.document),
                Arc::clone(&self.history),
                self.mcp_capture_tx.clone(),
                self.mcp_config.clone(),
                Arc::clone(&self.mcp_running),
                Arc::clone(&self.audit_log),
                Arc::clone(&self.mcp_document_path),
            );
        }
    }

    /// Publish the result of the last palette-triggered MCP operation without
    /// blocking the render loop. The receiver is kept on the app until the
    /// worker actually completes; polling a freshly-created receiver would
    /// otherwise drop every result before it can be observed.
    fn poll_mcp_operation_result(
        result_rx: &mut Option<std::sync::mpsc::Receiver<(String, Result<(), String>)>>,
        gui: &mut PhotonicApp,
    ) {
        let completion = result_rx.as_ref().and_then(|rx| match rx.try_recv() {
            Ok(result) => Some(Ok(result)),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Some(Err("MCP worker disconnected".to_string()))
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
        });

        let Some(completion) = completion else { return };
        *result_rx = None;

        match completion {
            Ok((tool, Ok(()))) => {
                gui.set_mcp_operation_status(format!("MCP operation completed: {tool}"));
            }
            Ok((tool, Err(error))) => {
                gui.set_mcp_operation_status(format!("MCP operation failed ({tool}): {error}"));
            }
            Err(error) => gui.set_mcp_operation_status(format!("MCP operation failed: {error}")),
        }
    }

    /// Start one argumentless MCP operation selected from the command palette.
    /// The palette only exposes tools whose schema has no required fields, so
    /// dispatching `{}` is intentional and cannot silently omit required data.
    fn start_mcp_operation(
        result_rx: &mut Option<std::sync::mpsc::Receiver<(String, Result<(), String>)>>,
        mcp_state: &AppState,
        gui: &mut PhotonicApp,
        tool: String,
    ) {
        if result_rx.is_some() {
            gui.set_mcp_operation_status("An MCP operation is already running".to_string());
            return;
        }

        let mcp_state = mcp_state.clone();
        let result_tool = tool.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        *result_rx = Some(rx);
        std::thread::spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())
                .and_then(|rt| {
                    rt.block_on(photonic_mcp::dispatch::dispatch_tool(
                        &mcp_state,
                        &tool,
                        serde_json::json!({}),
                    ))
                    .map(|_| ())
                });
            let _ = tx.send((result_tool, result));
        });
    }
}

impl ApplicationHandler for PhotonicWinitApp {
    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: winit::event::StartCause) {
        if matches!(cause, winit::event::StartCause::ResumeTimeReached { .. }) {
            if let Some(state) = &self.state {
                state.window.request_redraw();
            }
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }

        let window_icon = load_window_icon();
        #[allow(unused_mut)]
        let mut attrs = WindowAttributes::default()
            .with_title("Photonic")
            .with_maximized(self.window_state.maximized)
            .with_window_icon(window_icon);
        // Only pin an explicit inner size / position when NOT opening maximized.
        // Setting both `with_inner_size` and `with_maximized(true)` gives the
        // compositor two conflicting targets (the saved normal size vs. the
        // maximized size); on multi-monitor + fractional-scaling KWin that
        // conflict can oscillate forever (endless resize/relayout). A maximized
        // window derives its size from the output, so the saved bounds are only
        // needed for the un-maximized (restore) case.
        if !self.window_state.maximized {
            attrs = attrs.with_inner_size(PhysicalSize::new(
                self.window_state.width.clamp(320, 16_384),
                self.window_state.height.clamp(240, 16_384),
            ));
            if self.window_state.position_is_visible(event_loop) {
                attrs = attrs.with_position(winit::dpi::PhysicalPosition::new(
                    self.window_state.x,
                    self.window_state.y,
                ));
            } else if let Some(primary_monitor) = event_loop.primary_monitor() {
                attrs = attrs.with_position(primary_monitor.position());
            }
        }
        // On Linux the compositor (esp. Wayland/KWin) ignores the embedded .ico
        // for the titlebar/taskbar icon and instead maps the window to a desktop
        // file by its app_id / WM class. Set both to "photonic" so it resolves
        // photonic.desktop and uses its (improved) themed icon.
        #[cfg(target_os = "linux")]
        {
            use winit::platform::wayland::WindowAttributesExtWayland;
            use winit::platform::x11::WindowAttributesExtX11;
            attrs = WindowAttributesExtWayland::with_name(attrs, "photonic", "photonic");
            attrs = WindowAttributesExtX11::with_name(attrs, "photonic", "photonic");
        }
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .expect("Failed to create window"),
        );

        let capture_rx = self.capture_rx.take().expect("capture_rx already consumed");

        let renderer = match pollster::block_on(PhotonicRenderer::new(
            Arc::clone(&window),
            Arc::clone(&self.document),
            Arc::clone(&self.history),
            capture_rx,
        )) {
            Ok(renderer) => renderer,
            Err(error) => {
                let message = format!("GPU renderer initialization failed: {error}");
                error!("{message}");
                self.startup_error = Some(message);
                event_loop.exit();
                return;
            }
        };

        // Share the windowed renderer's GPU device/queue with the MCP export path
        // so `export_artboards`/`export_raster` render on the SAME GPU context. A
        // second wgpu device alongside the presenting surface device crashed the
        // running app; reusing this one avoids the conflict entirely.
        photonic_mcp::register_export_gpu(renderer.device_arc(), renderer.queue_arc());

        // ── Lua REPL (binds to the live document) ────────────────────────────
        let lua_repl = match LuaRepl::new(Arc::clone(&self.document), Arc::clone(&self.history)) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("Lua REPL init failed: {e}");
                LuaRepl::new_empty()
            }
        };

        // ── egui setup ───────────────────────────────────────────────────────
        let egui_ctx = egui::Context::default();
        egui_ctx.set_visuals(photonic_gui::build_dark_theme());
        // Spacing (incl. the 24px WCAG SC 2.5.8 hit-target floor, 41 §5 R-9)
        // persists across theme switches: `set_visuals` replaces only
        // `Style::visuals`, leaving `Style::spacing` intact.
        egui_ctx.style_mut(photonic_gui::theme::apply_spacing);

        let mut fonts = egui::FontDefinitions::default();
        egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
        egui_ctx.set_fonts(fonts);

        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            egui::ViewportId::ROOT,
            window.as_ref(),
            Some(window.scale_factor() as f32),
            None,
            None,
        );

        let mut egui_renderer =
            egui_wgpu::Renderer::new(renderer.device(), renderer.surface_format(), None, 1, false);
        // Install the Lightfall background shader pipeline so the welcome screens
        // can render it via an egui paint callback.
        egui_renderer
            .callback_resources
            .insert(photonic_gui::lightfall::LightfallResources::new(
                renderer.device(),
                renderer.queue(),
                renderer.surface_format(),
            ));

        write_mcp_config(self.mcp_config.port, self.mcp_config.secret.as_deref());
        info!("GPU renderer + egui initialized — window open");
        window.request_redraw();

        let mut gui = if self.show_welcome {
            PhotonicApp::new_with_welcome()
        } else {
            PhotonicApp::new()
        };
        gui.audit.log = Some(Arc::clone(&self.audit_log));
        gui.mcp_restart_requested = Some(Arc::clone(&self.mcp_restart_requested));

        // ── Video engine (video-editor 02 §1) ─────────────────────────────────
        // One engine per process, sharing the winit renderer's wgpu device and
        // queue so `EngineFrame` textures can be sampled by the egui pass
        // directly (03 §5). The bridge owns the engine thread; dropping the
        // GUI shuts it down and joins.
        gui.engine = Some(photonic_gui::EngineBridge::from_renderer(&renderer));
        info!("Video engine session opened (shared wgpu device)");

        self.state = Some(RenderState {
            window,
            renderer,
            egui_ctx,
            egui_state,
            egui_renderer,
            gui,
            lua_repl,
            claude_rx: None,
            claude_session_started: false,
            gui_history: Arc::clone(&self.history),
        });

        // If we were launched with a file path, tell the GUI which file is open.
        if let Some(path) = self.initial_file.take() {
            if let Some(state) = &mut self.state {
                if let Ok(doc) = self.document.try_lock() {
                    state.gui.welcome.add_recent(path.clone(), doc.name.clone());
                }
                state.gui.current_file = native_project_path(&path);
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(state) = &mut self.state else { return };

        let response = state.egui_state.on_window_event(&state.window, &event);
        if let Some(paste_in_place) =
            native_clipboard_paste_request(&event, state.egui_state.egui_input().modifiers)
        {
            if let Some(payload) = read_native_clipboard() {
                state
                    .gui
                    .queue_native_clipboard_paste(payload, paste_in_place);
            }
        }

        match event {
            WindowEvent::CloseRequested => {
                // If the GUI holds unsaved work, defer the quit and let it show the
                // Save all / Discard all / Cancel prompt. The GUI sets
                // `close_confirmed` once the user resolves it, which we act on in
                // the RedrawRequested arm below.
                if state.gui.has_unsaved_changes() {
                    state.gui.close_requested = true;
                    state.window.request_redraw();
                    return;
                }
                info!("Window closed");
                self.window_state.update_normal_bounds(&state.window);
                self.window_state.maximized = state.window.is_maximized();
                self.window_state.save();
                // Flush preferences so settings changed right before quitting
                // (e.g. the history limit) survive to the next launch.
                state.gui.prefs.save();
                event_loop.exit();
            }
            WindowEvent::Resized(PhysicalSize { width, height }) => {
                state.renderer.resize(width, height);
                // The canvas-clip rect the GUI last reported belongs to the old
                // window size. Drop it rather than clip the document with it —
                // an unclipped frame just lets the artboard reach a few pixels
                // it shouldn't; a wrongly clipped one blanks most of the canvas.
                state.renderer.set_canvas_scissor(None);
                self.window_state.update_normal_bounds(&state.window);
                state.window.request_redraw();
            }
            WindowEvent::Moved(_) => {
                self.window_state.update_normal_bounds(&state.window);
            }
            WindowEvent::RedrawRequested => {
                let repaint_after = match self.render_frame() {
                    Ok(repaint_after) => repaint_after,
                    Err(error) => {
                        error!(%error, "GPU surface failed; closing Photonic cleanly");
                        event_loop.exit();
                        return;
                    }
                };
                // A deferred quit (unsaved-changes prompt) may have been confirmed
                // by the GUI this frame — finalize window state + prefs and exit.
                let exit_window = self.state.as_ref().and_then(|s| {
                    if s.gui.close_confirmed {
                        Some(s.window.clone())
                    } else {
                        None
                    }
                });
                if let Some(win) = exit_window {
                    info!("Quit confirmed after unsaved-changes prompt");
                    if let Some(s) = &self.state {
                        s.gui.prefs.save();
                    }
                    self.window_state.update_normal_bounds(&win);
                    self.window_state.maximized = win.is_maximized();
                    self.window_state.save();
                    event_loop.exit();
                    return;
                }
                // Do not wake at a fixed 60 Hz when the editor is idle. egui
                // reports the earliest requested repaint for animations,
                // playback, and delayed UI work. `ResumeTimeReached` above
                // turns that deadline into the next native redraw, while input
                // and window events still request an immediate repaint.
                //
                // This is especially important on Wayland: a permanent timer
                // keeps the compositor and GPU busy even with a static window
                // and magnifies any layout/cache feedback into needless work.
                event_loop.set_control_flow(match repaint_after {
                    Some(delay) => ControlFlow::WaitUntil(Instant::now() + delay),
                    None => ControlFlow::Wait,
                });
            }
            _ => {
                if response.repaint {
                    state.window.request_redraw();
                }
            }
        }
    }
}

impl PhotonicWinitApp {
    /// Render one frame and return egui's next requested repaint deadline.
    /// `None` means there is no outstanding UI/playback work, so the native
    /// event loop may sleep until real input or a window event arrives.
    fn render_frame(&mut self) -> Result<Option<Duration>> {
        // Honor a pending MCP restart request from the GUI modal (#170) before we
        // take a mutable borrow of `self.state` for the frame.
        self.maybe_restart_mcp();
        let mcp_document_path = Arc::clone(&self.mcp_document_path);
        let Some(state) = &mut self.state else {
            return Ok(None);
        };

        // Keep GUI File → Save and MCP save_document pointed at the same native
        // file. MCP writes are picked up before drawing; GUI path changes are
        // published after drawing below.
        if let Ok(path) = mcp_document_path.lock() {
            if state.gui.current_file != *path {
                state.gui.current_file = path.clone();
            }
        }
        let gui_path_before = state.gui.current_file.clone();

        // 1. Build document geometry + push camera
        //
        // Clip the document present to the canvas viewport the GUI reported last
        // frame. The document pass covers the whole window; egui's panels used to
        // hide all of it that isn't canvas, but the rails and drawers are floating
        // cards now and the gaps between them let the (usually white) artboard
        // show through as a bright bar. One frame of lag is harmless — on the very
        // first frame, and for a frame after a resize, the scissor is simply the
        // previous viewport.
        state
            .renderer
            .set_canvas_scissor(state.gui.canvas_viewport_px());
        let (verts, idxs) = state.renderer.update();

        // 2. Acquire surface frame
        let mut frame = match state.renderer.begin_frame(&verts, &idxs)? {
            Some(frame) => frame,
            None => return Ok(None),
        };

        // 2b. Render text nodes over the document (before egui).
        state.renderer.render_text_pass(&mut frame);

        // 2c. Render Gaussian glow effects (GPU blur passes, additive composite).
        state.renderer.render_gaussian_glow_pass(&mut frame);

        // 2d. Present the newest video EngineFrame (03 §5) into its egui
        // native texture BEFORE the egui pass runs, so the monitor paints the
        // current frame this very frame. No-op when nothing new was published.
        if let Some(bridge) = state.gui.engine.as_mut() {
            bridge.present_latest(
                state.renderer.device(),
                state.renderer.queue(),
                &mut state.egui_renderer,
            );
        }

        // 3. Run egui (doc lock is held only for the duration of this closure)
        let raw_input = state.egui_state.take_egui_input(&state.window);
        let (w, h) = state.renderer.size();

        let doc_arc = Arc::clone(&self.document);
        let mcp_ok = self.mcp_running.load(Ordering::Relaxed);
        let egui_ctx = state.egui_ctx.clone();
        // Keep window position and scale factor up-to-date for the eyedropper.
        let sf = state.window.scale_factor() as f32;
        if let Ok(outer) = state.window.outer_position() {
            state.gui.window_logical_pos =
                ((outer.x as f32 / sf) as i32, (outer.y as f32 / sf) as i32);
        }
        state.gui.window_scale_factor = sf;

        let full_output = egui_ctx.run(raw_input, |ctx| {
            // try_lock — never block; skip GUI draw for this frame if the doc
            // lock is currently held by an MCP handler so the render loop
            // cannot be frozen indefinitely by lock contention.
            if let Ok(mut doc) = doc_arc.try_lock() {
                if let Ok(mut hist) = state.gui_history.try_lock() {
                    let mut view = state.renderer.view.clone();
                    state.gui.draw(
                        ctx,
                        &mut doc,
                        &mut view,
                        &mut state.renderer,
                        mcp_ok,
                        &mut hist,
                    );
                    state.renderer.view = view;
                }
            }
        });
        let repaint_after = full_output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map(|output| output.repaint_delay)
            .filter(|delay| *delay != Duration::MAX);
        // doc lock released here ↑

        Self::poll_mcp_operation_result(&mut self.mcp_result_rx, &mut state.gui);
        if let Some(tool) = state.gui.take_mcp_operation_request() {
            Self::start_mcp_operation(
                &mut self.mcp_result_rx,
                &self.mcp_state,
                &mut state.gui,
                tool,
            );
        }

        if state.gui.current_file != gui_path_before {
            if let Ok(mut path) = mcp_document_path.lock() {
                *path = state.gui.current_file.clone();
            }
        }

        // Flush debounced checkpoint: if a user action happened ≥30 s ago with
        // no further actions since, write the snapshot now.
        if let Ok(doc) = doc_arc.try_lock() {
            if let Ok(mut hist) = state.gui_history.try_lock() {
                hist.tick_checkpoint(&doc);
            }
        }

        // 4. Execute any Lua code queued by the console (doc lock is FREE now)
        if let Some(code) = state.gui.lua_console.pending.take() {
            let (prints, error) = state.lua_repl.eval(&code);
            for line in prints {
                state.gui.lua_console.log.push((false, line));
            }
            if let Some(err) = error {
                state
                    .gui
                    .lua_console
                    .log
                    .push((true, format!("Error: {err}")));
            }
        }

        // 4b. Dispatch a pending Claude message via `claude` subprocess.
        if let Some(user_msg) = state.gui.claude_chat.pending.take() {
            tracing::info!(
                "Dispatching Claude: {:?} (first={})",
                &user_msg[..user_msg.len().min(60)],
                !state.claude_session_started
            );
            let is_first = !state.claude_session_started;
            state.claude_session_started = true;
            let (tx, rx) = std::sync::mpsc::channel::<ClaudeEvent>();
            state.claude_rx = Some(rx);
            std::thread::spawn(move || {
                run_claude_stream(user_msg, is_first, tx);
            });
        }

        // Drain all available Claude events — stream them into the chat as they arrive.
        if let Some(rx) = &state.claude_rx {
            loop {
                match rx.try_recv() {
                    Ok(ClaudeEvent::ToolResult { name, summary }) => {
                        let icon = tool_icon(&name);
                        let first_line = summary.lines().next().unwrap_or("").trim();
                        let msg = if first_line.is_empty() {
                            format!("{icon} {name}")
                        } else {
                            format!("{icon} {name} — {first_line}")
                        };
                        tracing::debug!("Claude tool: {name}");
                        state.gui.claude_chat.messages.push((false, msg));
                        state.window.request_redraw();
                    }
                    Ok(ClaudeEvent::FinalText(text)) => {
                        tracing::info!("Claude final response ({} chars)", text.len());
                        state.gui.claude_chat.messages.push((false, text));
                        state.window.request_redraw();
                    }
                    Ok(ClaudeEvent::Error(e)) => {
                        tracing::warn!("Claude error: {}", e);
                        state
                            .gui
                            .claude_chat
                            .messages
                            .push((false, format!("⚠ {e}")));
                        state.window.request_redraw();
                    }
                    Ok(ClaudeEvent::Done) => {
                        state.claude_rx = None;
                        state.gui.claude_chat.busy = false;
                        // Snapshot the document after each AI session so the
                        // change log reflects AI-driven edits.
                        if let Ok(doc) = doc_arc.try_lock() {
                            if let Ok(mut hist) = state.gui_history.try_lock() {
                                hist.create_checkpoint("AI edit".to_string(), &doc);
                            }
                        }
                        state.window.request_redraw();
                        break;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        state.claude_rx = None;
                        state.gui.claude_chat.busy = false;
                        state.window.request_redraw();
                        break;
                    }
                }
            }
        }

        state
            .egui_state
            .handle_platform_output(&state.window, full_output.platform_output);

        // 5. Tessellate + prepare egui resources
        let tris = state
            .egui_ctx
            .tessellate(full_output.shapes, full_output.pixels_per_point);
        // Scale the GPU pass with the *same* pixels-per-point egui tessellated
        // at (native scale factor × user zoom). Using the bare native scale
        // factor here while egui tessellates at the zoomed ppp would draw the UI
        // at the wrong size/offset on any non-1.0-scale monitor.
        let screen_desc = ScreenDescriptor {
            size_in_pixels: [w, h],
            pixels_per_point: full_output.pixels_per_point,
        };

        for (id, delta) in &full_output.textures_delta.set {
            state.egui_renderer.update_texture(
                state.renderer.device(),
                state.renderer.queue(),
                *id,
                delta,
            );
        }

        let extra_cmds = state.egui_renderer.update_buffers(
            state.renderer.device(),
            state.renderer.queue(),
            &mut frame.encoder,
            &tris,
            &screen_desc,
        );
        if !extra_cmds.is_empty() {
            state.renderer.queue().submit(extra_cmds);
        }

        // 6. egui render pass (LoadOp::Load — draws over the document)
        {
            let mut rpass = frame
                .encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("egui_pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &frame.view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                })
                .forget_lifetime();
            state.egui_renderer.render(&mut rpass, &tris, &screen_desc);
        }

        for id in &full_output.textures_delta.free {
            state.egui_renderer.free_texture(id);
        }

        // 7. Submit + present
        state.renderer.finish_frame(frame);

        // 8. Service screenshot requests
        state.renderer.service_captures(&verts, &idxs);

        // Periodic heartbeat so we can see the render loop is alive in logs
        {
            use std::sync::atomic::{AtomicU64, Ordering};
            static FRAME: AtomicU64 = AtomicU64::new(0);
            let n = FRAME.fetch_add(1, Ordering::Relaxed);
            if n.is_multiple_of(600) {
                tracing::info!("render loop alive — frame {}", n);
            }
        }
        Ok(repaint_after)
    }
}

// ─── Windows file association ────────────────────────────────────────────────

/// Register `.photon` files with the current user's shell (HKCU — no elevation
/// required).  After this, Explorer shows the Photonic icon for `.photon` files
/// and double-clicking opens them in Photonic.
///
/// Safe to call on every launch; it is idempotent and only touches HKCU keys
/// owned by this application.
#[cfg(windows)]
fn register_file_association() {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};
    use winreg::RegKey;

    let exe = match std::env::current_exe() {
        Ok(p) => p.to_string_lossy().into_owned(),
        Err(e) => {
            tracing::warn!("file assoc: could not get exe path: {e}");
            return;
        }
    };

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let classes = match hkcu.open_subkey_with_flags("Software\\Classes", KEY_SET_VALUE) {
        Ok(k) => k,
        Err(e) => {
            tracing::warn!("file assoc: could not open HKCU\\Software\\Classes: {e}");
            return;
        }
    };

    // .photon → ProgID
    if let Ok((ext, _)) = classes.create_subkey(".photon") {
        let _ = ext.set_value("", &"PhotonicDocument");
    }

    // ProgID description
    if let Ok((prog, _)) = classes.create_subkey("PhotonicDocument") {
        let _ = prog.set_value("", &"Photonic Document");

        // Icon — first resource icon in the exe (the one we embedded via winresource)
        if let Ok((icon, _)) = prog.create_subkey("DefaultIcon") {
            let _ = icon.set_value("", &format!("\"{exe}\",0"));
        }

        // Open command
        if let Ok((shell, _)) = prog.create_subkey("shell\\open\\command") {
            let _ = shell.set_value("", &format!("\"{exe}\" \"%1\""));
        }
    }

    // Notify the shell so changes take effect without a log-off.
    unsafe {
        windows_sys::Win32::UI::Shell::SHChangeNotify(
            windows_sys::Win32::UI::Shell::SHCNE_ASSOCCHANGED as i32,
            windows_sys::Win32::UI::Shell::SHCNF_IDLIST,
            std::ptr::null(),
            std::ptr::null(),
        );
    }

    tracing::info!("file assoc: .photon registered → PhotonicDocument");
}

#[cfg(not(windows))]
fn register_file_association() {}

// ─── Window icon ─────────────────────────────────────────────────────────────

/// Load the bundled ICO file and decode the largest 32×32 (or largest available)
/// RGBA frame for use as the winit window icon.
fn load_window_icon() -> Option<winit::window::Icon> {
    let ico_bytes = include_bytes!("../assets/photonic.ico");
    let img = match image::load_from_memory_with_format(ico_bytes, image::ImageFormat::Ico) {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!("window icon: failed to decode ICO: {e}");
            return None;
        }
    };
    let rgba = img.into_rgba8();
    let (w, h) = rgba.dimensions();
    tracing::info!("window icon: loaded {}×{}", w, h);
    match winit::window::Icon::from_rgba(rgba.into_raw(), w, h) {
        Ok(icon) => Some(icon),
        Err(e) => {
            tracing::warn!("window icon: failed to create: {e}");
            None
        }
    }
}

// ─── Claude subprocess helpers ───────────────────────────────────────────────

/// Build the system prompt for a Claude session.
/// Tells Claude to use the registered Photonic MCP tools directly — no Bash/CLI needed.
fn photonic_skill() -> String {
    "You are an AI design assistant embedded inside Photonic, a vector graphics editor.\n\
\n\
TOOL ACCESS: Your Photonic MCP tools (create_shape, create_path, get_document_state, \
screenshot, update_node, etc.) are registered natively — call them directly as tools. \
NEVER use Bash, shell commands, or JSON-RPC to interact with Photonic. \
NEVER invoke the photonic-plan skill — it is for scratch builds only.\n\
\n\
Canvas: 1123 × 794 px, origin (0,0) top-left, centre ≈ (561, 397).\n\
\n\
Fill format: {\"type\":\"solid\",\"color\":\"#rrggbb\"} | {\"type\":\"none\"} | \
{\"type\":\"gradient\",\"gradient_type\":\"linear\"|\"radial\",\"colors\":[\"#hex1\",\"#hex2\"]}\n\
Stroke format: {\"color\":\"#rrggbb\",\"width\":2,\"enabled\":true}\n\
\n\
IMPROVEMENT WORKFLOW (use when editing an existing design):\n\
1. Call get_document_state AND screenshot in parallel — understand what exists.\n\
2. Make targeted changes: update_node to change colors/opacity, reorder_node for z-order, \
   create shapes to add elements, delete_nodes only for things being replaced.\n\
3. Preserve existing nodes unless a specific node is being replaced. Do not wipe and redraw.\n\
4. Take a final screenshot to confirm the result.\n\
\n\
CREATION WORKFLOW (use when building from scratch):\n\
1. get_document_state + screenshot in parallel.\n\
2. create_layer for each semantic group (background, base, detail, highlight).\n\
3. Draw back-to-front. Group each component after completing it.\n\
4. Take a final screenshot to confirm the result.\n\
\n\
Speed: batch independent tool calls into the same turn for parallel execution. \
Skip intermediate screenshots unless you need visual feedback to proceed."
        .to_string()
}

/// Register the Photonic MCP server in the user's Claude `~/.claude.json` so it
/// is always available when `claude` runs.
///
/// Uses the HTTP transport — Claude Code connects directly to the already-running
/// Photonic MCP HTTP server on the configured port. No proxy subprocess needed.
fn write_mcp_config(port: u16, secret: Option<&str>) {
    let server_entry = mcp_server_entry(port, secret);

    let Some(path) = claude_settings_path() else {
        return;
    };

    match write_mcp_config_at(&path, server_entry) {
        Ok(()) => info!(
            "Registered Photonic MCP server in Claude settings at {:?}",
            path
        ),
        Err(error) => tracing::warn!("Could not update Claude settings at {:?}: {error}", path),
    }
}

fn mcp_server_entry(port: u16, secret: Option<&str>) -> serde_json::Value {
    let mut server_entry = serde_json::json!({
        "type": "http",
        "url": format!("http://127.0.0.1:{port}/mcp")
    });
    if let Some(secret) = secret {
        server_entry["headers"] = serde_json::json!({ MCP_SECRET_HEADER: secret });
    }
    server_entry
}

/// Add Photonic's MCP entry to one Claude configuration file.
///
/// A missing file is initialized, but an existing file must be readable,
/// valid JSON, and an object before it can be changed. This prevents a
/// malformed configuration from being overwritten with a file containing only
/// Photonic's server.
fn write_mcp_config_at(
    path: &std::path::Path,
    server_entry: serde_json::Value,
) -> std::io::Result<()> {
    let mut settings = match std::fs::read(path) {
        Ok(contents) => serde_json::from_slice(&contents).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid JSON: {error}"),
            )
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(error) => return Err(error),
    };

    let Some(settings_object) = settings.as_object_mut() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Claude settings must be a JSON object",
        ));
    };

    let mcp_servers = settings_object
        .entry("mcpServers".to_owned())
        .or_insert_with(|| serde_json::json!({}));
    let Some(mcp_servers) = mcp_servers.as_object_mut() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Claude settings mcpServers must be a JSON object",
        ));
    };
    mcp_servers.insert("photonic".to_owned(), server_entry);

    let contents = serde_json::to_vec_pretty(&settings).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("could not serialize Claude settings: {error}"),
        )
    })?;
    write_private_claude_settings(path, &contents)
}

fn write_private_claude_settings(path: &std::path::Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

        options.mode(0o600);
        let mut file = options.open(path)?;
        // `mode` only applies to newly-created files. Tighten an existing file
        // before writing a secret so a permissive prior mode has no exposure
        // window while the new contents are being written.
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(contents)?;
        file.sync_all()
    }

    #[cfg(not(unix))]
    {
        // Windows user-profile files inherit the profile directory's ACL.
        let mut file = options.open(path)?;
        file.write_all(contents)?;
        file.sync_all()
    }
}

/// Return the path to Claude Code's `~/.claude.json`.
///
/// This is the primary Claude Code configuration file that stores MCP server
/// registrations, user preferences, and project state.  It is distinct from
/// `~/.claude/settings.json` which only holds model/permission settings.
fn claude_settings_path() -> Option<std::path::PathBuf> {
    // Claude Code always uses ~/.claude.json (note: NOT ~/.claude/settings.json).
    // On Windows, prefer USERPROFILE over HOME for the home directory.
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok()?;
    Some(std::path::PathBuf::from(home).join(".claude.json"))
}

/// Build a PATH string that includes Node.js and npm directories so that
/// subprocesses spawned from a GUI process (which may inherit a stripped PATH)
/// can resolve `node` and npm-installed shims like `claude.cmd`.
#[cfg(windows)]
fn augmented_path() -> String {
    let current = std::env::var("PATH").unwrap_or_default();
    let mut extras: Vec<String> = Vec::new();

    // npm global bin dir
    if let Ok(appdata) = std::env::var("APPDATA") {
        extras.push(format!("{appdata}\\npm"));
    }

    // Common Node.js install locations
    for candidate in &[r"C:\Program Files\nodejs", r"C:\Program Files (x86)\nodejs"] {
        if std::path::Path::new(candidate).exists() {
            extras.push(candidate.to_string());
        }
    }

    // nvm on Windows typically lives in %APPDATA%\nvm
    if let Ok(appdata) = std::env::var("APPDATA") {
        let nvm_root = std::path::PathBuf::from(&appdata).join("nvm");
        if nvm_root.exists() {
            // Add the currently-active version dir (first subdir found)
            if let Ok(mut entries) = std::fs::read_dir(&nvm_root) {
                if let Some(Ok(entry)) = entries.next() {
                    extras.push(entry.path().to_string_lossy().into_owned());
                }
            }
        }
    }

    if extras.is_empty() {
        current
    } else {
        format!("{};{}", extras.join(";"), current)
    }
}

/// Find the `claude` executable, handling Windows npm installs where the binary
/// is a `.cmd` shim not visible to CreateProcess without a full path.
fn find_claude() -> Result<std::process::Command, String> {
    #[cfg(windows)]
    {
        let path_env = augmented_path();

        // 1. Check %APPDATA%\npm\claude.cmd — standard npm global install
        if let Ok(appdata) = std::env::var("APPDATA") {
            let p = std::path::PathBuf::from(&appdata)
                .join("npm")
                .join("claude.cmd");
            if p.exists() {
                let mut c = std::process::Command::new("cmd");
                c.args(["/c", &p.to_string_lossy().into_owned()]);
                c.env("PATH", &path_env);
                return Ok(c);
            }
        }
        // 2. Ask cmd.exe where it lives (works if PATH happens to be inherited)
        if let Ok(out) = std::process::Command::new("cmd")
            .args(["/c", "where", "claude"])
            .env("PATH", &path_env)
            .output()
        {
            if let Some(line) = String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                let mut c = std::process::Command::new("cmd");
                c.args(["/c", line]);
                c.env("PATH", &path_env);
                return Ok(c);
            }
        }
        return Err(
            "claude not found — install with: npm install -g @anthropic-ai/claude-code".into(),
        );
    }
    #[cfg(not(windows))]
    Ok(std::process::Command::new("claude"))
}

/// Emoji prefix for each MCP tool name shown in the chat stream.
///
/// Claude Code prefixes MCP tools as `mcp__<server>__<tool>` — strip that
/// prefix before matching so both bare names and namespaced names work.
fn tool_icon(name: &str) -> &'static str {
    // Strip the `mcp__photonic__` namespace prefix added by Claude Code.
    let bare = name
        .strip_prefix("mcp__photonic__")
        .or_else(|| name.strip_prefix("mcp__"))
        .unwrap_or(name);
    match bare {
        "screenshot" => "📸",
        "get_document_state" | "get_node" => "📋",
        "create_shape" | "create_path" | "build_shape_from_points" => "✏",
        "update_node" => "✎",
        "delete_nodes" => "✗",
        "apply_transform" => "⟳",
        "reorder_node" => "⇅",
        "group_nodes" | "ungroup_nodes" => "⊞",
        "boolean_operation" => "∩",
        "create_layer" => "▤",
        "undo" | "redo" => "↩",
        "create_checkpoint" | "list_checkpoints" | "restore_checkpoint" => "◈",
        _ => "·",
    }
}

/// Spawn `claude` with `--output-format stream-json` and forward events to `tx`
/// as they arrive so the UI can render progress in real time.
fn run_claude_stream(user_msg: String, is_first: bool, tx: std::sync::mpsc::Sender<ClaudeEvent>) {
    use std::io::BufRead;

    let skill = photonic_skill();
    // settings.json was already updated at startup via write_mcp_config().
    // Passing --mcp-config on top of that caused duplicate server registration,
    // which Claude Code treats as a conflict and silently drops the tools.
    // Rely solely on settings.json here.

    let mut cmd = match find_claude() {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(ClaudeEvent::Error(e));
            let _ = tx.send(ClaudeEvent::Done);
            return;
        }
    };

    cmd.arg("-p")
        .arg(&user_msg)
        .arg("--dangerously-skip-permissions")
        .arg("--output-format")
        .arg("stream-json")
        .arg("--verbose")
        .arg("--append-system-prompt")
        .arg(&skill)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());

    if !is_first {
        cmd.arg("--continue");
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(ClaudeEvent::Error(format!("Failed to launch claude: {e}")));
            let _ = tx.send(ClaudeEvent::Done);
            return;
        }
    };

    let stdout = child.stdout.take().expect("stdout was piped");
    let reader = std::io::BufReader::new(stdout);

    // Maps tool_use id → tool name so we can label results when they arrive.
    let mut pending: std::collections::HashMap<String, String> = Default::default();
    let mut got_final = false;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let Ok(ev) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };

        match ev["type"].as_str() {
            // Note which tools were called so we can label their results.
            Some("assistant") => {
                if let Some(content) = ev["message"]["content"].as_array() {
                    for item in content {
                        if item["type"] == "tool_use" {
                            let id = item["id"].as_str().unwrap_or("").to_string();
                            let name = item["name"].as_str().unwrap_or("").to_string();
                            if !id.is_empty() && !name.is_empty() {
                                pending.insert(id, name);
                            }
                        }
                    }
                }
            }
            // Emit a ToolResult event as soon as each result arrives.
            Some("user") => {
                if let Some(content) = ev["message"]["content"].as_array() {
                    for item in content {
                        if item["type"] == "tool_result" {
                            let id = item["tool_use_id"].as_str().unwrap_or("").to_string();
                            let summary = item["content"]
                                .as_array()
                                .and_then(|a| a.first())
                                .and_then(|c| c["text"].as_str())
                                .or_else(|| item["content"].as_str())
                                .unwrap_or("")
                                .trim()
                                .to_string();
                            if let Some(name) = pending.remove(&id) {
                                if tx.send(ClaudeEvent::ToolResult { name, summary }).is_err() {
                                    return; // receiver dropped (window closed)
                                }
                            }
                        }
                    }
                }
            }
            // Final assistant reply.
            Some("result") => {
                let text = ev["result"].as_str().unwrap_or("").trim().to_string();
                if !text.is_empty() {
                    got_final = true;
                    let _ = tx.send(ClaudeEvent::FinalText(text));
                }
            }
            _ => {}
        }
    }

    let _ = child.wait();

    if !got_final {
        let _ = tx.send(ClaudeEvent::Error("(no response from claude)".into()));
    }
    let _ = tx.send(ClaudeEvent::Done);
}

#[cfg(test)]
mod mcp_config_tests {
    use super::{mcp_server_entry, write_mcp_config_at, write_private_claude_settings};
    use serde_json::json;
    use std::{fs, io};

    fn test_directory() -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("photonic-claude-config-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).expect("create temporary test directory");
        path
    }

    #[test]
    fn write_mcp_config_uses_the_configured_port() {
        let entry = mcp_server_entry(9000, Some("top-secret"));
        assert_eq!(entry["url"], "http://127.0.0.1:9000/mcp");
        assert_eq!(entry["headers"]["x-mcp-secret"], "top-secret");
    }

    #[test]
    fn malformed_config_is_preserved() {
        let directory = test_directory();
        let path = directory.join("claude.json");
        let original = b"{ not valid JSON\n";
        fs::write(&path, original).expect("write malformed config");

        let error =
            write_mcp_config_at(&path, mcp_server_entry(7842, None)).expect_err("malformed config");

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(fs::read(&path).expect("read config"), original);
        fs::remove_dir_all(directory).expect("remove temporary test directory");
    }

    #[test]
    fn valid_config_preserves_unrelated_settings_and_servers() {
        let directory = test_directory();
        let path = directory.join("claude.json");
        let original = json!({
            "theme": "dark",
            "mcpServers": {"other": {"type": "stdio", "command": "other-server"}}
        });
        fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();

        let entry = mcp_server_entry(9123, Some("test-secret"));
        write_mcp_config_at(&path, entry.clone()).expect("update valid config");

        let updated: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("read updated config")).unwrap();
        assert_eq!(updated["theme"], original["theme"]);
        assert_eq!(
            updated["mcpServers"]["other"],
            original["mcpServers"]["other"]
        );
        assert_eq!(updated["mcpServers"]["photonic"], entry);
        fs::remove_dir_all(directory).expect("remove temporary test directory");
    }

    #[test]
    fn malformed_mcp_servers_structure_is_preserved() {
        let directory = test_directory();
        let path = directory.join("claude.json");
        let original = br#"{"mcpServers":[]}"#;
        fs::write(&path, original).expect("write malformed config");

        let error = write_mcp_config_at(&path, mcp_server_entry(7842, None))
            .expect_err("malformed structure");

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(fs::read(&path).expect("read preserved config"), original);
        fs::remove_dir_all(directory).expect("remove temporary test directory");
    }

    #[cfg(unix)]
    #[test]
    fn claude_settings_are_created_and_updated_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let directory = std::env::temp_dir().join(format!(
            "photonic-private-settings-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(".claude.json");

        write_private_claude_settings(&path, br#"{"first":true}"#).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_private_claude_settings(&path, br#"{"secret":"private"}"#).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(std::fs::read(&path).unwrap(), br#"{"secret":"private"}"#);

        std::fs::remove_dir_all(directory).unwrap();
    }
}
