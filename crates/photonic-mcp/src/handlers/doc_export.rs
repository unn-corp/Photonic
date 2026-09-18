use crate::handlers::shared::{wait_for_capture, CaptureWaitError};
use crate::protocol::{
    AddExportProfileArgs, DeleteLayerArgs, DuplicateLayerArgs, ExportArtboardsArgs,
    ExportDesignTokensArgs, ExportIconSetArgs, ExportPdfArgs, ExportRasterArgs,
    ExportSelectionArgs, ExportSvgArgs, ImportDesignTokensArgs, PreviewSelectionArgs,
    RemoveExportProfileArgs, ReorderLayersArgs, RunExportProfileArgs, SetActiveLayerArgs,
    ToolResult,
};
use crate::server::AppState;
use photonic_core::node::SceneNodeKind;
use photonic_core::raster::validate_raster_dimensions;
use photonic_core::style::{Fill, FillKind};
use std::collections::BTreeSet;

/// Export the document as SVG text.
pub async fn set_active_layer(state: &AppState, args: SetActiveLayerArgs) -> ToolResult {
    tracing::debug!("tool: set_active_layer");
    use photonic_core::history::Command;

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let lid = if let Ok(uuid) = uuid::Uuid::parse_str(&args.layer_id) {
        uuid
    } else {
        match doc.layers.values().find(|l| l.name == args.layer_id) {
            Some(l) => l.id,
            None => return ToolResult::error(format!("Layer not found: {}", args.layer_id)),
        }
    };

    if !doc.layers.contains_key(&lid) {
        return ToolResult::error("Layer not found");
    }

    let old_id = doc.active_layer_id;
    history.execute_discrete(
        Command::SetActiveLayer {
            old_id,
            new_id: Some(lid),
        },
        &mut doc,
    );

    let name = doc
        .layers
        .get(&lid)
        .map(|l| l.name.clone())
        .unwrap_or_default();
    ToolResult::text(format!("Active layer set to '{name}'"))
        .with_data(serde_json::json!({ "layer_id": lid, "name": name }))
}

pub async fn delete_layer(state: &AppState, args: DeleteLayerArgs) -> ToolResult {
    tracing::debug!("tool: delete_layer");
    use photonic_core::history::Command;

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    if doc.layer_order.len() <= 1 {
        return ToolResult::error("Cannot delete the last remaining layer");
    }

    let lid = if let Ok(uuid) = uuid::Uuid::parse_str(&args.layer_id) {
        uuid
    } else {
        match doc.layers.values().find(|l| l.name == args.layer_id) {
            Some(l) => l.id,
            None => return ToolResult::error(format!("Layer not found: {}", args.layer_id)),
        }
    };

    let layer = match doc.layers.get(&lid) {
        Some(l) => l.clone(),
        None => return ToolResult::error("Layer not found"),
    };

    let node_count = layer.node_ids.len();

    if args.delete_nodes {
        // Delete all nodes on the layer first.
        let mut cmds = Vec::new();
        for nid in &layer.node_ids {
            cmds.push(Command::RemoveNode { node_id: *nid });
        }
        cmds.push(Command::RemoveLayerFull {
            layer: layer.clone(),
        });
        history.execute_discrete(Command::Batch(cmds), &mut doc);
    } else {
        // Move nodes to first remaining layer, then delete the empty layer.
        let target_lid = doc
            .layer_order
            .iter()
            .find(|&&id| id != lid)
            .copied()
            .unwrap();

        let mut cmds = Vec::new();
        for (i, nid) in layer.node_ids.iter().enumerate() {
            let target_len = doc
                .layers
                .get(&target_lid)
                .map(|l| l.node_ids.len())
                .unwrap_or(0);
            cmds.push(Command::MoveNodeToLayer {
                node_id: *nid,
                old_layer_id: lid,
                new_layer_id: target_lid,
                old_index: 0, // After each move, index shifts, but we always take from front.
                new_index: target_len + i,
            });
        }
        cmds.push(Command::RemoveLayerFull {
            layer: layer.clone(),
        });
        history.execute_discrete(Command::Batch(cmds), &mut doc);
    }

    let action = if args.delete_nodes {
        "deleted with"
    } else {
        "deleted, moved"
    };
    ToolResult::text(format!(
        "Layer '{}' {} {node_count} node(s)",
        layer.name, action
    ))
    .with_data(serde_json::json!({ "layer_id": lid, "nodes_affected": node_count }))
}

pub async fn reorder_layers(state: &AppState, args: ReorderLayersArgs) -> ToolResult {
    tracing::debug!("tool: reorder_layers");
    use photonic_core::history::Command;

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let new_order: Vec<uuid::Uuid> = match args
        .layer_order
        .iter()
        .map(|s| uuid::Uuid::parse_str(s))
        .collect::<Result<_, _>>()
    {
        Ok(order) => order,
        Err(error) => return ToolResult::error(format!("Invalid layer UUID: {error}")),
    };

    if new_order.len() != doc.layer_order.len() {
        return ToolResult::error(format!(
            "Layer count mismatch: provided {} but document has {}. All layers must be included.",
            new_order.len(),
            doc.layer_order.len()
        ));
    }

    // Verify all IDs are valid layers.
    for lid in &new_order {
        if !doc.layers.contains_key(lid) {
            return ToolResult::error(format!("Layer not found: {lid}"));
        }
    }

    let requested_ids: BTreeSet<_> = new_order.iter().copied().collect();
    let document_ids: BTreeSet<_> = doc.layers.keys().copied().collect();
    if requested_ids.len() != new_order.len() || requested_ids != document_ids {
        return ToolResult::error("Layer order must contain every document layer exactly once.");
    }

    let old_order = doc.layer_order.clone();
    history.execute_discrete(
        Command::ReorderLayers {
            old_order,
            new_order: new_order.clone(),
        },
        &mut doc,
    );

    ToolResult::text(format!("Reordered {} layers", new_order.len()))
        .with_data(serde_json::json!({ "layer_order": new_order }))
}

pub async fn duplicate_layer(state: &AppState, args: DuplicateLayerArgs) -> ToolResult {
    tracing::debug!("tool: duplicate_layer");
    use photonic_core::history::Command;
    use photonic_core::layer::Layer;

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    // Resolve layer.
    let src_layer_id = if let Ok(uuid) = uuid::Uuid::parse_str(&args.layer_id) {
        uuid
    } else {
        match doc.layers.values().find(|l| l.name == args.layer_id) {
            Some(l) => l.id,
            None => return ToolResult::error(format!("Layer not found: {}", args.layer_id)),
        }
    };

    let src_layer = match doc.layers.get(&src_layer_id) {
        Some(l) => l.clone(),
        None => return ToolResult::error("Layer not found"),
    };

    // Create new layer.
    let new_layer_name = args
        .name
        .unwrap_or_else(|| format!("{} Copy", src_layer.name));
    let mut new_layer = Layer::new(&new_layer_name);
    new_layer.visible = src_layer.visible;
    new_layer.opacity = src_layer.opacity;
    new_layer.blend_mode = src_layer.blend_mode;
    new_layer.color = src_layer.color;
    new_layer.is_template = src_layer.is_template;
    let new_layer_id = new_layer.id;

    // Deep-clone all nodes, assigning new IDs.
    let mut commands = Vec::new();
    commands.push(Command::AddLayer { layer: new_layer });

    for &nid in &src_layer.node_ids {
        if let Some(node) = doc.nodes.get(&nid) {
            let mut cloned = node.clone();
            cloned.id = uuid::Uuid::new_v4();
            cloned.name = format!("{} (copy)", node.name);
            cloned.layer_id = new_layer_id;
            commands.push(Command::AddNode {
                node: cloned,
                layer_id: Some(new_layer_id),
            });
        }
    }

    let node_count = src_layer.node_ids.len();
    history.execute_discrete(Command::Batch(commands), &mut doc);

    ToolResult::text(format!(
        "Duplicated layer '{}' → '{}' ({node_count} nodes)",
        src_layer.name, new_layer_name
    ))
    .with_data(serde_json::json!({
        "new_layer_id": new_layer_id,
        "node_count": node_count,
    }))
}

pub async fn export_svg(state: &AppState, args: ExportSvgArgs) -> ToolResult {
    tracing::debug!("tool: export_svg");
    let doc = state.document.lock().await;
    let opts = photonic_core::export::SvgExportOptions {
        semantic_ids: args.semantic_ids.unwrap_or(true),
        precision: args.precision.unwrap_or(4).clamp(1, 6),
        ..Default::default()
    };
    let svg = photonic_core::export::export_svg(&doc, &opts);

    let output = if args.inner_only {
        // Strip the outer <svg ...>...</svg> wrapper, returning only the body.
        svg.find('>')
            .and_then(|start| svg.rfind("</svg>").map(|end| &svg[start + 1..end]))
            .unwrap_or(&svg)
            .trim()
            .to_string()
    } else {
        svg.clone()
    };

    let byte_count = output.len();
    ToolResult::text(format!(
        "SVG export — {} bytes, {}×{} canvas",
        byte_count, doc.width, doc.height
    ))
    .with_data(serde_json::json!({ "svg": output, "bytes": byte_count }))
}

pub async fn export_pdf(state: &AppState, args: ExportPdfArgs) -> ToolResult {
    tracing::debug!("tool: export_pdf");
    let background = match args.background.as_deref() {
        Some(hex) => match photonic_core::color::Color::from_hex(hex) {
            Some(c) => Some(c),
            None => {
                return ToolResult::error(format!("Invalid background '{hex}' (expected #rrggbb)"))
            }
        },
        None => None,
    };

    let icc_profile = args.profile.map(std::path::PathBuf::from);

    let doc = state.document.lock().await;

    // Resolve color_mode: explicit arg wins; when omitted fall back to the document's
    // stored color mode so callers don't have to repeat it every export.
    let color_mode = match args.color_mode.as_deref() {
        Some("cmyk") => photonic_core::document::ColorMode::Cmyk,
        Some("rgb") => photonic_core::document::ColorMode::Rgb,
        Some(other) => {
            return ToolResult::error(format!("color_mode must be 'rgb' or 'cmyk', got '{other}'"))
        }
        None => doc.color_mode,
    };

    // When `outline_text` is requested, convert every text node to vector paths
    // so the exported PDF has zero font dependencies. Work on a throw-away clone;
    // the live document (and its undo history) is untouched.
    let export_doc: std::borrow::Cow<photonic_core::document::Document> =
        if args.outline_text.unwrap_or(false) {
            let mut font_system = glyphon::FontSystem::new();
            std::borrow::Cow::Owned(photonic_render::outline_document_text(
                &doc,
                &mut font_system,
            ))
        } else {
            std::borrow::Cow::Borrowed(&*doc)
        };

    let opts = photonic_core::export::PdfExportOptions {
        background,
        outline_text: args.outline_text.unwrap_or(false),
        marks: args.marks.unwrap_or(false),
        color_mode,
        icc_profile,
    };

    use base64::Engine;

    // ── Per-artboard / multi-page mode ────────────────────────────────────────
    // Triggered when any artboard selector is set. Each artboard becomes a page
    // clipped to its rectangle + bleed (single multi-page PDF by default, or one
    // file per artboard when `separate_files` is set).
    let artboard_mode =
        args.all.unwrap_or(false) || args.range.is_some() || args.artboards.is_some();
    if artboard_mode {
        const MAX_BOARDS: usize = 64;
        let selected = match select_artboards_for_export(
            &doc.artboards,
            doc.active_artboard,
            args.all.unwrap_or(false),
            args.range,
            args.artboards.as_deref(),
            MAX_BOARDS,
        ) {
            Ok(v) => v,
            Err(e) => return ToolResult::error(e),
        };
        let regions: Vec<photonic_core::export::PageRegion> = selected
            .iter()
            .map(photonic_core::export::PageRegion::artboard)
            .collect();

        if args.separate_files.unwrap_or(false) {
            // One file per artboard, using `path` as a template.
            let template = match args.path.as_deref() {
                Some(p) => p,
                None => {
                    return ToolResult::error(
                        "separate_files=true requires `path` (a template with {name}/{index} or a filename)",
                    )
                }
            };
            let mut items = Vec::new();
            let mut total = 0usize;
            for (i, (ab, region)) in selected.iter().zip(regions.iter()).enumerate() {
                let bytes =
                    photonic_core::export::export_pdf_regions(&export_doc, &opts, &[*region]);
                let out_path = expand_path_template(template, &ab.name, i + 1);
                if let Err(e) = std::fs::write(&out_path, &bytes) {
                    return ToolResult::error(format!("Failed to write PDF to '{out_path}': {e}"));
                }
                total += bytes.len();
                items.push(serde_json::json!({
                    "artboard_id": ab.id,
                    "name": ab.name,
                    "path": out_path,
                    "bytes": bytes.len(),
                    "data_base64": base64::engine::general_purpose::STANDARD.encode(&bytes),
                }));
            }
            let n = items.len();
            return ToolResult::text(format!(
                "PDF export — {n} file{} ({total} bytes total), one per artboard",
                if n == 1 { "" } else { "s" }
            ))
            .with_data(serde_json::json!({
                "format": "pdf",
                "mode": "per-file",
                "count": n,
                "mime": "application/pdf",
                "files": items,
            }));
        }

        // Single multi-page PDF.
        let bytes = photonic_core::export::export_pdf_regions(&export_doc, &opts, &regions);
        if let Some(ref path) = args.path {
            if let Err(e) = std::fs::write(path, &bytes) {
                return ToolResult::error(format!("Failed to write PDF to '{path}': {e}"));
            }
        }
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let n = regions.len();
        return ToolResult::text(format!(
            "PDF export — {} bytes, {n} page{} (one per artboard)",
            bytes.len(),
            if n == 1 { "" } else { "s" }
        ))
        .with_data(serde_json::json!({
            "format": "pdf",
            "mode": "multi-page",
            "pages": n,
            "bytes": bytes.len(),
            "mime": "application/pdf",
            "data_base64": b64,
        }));
    }

    // ── Whole-canvas single page ──────────────────────────────────────────────
    let bytes = photonic_core::export::export_pdf(&export_doc, &opts);

    // Optionally write to a filesystem path.
    if let Some(ref path) = args.path {
        if let Err(e) = std::fs::write(path, &bytes) {
            return ToolResult::error(format!("Failed to write PDF to '{path}': {e}"));
        }
    }

    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let byte_count = bytes.len();
    ToolResult::text(format!(
        "PDF export — {} bytes, {}×{} page",
        byte_count, doc.width, doc.height
    ))
    .with_data(serde_json::json!({
        "format": "pdf",
        "bytes": byte_count,
        "mime": "application/pdf",
        "data_base64": b64,
    }))
}

/// Expand a per-artboard filename template. `{name}` → the (sanitized) artboard
/// name, `{index}`/`{n}` → the 1-based index. If the template has no placeholder,
/// `-{index}` is inserted before the file extension (or appended when none).
fn expand_path_template(template: &str, name: &str, index: usize) -> String {
    let safe_name: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if template.contains("{name}") || template.contains("{index}") || template.contains("{n}") {
        return template
            .replace("{name}", &safe_name)
            .replace("{index}", &index.to_string())
            .replace("{n}", &index.to_string());
    }
    // No placeholder: insert -{index} before the extension.
    match template.rfind('.') {
        Some(dot) if dot > template.rfind(['/', '\\']).map(|s| s + 1).unwrap_or(0) => {
            format!("{}-{index}{}", &template[..dot], &template[dot..])
        }
        _ => format!("{template}-{index}"),
    }
}

/// Write `bytes` to `path`, creating any missing parent directories first.
fn write_file_with_parents(path: &str, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(path, bytes)
}

/// Read a PNG's pixel dimensions straight from its IHDR chunk — no full decode.
/// The byte layout is fixed: 8-byte signature, then the IHDR chunk whose data
/// begins at offset 16 (width, big-endian u32) and 20 (height).
fn png_dimensions(png: &[u8]) -> Option<(u32, u32)> {
    if png.len() < 24 || &png[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
    let h = u32::from_be_bytes([png[20], png[21], png[22], png[23]]);
    Some((w, h))
}

fn validate_export_dimensions(width: Option<u32>, height: Option<u32>) -> Result<(), String> {
    match (width, height) {
        (Some(width), Some(height)) => validate_raster_dimensions(width, height),
        (Some(width), None) => validate_raster_dimensions(width, 1),
        (None, Some(height)) => validate_raster_dimensions(1, height),
        (None, None) => Ok(()),
    }
}

pub async fn export_raster(state: &AppState, args: ExportRasterArgs) -> ToolResult {
    tracing::debug!("tool: export_raster");

    let format = args.format.as_deref().unwrap_or("png");
    let is_jpeg = matches!(format, "jpeg" | "jpg");
    let is_webp = format == "webp";
    let is_gif = format == "gif";
    let is_tiff = matches!(format, "tiff" | "tif");
    if !matches!(
        format,
        "png" | "jpeg" | "jpg" | "webp" | "gif" | "tiff" | "tif"
    ) {
        return ToolResult::error(format!(
            "Unsupported format: '{format}'. Use 'png', 'jpeg', 'webp', 'gif', or 'tiff'."
        ));
    }

    if let Err(error) = validate_export_dimensions(args.width, args.height) {
        return ToolResult::error(format!("Invalid output dimensions: {error}"));
    }

    // Capture a screenshot from the render thread (PNG bytes).
    let (tx, rx) = tokio::sync::oneshot::channel::<Vec<u8>>();
    let sent = state
        .capture_tx
        .lock()
        .map(|tx_guard| tx_guard.send(tx).is_ok())
        .unwrap_or(false);

    if !sent {
        return ToolResult::error("Export unavailable — render request channel is closed");
    }

    let png_bytes = match wait_for_capture(rx).await {
        Ok(bytes) if !bytes.is_empty() => bytes,
        Ok(_) => return ToolResult::error("Render thread returned empty image data"),
        Err(CaptureWaitError::Timeout) => {
            tracing::warn!("tool: export_raster — render thread response timed out");
            return ToolResult::error(
                "Raster export timed out waiting for the render thread to return image data",
            );
        }
        Err(CaptureWaitError::Disconnected) => {
            tracing::warn!("tool: export_raster — render thread closed the response channel");
            return ToolResult::error(
                "Render thread closed the raster export response channel before returning image data",
            );
        }
    };

    // Optionally resize.
    let png_bytes = match (args.width, args.height) {
        (Some(w), Some(h)) => match resize_png(&png_bytes, w, h) {
            Ok(resized) => resized,
            Err(error) => return ToolResult::error(format!("Failed to resize export: {error}")),
        },
        _ => png_bytes,
    };

    // Output dimensions (from the canonical PNG, before any format conversion).
    let (out_w, out_h) = png_dimensions(&png_bytes).unwrap_or((0, 0));

    // Convert format if needed.
    let (final_bytes, mime) = if is_jpeg {
        let quality = args.quality.unwrap_or(90).clamp(1, 100);
        match png_to_jpeg(&png_bytes, quality) {
            Some(jpeg) => (jpeg, "image/jpeg"),
            None => return ToolResult::error("Failed to encode JPEG"),
        }
    } else if is_webp {
        let quality = args.quality.unwrap_or(80).clamp(1, 100);
        match png_to_webp(&png_bytes, quality) {
            Some(webp) => (webp, "image/webp"),
            None => return ToolResult::error("Failed to encode WebP"),
        }
    } else if is_gif {
        match png_to_gif(&png_bytes) {
            Some(gif) => (gif, "image/gif"),
            None => return ToolResult::error("Failed to encode GIF"),
        }
    } else if is_tiff {
        match png_to_tiff(&png_bytes) {
            Some(tiff) => (tiff, "image/tiff"),
            None => return ToolResult::error("Failed to encode TIFF"),
        }
    } else {
        (png_bytes, "image/png")
    };

    let byte_count = final_bytes.len();
    let fmt_label = if is_jpeg {
        "JPEG"
    } else if is_webp {
        "WebP"
    } else if is_gif {
        "GIF"
    } else if is_tiff {
        "TIFF"
    } else {
        "PNG"
    };

    // When `path` is set, write the encoded image to disk and return a SMALL
    // result (path + dimensions) — never the base64, so a full-resolution PNG
    // does not overwhelm the MCP socket. Base64 is only returned when `path`
    // is omitted. Mirrors `export_pdf`'s path handling.
    if let Some(ref path) = args.path {
        if let Err(e) = write_file_with_parents(path, &final_bytes) {
            return ToolResult::error(format!("Failed to write image to '{path}': {e}"));
        }
        return ToolResult::text(format!(
            "{fmt_label} export — {byte_count} bytes → {path} ({out_w}×{out_h})"
        ))
        .with_data(serde_json::json!({
            "format": fmt_label.to_lowercase(),
            "path": path,
            "width": out_w,
            "height": out_h,
            "bytes": byte_count,
            "mime": mime,
        }));
    }

    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&final_bytes);
    ToolResult::text(format!("{fmt_label} export — {byte_count} bytes")).with_data(
        serde_json::json!({
            "format": fmt_label.to_lowercase(),
            "width": out_w,
            "height": out_h,
            "bytes": byte_count,
            "mime": mime,
            "data_base64": b64,
        }),
    )
}

#[cfg(test)]
mod export_raster_capture_tests {
    use super::export_raster;
    use crate::handlers::shared::CAPTURE_RESPONSE_TIMEOUT;
    use crate::protocol::{ContentItem, ExportRasterArgs, ToolResult};
    use crate::server::{AppState, McpServerConfig};
    use photonic_core::{history::CommandHistory, AuditLog, Document};
    use std::sync::{mpsc, Arc, Mutex as StdMutex};
    use tokio::sync::{oneshot, Mutex};

    fn state_with_capture_tx(capture_tx: mpsc::Sender<oneshot::Sender<Vec<u8>>>) -> AppState {
        AppState {
            document: Arc::new(Mutex::new(Document::new("capture test", 200.0, 100.0))),
            history: Arc::new(Mutex::new(CommandHistory::new(100))),
            document_path: Arc::new(StdMutex::new(None)),
            capture_tx: Arc::new(StdMutex::new(capture_tx)),
            config: McpServerConfig::default(),
            audit_log: Arc::new(StdMutex::new(AuditLog::new())),
            clipboard_ring: Arc::new(crate::handlers::clipboard::new_clipboard_ring()),
            ..AppState::headless_for_test()
        }
    }

    fn result_text(result: &ToolResult) -> &str {
        match result.content.first() {
            Some(ContentItem::Text { text }) => text,
            other => panic!("expected a text result, got {other:?}"),
        }
    }

    fn test_png() -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 0, 0, 255]));
        let mut bytes = Vec::new();
        image
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    }

    #[tokio::test(start_paused = true)]
    async fn export_raster_times_out_when_render_loop_does_not_service_request() {
        let (capture_tx, _capture_rx) = mpsc::channel();
        let state = state_with_capture_tx(capture_tx);
        let task =
            tokio::spawn(async move { export_raster(&state, ExportRasterArgs::default()).await });

        tokio::task::yield_now().await;
        tokio::time::advance(CAPTURE_RESPONSE_TIMEOUT).await;
        let result = task.await.unwrap();

        assert_eq!(result.is_error, Some(true));
        assert!(result_text(&result).contains("timed out"));
    }

    #[tokio::test]
    async fn export_raster_reports_request_channel_closure() {
        let (capture_tx, capture_rx) = mpsc::channel();
        drop(capture_rx);
        let state = state_with_capture_tx(capture_tx);

        let result = export_raster(&state, ExportRasterArgs::default()).await;

        assert_eq!(result.is_error, Some(true));
        assert!(result_text(&result).contains("request channel is closed"));
    }

    #[tokio::test]
    async fn export_raster_reports_response_channel_closure() {
        let (capture_tx, capture_rx) = mpsc::channel();
        let state = state_with_capture_tx(capture_tx);
        let service = std::thread::spawn(move || {
            let reply_tx = capture_rx.recv().unwrap();
            drop(reply_tx);
        });

        let result = export_raster(&state, ExportRasterArgs::default()).await;
        service.join().unwrap();

        assert_eq!(result.is_error, Some(true));
        assert!(result_text(&result).contains("response channel"));
    }

    #[tokio::test]
    async fn export_raster_returns_serviced_capture() {
        let (capture_tx, capture_rx) = mpsc::channel();
        let state = state_with_capture_tx(capture_tx);
        let service = std::thread::spawn(move || {
            let reply_tx = capture_rx.recv().unwrap();
            reply_tx.send(test_png()).unwrap();
        });

        let result = export_raster(&state, ExportRasterArgs::default()).await;
        service.join().unwrap();

        assert_eq!(result.is_error, None);
        assert!(result_text(&result).contains("PNG export"));
        assert!(result.content.len() >= 2);
    }
}

/// Resolve which artboards `export_artboards` should render, applying the
/// selection precedence (all > range > list > active) and bounds/cap checks.
/// Pure (no I/O) so it is unit-tested directly. `max_boards` caps the result.
fn select_artboards_for_export(
    all: &[photonic_core::Artboard],
    active: Option<uuid::Uuid>,
    all_flag: bool,
    range: Option<[usize; 2]>,
    list: Option<&[String]>,
    max_boards: usize,
) -> Result<Vec<photonic_core::Artboard>, String> {
    if all.is_empty() {
        return Err("Document has no artboards".to_string());
    }

    let selected: Vec<photonic_core::Artboard> = if all_flag {
        all.to_vec()
    } else if let Some([start, end]) = range {
        if start < 1 || end < start || end > all.len() {
            return Err(format!(
                "range [{start}, {end}] out of bounds — document has {} artboard(s) (1-based, inclusive)",
                all.len()
            ));
        }
        all[start - 1..end].to_vec()
    } else if let Some(list) = list {
        if list.is_empty() {
            return Err(
                "`artboards` was empty — omit it to export the active artboard".to_string(),
            );
        }
        let mut out = Vec::new();
        for sel in list {
            let found = if let Ok(id) = uuid::Uuid::parse_str(sel) {
                all.iter().find(|a| a.id == id).cloned()
            } else if let Ok(idx) = sel.parse::<usize>() {
                (idx >= 1 && idx <= all.len()).then(|| all[idx - 1].clone())
            } else {
                all.iter().find(|a| a.name == *sel).cloned()
            };
            match found {
                Some(a) => out.push(a),
                None => {
                    return Err(format!(
                        "Artboard '{sel}' not found (use a UUID, exact name, or 1-based index)"
                    ))
                }
            }
        }
        out
    } else {
        // Default: the active artboard (falls back to the first).
        active
            .and_then(|id| all.iter().find(|a| a.id == id).cloned())
            .or_else(|| all.first().cloned())
            .into_iter()
            .collect()
    };

    if selected.is_empty() {
        return Err("No artboards matched the request".to_string());
    }
    if selected.len() > max_boards {
        return Err(format!(
            "Requested {} artboards; max {max_boards} per call. Narrow the range or split into multiple calls.",
            selected.len()
        ));
    }
    Ok(selected)
}

/// Export one or more artboards to raster images, one image per artboard.
///
/// Unlike `export_raster` (which snapshots the live editor viewport), this
/// renders each artboard's rectangle off-screen via the headless renderer, so
/// the result is exact regardless of what the editor is currently showing.
/// Selection precedence: `all` > `range` > `artboards` list > the active artboard.
pub async fn export_artboards(state: &AppState, args: ExportArtboardsArgs) -> ToolResult {
    tracing::debug!("tool: export_artboards");
    use photonic_core::Artboard;

    // Caps that bound the response payload.
    const MAX_BOARDS: usize = 24;
    const MAX_DIM: u32 = 8192;

    let format = args.format.as_deref().unwrap_or("png").to_lowercase();
    if !matches!(
        format.as_str(),
        "png" | "jpeg" | "jpg" | "webp" | "gif" | "tiff" | "tif"
    ) {
        return ToolResult::error(format!(
            "Unsupported format: '{format}'. Use 'png', 'jpeg', 'webp', 'gif', or 'tiff'."
        ));
    }
    let scale = args.scale.unwrap_or(1.0).clamp(0.05, 8.0);
    let transparent = args.transparent.unwrap_or(false);
    let bleed = args.bleed.unwrap_or(false);

    // Resolve the target artboards and snapshot the document under one lock.
    let (render_doc, targets) = {
        let doc = state.document.lock().await;
        let selected = match select_artboards_for_export(
            &doc.artboards,
            doc.active_artboard,
            args.all.unwrap_or(false),
            args.range,
            args.artboards.as_deref(),
            MAX_BOARDS,
        ) {
            Ok(v) => v,
            Err(e) => return ToolResult::error(e),
        };
        (doc.clone(), selected)
    };

    // Print-bleed inset in document px, added to all four sides of each artboard
    // rectangle when `bleed` is set. `bleed_mm` is converted at the document DPI
    // (the same coordinate space artboard rectangles live in), so the expanded
    // region includes exactly the document's print bleed.
    let bleed_px = if bleed {
        photonic_core::units::to_px(
            render_doc.bleed_mm,
            photonic_core::units::DocumentUnit::Mm,
            render_doc.dpi,
        )
    } else {
        0.0
    };

    // Render every target off-thread through the editor's own surfaceless
    // renderer, so each artboard PNG is pixel-identical to what the canvas shows
    // (WYSIWYG). Creating GPU work off the async runtime.
    let renderer = export_renderer().await;
    let jobs = targets.clone();
    let rendered: Vec<(Artboard, Vec<u8>, u32, u32)> = tokio::task::spawn_blocking(move || {
        use photonic_render::{ExportBackground, ExportOptions};
        let background = if transparent {
            ExportBackground::Transparent
        } else {
            ExportBackground::Artboard
        };
        let mut guard = renderer.lock().unwrap();
        let Some(renderer) = guard.as_mut() else {
            return Vec::new(); // no GPU adapter — surfaced as an error below
        };
        jobs.into_iter()
            .map(|a| {
                // Expand the trim rectangle by the print bleed on all four sides.
                let (rx, ry, rw, rh) = (
                    a.x - bleed_px,
                    a.y - bleed_px,
                    a.width + 2.0 * bleed_px,
                    a.height + 2.0 * bleed_px,
                );
                let pw = ((rw * scale).round() as i64).clamp(1, MAX_DIM as i64) as u32;
                let ph = ((rh * scale).round() as i64).clamp(1, MAX_DIM as i64) as u32;
                let opts = ExportOptions {
                    background,
                    region: Some((rx, ry, rw, rh)),
                    ..Default::default()
                };
                let (px, w, h) = renderer.render_export_rgba(&render_doc, pw, ph, &opts);
                (a, px, w, h)
            })
            .collect()
    })
    .await
    .unwrap_or_default();

    if rendered.is_empty() || rendered.iter().all(|(_, px, _, _)| px.is_empty()) {
        return ToolResult::error("Renderer returned no pixels (GPU readback failed)");
    }

    // Encode each artboard to the requested format. When `path` is set, write each
    // image to disk and return a SMALL result (path + dimensions per artboard) —
    // never base64, so a full-resolution PNG never floods the MCP socket. Base64 is
    // only emitted when `path` is omitted. Mirrors `export_pdf`'s path handling.
    use base64::Engine;
    let board_count = rendered.len();
    let mut items: Vec<serde_json::Value> = Vec::new();
    let mut total_bytes = 0usize;
    for (idx, (a, px, w, h)) in rendered.into_iter().enumerate() {
        if px.is_empty() {
            continue;
        }
        let Some(img) = image::RgbaImage::from_raw(w, h, px) else {
            continue;
        };
        let mut png = Vec::new();
        if image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .is_err()
        {
            continue;
        }
        let (bytes, mime, fmt_label) = match format.as_str() {
            "jpeg" | "jpg" => {
                let q = args.quality.unwrap_or(90).clamp(1, 100);
                match png_to_jpeg(&png, q) {
                    Some(b) => (b, "image/jpeg", "jpeg"),
                    None => return ToolResult::error("Failed to encode JPEG"),
                }
            }
            "webp" => {
                let q = args.quality.unwrap_or(80).clamp(1, 100);
                match png_to_webp(&png, q) {
                    Some(b) => (b, "image/webp", "webp"),
                    None => return ToolResult::error("Failed to encode WebP"),
                }
            }
            "gif" => match png_to_gif(&png) {
                Some(b) => (b, "image/gif", "gif"),
                None => return ToolResult::error("Failed to encode GIF"),
            },
            "tiff" | "tif" => match png_to_tiff(&png) {
                Some(b) => (b, "image/tiff", "tiff"),
                None => return ToolResult::error("Failed to encode TIFF"),
            },
            _ => (png, "image/png", "png"),
        };
        total_bytes += bytes.len();

        if let Some(template) = args.path.as_deref() {
            // Single artboard → write to `path` verbatim; multiple → expand the
            // {name}/{index} template so each artboard gets a distinct file.
            let out_path = if board_count > 1 {
                expand_path_template(template, &a.name, idx + 1)
            } else {
                template.to_string()
            };
            if let Err(e) = write_file_with_parents(&out_path, &bytes) {
                return ToolResult::error(format!("Failed to write image to '{out_path}': {e}"));
            }
            items.push(serde_json::json!({
                "artboard_id": a.id,
                "name": a.name,
                "path": out_path,
                "width": w,
                "height": h,
                "format": fmt_label,
                "bytes": bytes.len(),
                "mime": mime,
            }));
        } else {
            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
            items.push(serde_json::json!({
                "artboard_id": a.id,
                "name": a.name,
                "width": w,
                "height": h,
                "format": fmt_label,
                "bytes": bytes.len(),
                "mime": mime,
                "data_base64": b64,
            }));
        }
    }

    if items.is_empty() {
        return ToolResult::error("No artboards could be encoded");
    }

    let n = items.len();
    let summary = if args.path.is_some() {
        format!(
            "Exported {n} artboard{} as {} → disk — {} bytes total",
            if n == 1 { "" } else { "s" },
            format.to_uppercase(),
            total_bytes
        )
    } else {
        format!(
            "Exported {n} artboard{} as {} — {} bytes total",
            if n == 1 { "" } else { "s" },
            format.to_uppercase(),
            total_bytes
        )
    };
    ToolResult::text(summary)
        .with_data(serde_json::json!({ "count": n, "format": format, "artboards": items }))
}

/// Lazily-initialized offscreen renderer shared by `preview_selection`. Created
/// once on first use (creating a wgpu device is expensive). The GUI app already
/// runs a HeadlessRenderer alongside the live context, so this coexists safely.
static PREVIEW_RENDERER: tokio::sync::OnceCell<std::sync::Arc<photonic_render::HeadlessRenderer>> =
    tokio::sync::OnceCell::const_new();

async fn preview_renderer() -> std::sync::Arc<photonic_render::HeadlessRenderer> {
    PREVIEW_RENDERER
        .get_or_init(|| async {
            std::sync::Arc::new(photonic_render::HeadlessRenderer::new().await)
        })
        .await
        .clone()
}

/// Lazily-initialized **surfaceless** editor renderer shared by PNG/artboard
/// export. Rendering through the same [`photonic_render::PhotonicRenderer`] the
/// on-canvas editor uses makes export pixel-identical to the editor (WYSIWYG) —
/// same glyphon text layout, same GPU gradient shading, same stroke width +
/// opacity — retiring the divergent `HeadlessRenderer` draw path for export. Held
/// behind a `std::sync::Mutex` because a render pass needs `&mut` (it mutates
/// per-frame glyph/geometry state). The inner `Option` is `None` when the machine
/// has no GPU adapter (headless CI); callers then surface a "renderer unavailable"
/// error, exactly as the old headless path did when readback failed.
type ExportRenderer = std::sync::Arc<std::sync::Mutex<Option<photonic_render::PhotonicRenderer>>>;

static EXPORT_RENDERER: tokio::sync::OnceCell<ExportRenderer> = tokio::sync::OnceCell::const_new();

/// GPU device/queue shared from the live GUI renderer. When the app runs with a
/// window, it registers a clone of the windowed renderer's device+queue here (see
/// [`register_export_gpu`]) so the export renderer reuses the SAME GPU context
/// instead of creating a second `wgpu::Instance`/adapter/device. Creating a second
/// device alongside the presenting surface device crashed the running app; sharing
/// one device is safe (`Device`/`Queue` are `Send + Sync`). Absent in headless MCP
/// mode (no window, no live renderer), where a self-owned device is safe.
static EXPORT_GPU: std::sync::OnceLock<(
    std::sync::Arc<wgpu::Device>,
    std::sync::Arc<wgpu::Queue>,
)> = std::sync::OnceLock::new();

/// Register the live GUI renderer's GPU device/queue for reuse by artboard/PNG
/// export. Called once from the app after the windowed renderer is created. A
/// second call is ignored (the first registration wins).
pub fn register_export_gpu(
    device: std::sync::Arc<wgpu::Device>,
    queue: std::sync::Arc<wgpu::Queue>,
) {
    let _ = EXPORT_GPU.set((device, queue));
}

async fn export_renderer() -> ExportRenderer {
    EXPORT_RENDERER
        .get_or_init(|| async {
            // Build the renderer OFF the async runtime thread: `assemble` takes a
            // `blocking_lock()` on the seed document (renderer/mod.rs) and the
            // headless branch does a blocking `pollster::block_on` on the adapter/
            // device request — both panic ("Cannot block the current thread from
            // within a runtime") if run on the tokio runtime thread. `spawn_blocking`
            // moves construction to a blocking-pool thread where blocking is allowed.
            let renderer = tokio::task::spawn_blocking(|| {
                // The capture channel is unused (we call `render_export_rgba`
                // directly), so dropping the sender is harmless. The 16×16 seed size
                // is resized to each artboard on first render.
                let (_tx, rx) = std::sync::mpsc::channel();
                let doc = std::sync::Arc::new(tokio::sync::Mutex::new(
                    photonic_core::document::Document::new("export", 1.0, 1.0),
                ));
                match EXPORT_GPU.get() {
                    // GUI mode: reuse the live renderer's device — no second GPU context.
                    Some((device, queue)) => {
                        Some(photonic_render::PhotonicRenderer::new_offscreen_shared(
                            std::sync::Arc::clone(device),
                            std::sync::Arc::clone(queue),
                            16,
                            16,
                            doc,
                            rx,
                        ))
                    }
                    // Headless MCP mode: no live renderer, so a self-owned device is
                    // safe. `new_offscreen` is async (adapter/device request); drive
                    // it to completion here with `pollster` since we're off-runtime.
                    None => pollster::block_on(photonic_render::PhotonicRenderer::new_offscreen(
                        16, 16, doc, rx,
                    )),
                }
            })
            .await
            .unwrap_or(None);
            std::sync::Arc::new(std::sync::Mutex::new(renderer))
        })
        .await
        .clone()
}

/// #204: render the selection at target display sizes over light AND dark
/// backgrounds as a single contact-sheet PNG — judge small-size legibility and
/// on-surface contrast without leaving Photonic.
pub async fn preview_selection(state: &AppState, args: PreviewSelectionArgs) -> ToolResult {
    tracing::debug!("tool: preview_selection");
    use photonic_core::node::SceneNodeKind;

    let sizes: Vec<u32> = args
        .sizes
        .clone()
        .unwrap_or_else(|| vec![24, 32, 48])
        .into_iter()
        .filter(|s| *s > 0 && *s <= 1024)
        .collect();
    if sizes.is_empty() {
        return ToolResult::error("No valid `sizes` (each must be 1–1024).");
    }
    let bg_hexes = args
        .backgrounds
        .clone()
        .unwrap_or_else(|| vec!["#ffffff".into(), "#0b0b12".into()]);
    let mut backgrounds: Vec<[u8; 3]> = Vec::new();
    for hex in &bg_hexes {
        match photonic_core::color::Color::from_hex(hex) {
            Some(c) => backgrounds.push([
                (c.r * 255.0).round() as u8,
                (c.g * 255.0).round() as u8,
                (c.b * 255.0).round() as u8,
            ]),
            None => return ToolResult::error(format!("Invalid background color '{hex}'")),
        }
    }
    if backgrounds.is_empty() {
        return ToolResult::error("At least one background is required.");
    }

    // Snapshot the document + resolve the selection under the lock, then release.
    let (render_doc, region, node_count) = {
        let doc = state.document.lock().await;
        let ids: Vec<photonic_core::node::NodeId> = match &args.node_ids {
            Some(raw) if !raw.is_empty() => raw
                .iter()
                .filter_map(|s| {
                    uuid::Uuid::parse_str(s)
                        .ok()
                        .or_else(|| doc.find_node_by_name(s).map(|n| n.id))
                })
                .collect(),
            _ => doc.selection.ids().copied().collect(),
        };
        if ids.is_empty() {
            return ToolResult::error("No nodes specified and no active selection");
        }

        // Kept set = the selected nodes plus all descendants of any selected group,
        // so a selected group renders whole and unrelated icons are excluded.
        let mut keep: std::collections::HashSet<photonic_core::node::NodeId> =
            ids.iter().copied().collect();
        let mut stack: Vec<photonic_core::node::NodeId> = ids.clone();
        while let Some(id) = stack.pop() {
            if let Some(SceneNodeKind::Group(g)) = doc.nodes.get(&id).map(|n| &n.kind) {
                for c in &g.children {
                    if keep.insert(*c) {
                        stack.push(*c);
                    }
                }
            }
        }

        // Union world-space bbox of the selected (top-level) nodes.
        let mut bbox: Option<(f64, f64, f64, f64)> = None;
        for id in &ids {
            if let Some(node) = doc.nodes.get(id) {
                if let Some(lb) = node.local_bounds() {
                    let (x0, y0) = node.transform.apply(lb.x0, lb.y0);
                    let (x1, y1) = node.transform.apply(lb.x1, lb.y1);
                    let (nx0, ny0) = (x0.min(x1), y0.min(y1));
                    let (nx1, ny1) = (x0.max(x1), y0.max(y1));
                    bbox = Some(match bbox {
                        None => (nx0, ny0, nx1, ny1),
                        Some((ax0, ay0, ax1, ay1)) => {
                            (ax0.min(nx0), ay0.min(ny0), ax1.max(nx1), ay1.max(ny1))
                        }
                    });
                }
            }
        }
        let Some((bx0, by0, bx1, by1)) = bbox else {
            return ToolResult::error("Selection has no measurable bounds to preview.");
        };

        // Expand to a centered square + padding so aspect ratio is preserved.
        let bw = (bx1 - bx0).max(1e-6);
        let bh = (by1 - by0).max(1e-6);
        let cx = (bx0 + bx1) / 2.0;
        let cy = (by0 + by1) / 2.0;
        let pad = args.pad.unwrap_or(0.15).max(0.0);
        let side = bw.max(bh) * (1.0 + 2.0 * pad);
        let region = (cx - side / 2.0, cy - side / 2.0, side, side);

        // Clone the doc and hide everything outside the kept set so only the
        // selection renders inside the region.
        let mut clone = doc.clone();
        for (id, node) in clone.nodes.iter_mut() {
            if !keep.contains(id) {
                node.visible = false;
            }
        }
        (clone, region, ids.len())
    };

    // Render each size once (transparent bg, fit to the square region) off-thread.
    let renderer = preview_renderer().await;
    let sizes_for_job = sizes.clone();
    let rendered: Vec<(u32, Vec<u8>)> = tokio::task::spawn_blocking(move || {
        use photonic_render::{ExportBackground, ExportOptions};
        let opts = ExportOptions {
            background: ExportBackground::Transparent,
            region: Some(region),
            ..Default::default()
        };
        sizes_for_job
            .into_iter()
            .map(|s| {
                let (px, _, _) = renderer.render_rgba_with_opts(&render_doc, s, s, &opts);
                (s, px)
            })
            .collect()
    })
    .await
    .unwrap_or_default();

    if rendered.iter().all(|(_, px)| px.is_empty()) {
        return ToolResult::error("Renderer returned no pixels (GPU readback failed).");
    }

    // Assemble the contact sheet: rows = backgrounds, columns = sizes.
    let margin: u32 = 10;
    let max_size = *sizes.iter().max().unwrap();
    let cell = max_size + 2 * margin;
    let cols = sizes.len() as u32;
    let rows = backgrounds.len() as u32;
    let sheet_w = cols * cell;
    let sheet_h = rows * cell;
    let mut sheet = image::RgbaImage::new(sheet_w, sheet_h);

    for (r, bg) in backgrounds.iter().enumerate() {
        let r = r as u32;
        // Fill this row band with the background swatch.
        for y in (r * cell)..((r + 1) * cell) {
            for x in 0..sheet_w {
                sheet.put_pixel(x, y, image::Rgba([bg[0], bg[1], bg[2], 255]));
            }
        }
        for (c, (s, px)) in rendered.iter().enumerate() {
            if px.is_empty() {
                continue;
            }
            let c = c as u32;
            let s = *s;
            let ox = c * cell + (cell - s) / 2;
            let oy = r * cell + (cell - s) / 2;
            // Alpha-composite the icon over the already-painted background.
            for iy in 0..s {
                for ix in 0..s {
                    let idx = ((iy * s + ix) * 4) as usize;
                    if idx + 3 >= px.len() {
                        continue;
                    }
                    let a = px[idx + 3] as f32 / 255.0;
                    if a <= 0.0 {
                        continue;
                    }
                    let dst = sheet.get_pixel(ox + ix, oy + iy).0;
                    let blend = |fg: u8, bg: u8| -> u8 {
                        (fg as f32 * a + bg as f32 * (1.0 - a)).round() as u8
                    };
                    sheet.put_pixel(
                        ox + ix,
                        oy + iy,
                        image::Rgba([
                            blend(px[idx], dst[0]),
                            blend(px[idx + 1], dst[1]),
                            blend(px[idx + 2], dst[2]),
                            255,
                        ]),
                    );
                }
            }
        }
    }

    // Encode the sheet as PNG.
    let mut out = Vec::new();
    if image::DynamicImage::ImageRgba8(sheet)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .is_err()
    {
        return ToolResult::error("Failed to encode contact-sheet PNG.");
    }

    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&out);
    ToolResult::text(format!(
        "Preview contact sheet — {node_count} node(s), sizes {:?}, {} background(s), {}×{} px",
        sizes, rows, sheet_w, sheet_h
    ))
    .with_data(serde_json::json!({
        "format": "png",
        "bytes": out.len(),
        "mime": "image/png",
        "data_base64": b64,
        "sizes": sizes,
        "backgrounds": bg_hexes,
        "width": sheet_w,
        "height": sheet_h,
    }))
}

fn resize_png(png_bytes: &[u8], w: u32, h: u32) -> Result<Vec<u8>, String> {
    use image::{imageops::FilterType, ImageFormat};
    validate_raster_dimensions(w, h)?;
    let img = image::load_from_memory_with_format(png_bytes, ImageFormat::Png)
        .map_err(|error| format!("could not decode PNG: {error}"))?;
    let resized = img.resize_exact(w, h, FilterType::Triangle);
    let mut out = Vec::new();
    resized
        .write_to(&mut std::io::Cursor::new(&mut out), ImageFormat::Png)
        .map_err(|error| format!("could not encode PNG: {error}"))?;
    Ok(out)
}

fn png_to_jpeg(png_bytes: &[u8], quality: u8) -> Option<Vec<u8>> {
    let img = image::load_from_memory_with_format(png_bytes, image::ImageFormat::Png).ok()?;
    // Composite alpha onto white (to_rgb8 composites onto black).
    let rgba = img.to_rgba8();
    let mut rgb = image::RgbImage::new(rgba.width(), rgba.height());
    for (src, dst) in rgba.pixels().zip(rgb.pixels_mut()) {
        let a = src[3] as f32 / 255.0;
        dst[0] = (src[0] as f32 * a + 255.0 * (1.0 - a)) as u8;
        dst[1] = (src[1] as f32 * a + 255.0 * (1.0 - a)) as u8;
        dst[2] = (src[2] as f32 * a + 255.0 * (1.0 - a)) as u8;
    }
    let mut buf = Vec::new();
    let encoder =
        image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::Cursor::new(&mut buf), quality);
    image::DynamicImage::ImageRgb8(rgb)
        .write_with_encoder(encoder)
        .ok()?;
    Some(buf)
}

fn png_to_gif(png_bytes: &[u8]) -> Option<Vec<u8>> {
    let img = image::load_from_memory_with_format(png_bytes, image::ImageFormat::Png).ok()?;
    let mut buf = Vec::new();
    let encoder = image::codecs::gif::GifEncoder::new(std::io::Cursor::new(&mut buf));
    img.write_with_encoder(encoder).ok()?;
    Some(buf)
}

fn png_to_tiff(png_bytes: &[u8]) -> Option<Vec<u8>> {
    let img = image::load_from_memory_with_format(png_bytes, image::ImageFormat::Png).ok()?;
    let mut buf = Vec::new();
    img.write_to(
        &mut std::io::Cursor::new(&mut buf),
        image::ImageFormat::Tiff,
    )
    .ok()?;
    Some(buf)
}

fn png_to_webp(png_bytes: &[u8], _quality: u8) -> Option<Vec<u8>> {
    let img = image::load_from_memory_with_format(png_bytes, image::ImageFormat::Png).ok()?;
    let mut buf = Vec::new();
    let encoder = image::codecs::webp::WebPEncoder::new_lossless(std::io::Cursor::new(&mut buf));
    img.write_with_encoder(encoder).ok()?;
    Some(buf)
}

/// Export a selection of nodes as a clean, minimal SVG with a tight viewBox.
pub async fn export_selection_as_svg(state: &AppState, args: ExportSelectionArgs) -> ToolResult {
    tracing::debug!("tool: export_selection_as_svg");
    let doc = state.document.lock().await;

    // Resolve node IDs: explicit list → current selection → error.
    let ids: Vec<photonic_core::node::NodeId> = match &args.node_ids {
        Some(raw) if !raw.is_empty() => raw
            .iter()
            .filter_map(|s| uuid::Uuid::parse_str(s).ok())
            .collect(),
        _ => doc.selection.ids().copied().collect(),
    };

    if ids.is_empty() {
        return ToolResult::error("No nodes specified and no active selection");
    }

    let opts = selection_svg_opts(args.precision, args.normalize.as_deref(), args.pad);
    let svg = photonic_core::export::export_nodes_as_svg_opts(&doc, &ids, &opts);

    let output = if args.as_react_component {
        let name = args.component_name.as_deref().unwrap_or("SvgIcon");
        let indented = svg
            .lines()
            .map(|l| format!("    {}", l))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "import React from 'react';\n\n\
             export function {}(props: React.SVGProps<SVGSVGElement>) {{\n  return (\n{}\n  );\n}}\n",
            name, indented
        )
    } else {
        svg
    };

    let byte_count = output.len();
    ToolResult::text(format!(
        "Selection SVG — {} node(s), {} bytes",
        ids.len(),
        byte_count
    ))
    .with_data(serde_json::json!({
        "svg": output,
        "bytes": byte_count,
        "node_count": ids.len()
    }))
}

/// Build [`SvgSelectionOptions`] from MCP args (shared by selection + icon-set
/// export). `default_square` picks the framing when `normalize` is unspecified.
fn selection_svg_opts_ex(
    precision: Option<u8>,
    normalize: Option<&str>,
    pad: Option<f64>,
    default_square: bool,
) -> photonic_core::export::SvgSelectionOptions {
    use photonic_core::export::{SvgNormalize, SvgSelectionOptions};
    let normalize = match normalize.map(|s| s.to_ascii_lowercase()) {
        Some(ref s) if s == "square" => SvgNormalize::Square {
            pad: pad.unwrap_or(0.1).max(0.0),
        },
        Some(ref s) if s == "tight" => SvgNormalize::Tight,
        _ if default_square => SvgNormalize::Square {
            pad: pad.unwrap_or(0.1).max(0.0),
        },
        _ => SvgNormalize::Tight,
    };
    SvgSelectionOptions {
        precision: precision.unwrap_or(4).clamp(1, 6),
        optimize: true,
        normalize,
    }
}

fn selection_svg_opts(
    precision: Option<u8>,
    normalize: Option<&str>,
    pad: Option<f64>,
) -> photonic_core::export::SvgSelectionOptions {
    selection_svg_opts_ex(precision, normalize, pad, false)
}

/// #203: batch-export N tagged groups to normalized `.svg` files (or inline) in
/// one call — the canonical icon-pipeline workflow, no external post-pass needed.
pub async fn export_icon_set(state: &AppState, args: ExportIconSetArgs) -> ToolResult {
    tracing::debug!("tool: export_icon_set");
    let doc = state.document.lock().await;
    let opts = selection_svg_opts_ex(args.precision, args.normalize.as_deref(), args.pad, true);

    // Resolve the icon list: explicit entries, or every top-level group.
    struct Resolved {
        name: String,
        ids: Vec<photonic_core::node::NodeId>,
    }
    let mut resolved: Vec<Resolved> = Vec::new();
    match &args.icons {
        Some(entries) if !entries.is_empty() => {
            for e in entries {
                let ids: Vec<_> = e
                    .node_ids
                    .iter()
                    .filter_map(|s| uuid::Uuid::parse_str(s).ok())
                    .collect();
                if ids.is_empty() {
                    return ToolResult::error(format!("Icon '{}' has no valid node_ids", e.name));
                }
                resolved.push(Resolved {
                    name: e.name.clone(),
                    ids,
                });
            }
        }
        _ => {
            // Every top-level group across all layers, in draw order.
            for layer_id in &doc.layer_order {
                let Some(layer) = doc.layers.get(layer_id) else {
                    continue;
                };
                for node_id in &layer.node_ids {
                    if let Some(node) = doc.nodes.get(node_id) {
                        if matches!(node.kind, SceneNodeKind::Group(_)) {
                            resolved.push(Resolved {
                                name: node.name.clone(),
                                ids: vec![*node_id],
                            });
                        }
                    }
                }
            }
            if resolved.is_empty() {
                return ToolResult::error(
                    "No icons given and no top-level groups found. Pass `icons` explicitly \
                     or group each icon's paths first.",
                );
            }
        }
    }

    // De-duplicate + sanitize output file names.
    let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut written: Vec<serde_json::Value> = Vec::new();
    let mut files_ok = 0usize;
    let mut errors: Vec<String> = Vec::new();

    for icon in &resolved {
        let svg = photonic_core::export::export_nodes_as_svg_opts(&doc, &icon.ids, &opts);
        let mut base = slugify_filename(&icon.name);
        if base.is_empty() {
            base = "icon".to_string();
        }
        // Ensure unique stem.
        let mut stem = base.clone();
        let mut n = 2;
        while !used.insert(stem.clone()) {
            stem = format!("{base}-{n}");
            n += 1;
        }
        let file_name = format!("{stem}.svg");

        if let Some(dir) = &args.out_dir {
            let path = std::path::Path::new(dir).join(&file_name);
            match std::fs::create_dir_all(dir).and_then(|_| std::fs::write(&path, &svg)) {
                Ok(_) => {
                    files_ok += 1;
                    written.push(serde_json::json!({
                        "name": file_name,
                        "path": path.to_string_lossy(),
                        "bytes": svg.len(),
                    }));
                }
                Err(e) => errors.push(format!("{file_name}: {e}")),
            }
        } else {
            written.push(serde_json::json!({
                "name": file_name,
                "svg": svg,
                "bytes": svg.len(),
            }));
        }
    }

    if !errors.is_empty() {
        return ToolResult::error(format!(
            "Icon-set export had {} error(s): {}",
            errors.len(),
            errors.join("; ")
        ));
    }

    let summary = if args.out_dir.is_some() {
        format!(
            "Exported {} icon(s) to {}",
            files_ok,
            args.out_dir.as_deref().unwrap_or("")
        )
    } else {
        format!("Exported {} icon(s) (inline)", written.len())
    };
    ToolResult::text(summary).with_data(serde_json::json!({
        "count": written.len(),
        "icons": written,
        "out_dir": args.out_dir,
    }))
}

/// Slugify a name into a safe file stem (alnum + dash), lower-cased.
fn slugify_filename(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_dash = true;
    for c in name.chars() {
        if c.is_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

// ─── Design Token Export ──────────────────────────────────────────────────────

/// Extract the document's design vocabulary as structured design tokens.
pub async fn export_design_tokens(state: &AppState, args: ExportDesignTokensArgs) -> ToolResult {
    tracing::debug!("tool: export_design_tokens");
    let doc = state.document.lock().await;

    let mut colors: BTreeSet<String> = BTreeSet::new();
    let mut font_families: BTreeSet<String> = BTreeSet::new();
    let mut font_sizes: Vec<f64> = Vec::new();
    let mut stroke_widths: Vec<f64> = Vec::new();

    for node in doc.nodes.values() {
        match &node.kind {
            SceneNodeKind::Path(p) => {
                collect_fill_colors(&p.fill, &mut colors);
                if p.stroke.enabled {
                    colors.insert(p.stroke.color.to_hex());
                    push_unique_f64(&mut stroke_widths, p.stroke.width);
                }
            }
            SceneNodeKind::Text(t) => {
                collect_fill_colors(&t.fill, &mut colors);
                if t.stroke.enabled {
                    colors.insert(t.stroke.color.to_hex());
                    push_unique_f64(&mut stroke_widths, t.stroke.width);
                }
                font_families.insert(t.font_family.clone());
                push_unique_f64(&mut font_sizes, t.font_size);
            }
            SceneNodeKind::Group(_) => {}
            // raster: no fill/stroke/font tokens to collect
            SceneNodeKind::Raster(_) => {}
        }
    }

    font_sizes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    stroke_widths.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let format = args.format.as_deref().unwrap_or("json");
    let output = match format {
        "css" => format_tokens_css(&colors, &font_families, &font_sizes, &stroke_widths),
        "tailwind" => format_tokens_tailwind(&colors, &font_families, &font_sizes, &stroke_widths),
        "style-dictionary" => {
            format_tokens_style_dictionary(&colors, &font_families, &font_sizes, &stroke_widths)
        }
        _ => format_tokens_json(&colors, &font_families, &font_sizes, &stroke_widths),
    };

    ToolResult::text(format!(
        "Design tokens: {} color(s), {} font family/ies, {} font size(s), {} stroke width(s)",
        colors.len(),
        font_families.len(),
        font_sizes.len(),
        stroke_widths.len()
    ))
    .with_data(serde_json::json!({ "format": format, "tokens": output }))
}

fn collect_fill_colors(fill: &Fill, set: &mut BTreeSet<String>) {
    if !fill.enabled {
        return;
    }
    if let FillKind::Solid(color) = &fill.kind {
        set.insert(color.to_hex());
    }
    // Gradients are not exported as single-value tokens.
}

fn push_unique_f64(vec: &mut Vec<f64>, val: f64) {
    let already = vec.iter().any(|&v| (v - val).abs() < 0.01);
    if !already {
        vec.push(val);
    }
}

fn format_tokens_json(
    colors: &BTreeSet<String>,
    font_families: &BTreeSet<String>,
    font_sizes: &[f64],
    stroke_widths: &[f64],
) -> String {
    let colors_obj: serde_json::Map<String, serde_json::Value> = colors
        .iter()
        .enumerate()
        .map(|(i, hex)| (format!("color-{}", i + 1), serde_json::json!(hex)))
        .collect();

    let families: Vec<_> = font_families.iter().collect();
    let sizes: Vec<_> = font_sizes.iter().map(|v| serde_json::json!(v)).collect();
    let widths: Vec<_> = stroke_widths.iter().map(|v| serde_json::json!(v)).collect();

    serde_json::to_string_pretty(&serde_json::json!({
        "colors": colors_obj,
        "font_families": families,
        "font_sizes": sizes,
        "stroke_widths": widths,
    }))
    .unwrap_or_default()
}

fn format_tokens_css(
    colors: &BTreeSet<String>,
    font_families: &BTreeSet<String>,
    font_sizes: &[f64],
    stroke_widths: &[f64],
) -> String {
    let mut lines = vec![":root {".to_string()];

    for (i, hex) in colors.iter().enumerate() {
        lines.push(format!("  --color-{}: {};", i + 1, hex));
    }
    for (i, family) in font_families.iter().enumerate() {
        lines.push(format!("  --font-family-{}: {};", i + 1, family));
    }
    for (i, size) in font_sizes.iter().enumerate() {
        lines.push(format!("  --font-size-{}: {}px;", i + 1, size));
    }
    for (i, width) in stroke_widths.iter().enumerate() {
        lines.push(format!("  --stroke-width-{}: {}px;", i + 1, width));
    }

    lines.push("}".to_string());
    lines.join("\n")
}

fn format_tokens_tailwind(
    colors: &BTreeSet<String>,
    font_families: &BTreeSet<String>,
    font_sizes: &[f64],
    stroke_widths: &[f64],
) -> String {
    let colors_obj: serde_json::Map<String, serde_json::Value> = colors
        .iter()
        .enumerate()
        .map(|(i, hex)| (format!("color-{}", i + 1), serde_json::json!(hex)))
        .collect();

    let families_obj: serde_json::Map<String, serde_json::Value> = font_families
        .iter()
        .enumerate()
        .map(|(i, f)| (format!("family-{}", i + 1), serde_json::json!([f])))
        .collect();

    let sizes_obj: serde_json::Map<String, serde_json::Value> = font_sizes
        .iter()
        .enumerate()
        .map(|(i, v)| {
            (
                format!("size-{}", i + 1),
                serde_json::json!(format!("{}px", v)),
            )
        })
        .collect();

    let widths_obj: serde_json::Map<String, serde_json::Value> = stroke_widths
        .iter()
        .enumerate()
        .map(|(i, v)| {
            (
                format!("width-{}", i + 1),
                serde_json::json!(format!("{}px", v)),
            )
        })
        .collect();

    serde_json::to_string_pretty(&serde_json::json!({
        "theme": {
            "extend": {
                "colors": colors_obj,
                "fontFamily": families_obj,
                "fontSize": sizes_obj,
                "borderWidth": widths_obj,
            }
        }
    }))
    .unwrap_or_default()
}

fn format_tokens_style_dictionary(
    colors: &BTreeSet<String>,
    font_families: &BTreeSet<String>,
    font_sizes: &[f64],
    stroke_widths: &[f64],
) -> String {
    let mut root = serde_json::Map::new();

    if !colors.is_empty() {
        let color_tokens: serde_json::Map<String, serde_json::Value> = colors
            .iter()
            .enumerate()
            .map(|(i, hex)| {
                (
                    format!("color-{}", i + 1),
                    serde_json::json!({ "value": hex, "$type": "color" }),
                )
            })
            .collect();
        root.insert("color".to_string(), serde_json::Value::Object(color_tokens));
    }

    if !font_families.is_empty() {
        let family_tokens: serde_json::Map<String, serde_json::Value> = font_families
            .iter()
            .enumerate()
            .map(|(i, f)| {
                (
                    format!("family-{}", i + 1),
                    serde_json::json!({ "value": f, "$type": "fontFamily" }),
                )
            })
            .collect();
        root.insert(
            "fontFamily".to_string(),
            serde_json::Value::Object(family_tokens),
        );
    }

    if !font_sizes.is_empty() {
        let size_tokens: serde_json::Map<String, serde_json::Value> = font_sizes
            .iter()
            .enumerate()
            .map(|(i, v)| {
                (
                    format!("size-{}", i + 1),
                    serde_json::json!({ "value": format!("{}px", v), "$type": "dimension" }),
                )
            })
            .collect();
        root.insert(
            "fontSize".to_string(),
            serde_json::Value::Object(size_tokens),
        );
    }

    if !stroke_widths.is_empty() {
        let width_tokens: serde_json::Map<String, serde_json::Value> = stroke_widths
            .iter()
            .enumerate()
            .map(|(i, v)| {
                (
                    format!("width-{}", i + 1),
                    serde_json::json!({ "value": format!("{}px", v), "$type": "dimension" }),
                )
            })
            .collect();
        root.insert(
            "strokeWidth".to_string(),
            serde_json::Value::Object(width_tokens),
        );
    }

    serde_json::to_string_pretty(&serde_json::Value::Object(root)).unwrap_or_default()
}

// ─── Checkpoint Diff ─────────────────────────────────────────────────────────

pub async fn add_export_profile(state: &AppState, args: AddExportProfileArgs) -> ToolResult {
    tracing::debug!("tool: add_export_profile");

    let format = args.format.to_lowercase();
    if !matches!(format.as_str(), "svg" | "png" | "jpeg" | "jpg" | "webp") {
        return ToolResult::error(format!(
            "Unsupported format '{}'. Use svg, png, jpeg, or webp.",
            args.format
        ));
    }
    if args.name.trim().is_empty() {
        return ToolResult::error("Profile name must not be empty");
    }

    use photonic_core::ExportProfile;
    let profile = ExportProfile {
        name: args.name.trim().to_string(),
        format: format.clone(),
        width: args.width,
        height: args.height,
        semantic_ids: args.semantic_ids,
        precision: args.precision,
    };

    let mut doc = state.document.lock().await;
    // Replace existing or append.
    if let Some(existing) = doc
        .export_profiles
        .iter_mut()
        .find(|p| p.name == profile.name)
    {
        *existing = profile.clone();
        ToolResult::text(format!("Updated export profile '{}'.", profile.name))
    } else {
        doc.export_profiles.push(profile.clone());
        ToolResult::text(format!(
            "Added export profile '{}' ({}).",
            profile.name, format
        ))
    }
    .with_data(serde_json::json!({ "name": profile.name, "format": format }))
}

pub async fn list_export_profiles(state: &AppState) -> ToolResult {
    tracing::debug!("tool: list_export_profiles");
    let doc = state.document.lock().await;
    if doc.export_profiles.is_empty() {
        return ToolResult::text("No export profiles defined.");
    }
    let profiles: Vec<_> = doc
        .export_profiles
        .iter()
        .map(|p| {
            serde_json::json!({
                "name": p.name,
                "format": p.format,
                "width": p.width,
                "height": p.height,
                "semantic_ids": p.semantic_ids,
                "precision": p.precision,
            })
        })
        .collect();
    ToolResult::text(format!("{} export profile(s) defined.", profiles.len()))
        .with_data(serde_json::json!({ "profiles": profiles }))
}

pub async fn remove_export_profile(state: &AppState, args: RemoveExportProfileArgs) -> ToolResult {
    tracing::debug!("tool: remove_export_profile");
    let mut doc = state.document.lock().await;
    let before = doc.export_profiles.len();
    doc.export_profiles.retain(|p| p.name != args.name);
    if doc.export_profiles.len() < before {
        ToolResult::text(format!("Removed export profile '{}'.", args.name))
    } else {
        ToolResult::error(format!("No profile named '{}' found.", args.name))
    }
}

pub async fn run_export_profile(state: &AppState, args: RunExportProfileArgs) -> ToolResult {
    tracing::debug!("tool: run_export_profile");

    let profile = {
        let doc = state.document.lock().await;
        doc.export_profiles
            .iter()
            .find(|p| p.name == args.name)
            .cloned()
    };

    let profile = match profile {
        Some(p) => p,
        None => return ToolResult::error(format!("No export profile named '{}'.", args.name)),
    };

    match profile.format.as_str() {
        "svg" => {
            let svg_args = crate::protocol::ExportSvgArgs {
                semantic_ids: profile.semantic_ids,
                precision: profile.precision.map(|p| p as u8),
                inner_only: false,
            };
            export_svg(state, svg_args).await
        }
        "png" | "jpeg" | "jpg" | "webp" => {
            let raster_args = crate::protocol::ExportRasterArgs {
                format: Some(profile.format.clone()),
                width: profile.width,
                height: profile.height,
                quality: None,
                path: None,
            };
            export_raster(state, raster_args).await
        }
        other => ToolResult::error(format!("Unknown format '{}' in profile.", other)),
    }
}

// ─── Document Templates ───────────────────────────────────────────────────────

/// #207: import named color swatches from a design-tokens payload (CSS custom
/// properties / JSON / style-dictionary) — the counterpart to
/// `export_design_tokens`. Registered swatches are referenceable by name.
pub async fn import_design_tokens(state: &AppState, args: ImportDesignTokensArgs) -> ToolResult {
    tracing::debug!("tool: import_design_tokens");
    use photonic_core::ColorSwatch;

    // Resolve the source text.
    let text = match (&args.content, &args.path) {
        (Some(c), _) if !c.trim().is_empty() => c.clone(),
        (_, Some(p)) => match std::fs::read_to_string(p) {
            Ok(s) => s,
            Err(e) => return ToolResult::error(format!("Failed to read tokens file '{p}': {e}")),
        },
        _ => return ToolResult::error("Provide either `content` (inline tokens) or `path`."),
    };

    // (name, hex) pairs parsed from the payload (shared with the GUI import).
    let tokens = photonic_core::tokens::parse_token_colors(&text, args.format.as_deref());

    if tokens.is_empty() {
        return ToolResult::error(
            "No color tokens found. Expected CSS custom properties (--name: #hex) or JSON \
             with hex color values.",
        );
    }

    let prefix = args.prefix.clone().unwrap_or_default();
    let mut doc = state.document.lock().await;
    if args.clear_existing {
        doc.color_swatches.clear();
    }

    let mut added = 0usize;
    let mut updated = 0usize;
    let mut names: Vec<String> = Vec::new();
    for (name, hex) in &tokens {
        let full = format!("{prefix}{name}");
        // Normalize to #rrggbb; skip anything unparseable.
        let Some(color) = photonic_core::color::Color::from_hex(hex) else {
            continue;
        };
        let norm = color.to_hex();
        if let Some(existing) = doc.color_swatches.iter_mut().find(|s| s.name == full) {
            existing.color_hex = norm.clone();
            updated += 1;
        } else {
            doc.color_swatches.push(ColorSwatch::new(&full, &norm));
            added += 1;
        }
        names.push(full);
    }

    ToolResult::text(format!(
        "Imported design tokens: {added} swatch(es) added, {updated} updated."
    ))
    .with_data(serde_json::json!({
        "added": added,
        "updated": updated,
        "swatches": names,
    }))
}

#[cfg(test)]
mod reorder_layers_tests {
    use super::reorder_layers;
    use crate::protocol::ReorderLayersArgs;
    use crate::server::{AppState, McpServerConfig};
    use photonic_core::node::{PathNode, SceneNode, SceneNodeKind};
    use photonic_core::path::PathData;
    use photonic_core::{AuditLog, Document, Layer};
    use std::sync::{Arc, Mutex as StdMutex};
    use tokio::sync::Mutex;
    use uuid::Uuid;

    fn state_with(doc: Document) -> AppState {
        let (tx, _rx) = std::sync::mpsc::channel();
        AppState {
            document: Arc::new(Mutex::new(doc)),
            history: Arc::new(Mutex::new(photonic_core::history::CommandHistory::new(100))),
            document_path: Arc::new(StdMutex::new(None)),
            capture_tx: Arc::new(StdMutex::new(tx)),
            config: McpServerConfig::default(),
            path_policy: photonic_core::PathPolicy::test_default(),
            audit_log: Arc::new(StdMutex::new(AuditLog::new())),
            clipboard_ring: Arc::new(crate::handlers::clipboard::new_clipboard_ring()),
            video_engine: Arc::new(crate::handlers::video_jobs::VideoEngineHandle::new()),
            video_jobs: Arc::new(StdMutex::new(
                crate::handlers::video_jobs::JobRegistry::new(),
            )),
        }
    }

    fn two_layer_doc() -> (Document, Uuid, Uuid) {
        let mut doc = Document::new("reorder test", 200.0, 100.0);
        let first_id = doc.layer_order[0];
        let second_id = doc.add_layer(Layer::new("Layer 2"));

        doc.add_node(
            SceneNode::new(
                "first",
                first_id,
                SceneNodeKind::Path(PathNode::new(PathData::rect(0.0, 0.0, 10.0, 10.0))),
            ),
            Some(first_id),
        );
        doc.add_node(
            SceneNode::new(
                "second",
                second_id,
                SceneNodeKind::Path(PathNode::new(PathData::rect(20.0, 0.0, 10.0, 10.0))),
            ),
            Some(second_id),
        );

        (doc, first_id, second_id)
    }

    fn draw_order_names(doc: &Document) -> Vec<String> {
        doc.nodes_in_draw_order()
            .into_iter()
            .map(|node| node.name.clone())
            .collect()
    }

    #[tokio::test]
    async fn duplicate_layer_ids_are_rejected_without_mutation() {
        let (doc, first_id, _) = two_layer_doc();
        let original_order = doc.layer_order.clone();
        let state = state_with(doc);

        let result = reorder_layers(
            &state,
            ReorderLayersArgs {
                layer_order: vec![first_id.to_string(), first_id.to_string()],
            },
        )
        .await;

        assert_eq!(result.is_error, Some(true));
        let doc = state.document.lock().await;
        assert_eq!(doc.layer_order, original_order);
        assert_eq!(draw_order_names(&doc), ["first", "second"]);
        assert_eq!(state.history.lock().await.undo_depth(), 0);
    }

    #[tokio::test]
    async fn valid_reversed_order_updates_draw_order_once() {
        let (doc, first_id, second_id) = two_layer_doc();
        let state = state_with(doc);

        let result = reorder_layers(
            &state,
            ReorderLayersArgs {
                layer_order: vec![second_id.to_string(), first_id.to_string()],
            },
        )
        .await;

        assert_ne!(result.is_error, Some(true));
        let doc = state.document.lock().await;
        assert_eq!(doc.layer_order, [second_id, first_id]);
        assert_eq!(draw_order_names(&doc), ["second", "first"]);
        assert_eq!(state.history.lock().await.undo_depth(), 1);
    }

    #[tokio::test]
    async fn missing_unknown_and_invalid_layer_ids_are_rejected() {
        let (doc, first_id, second_id) = two_layer_doc();
        let original_order = doc.layer_order.clone();
        let unknown_id = Uuid::new_v4();

        for layer_order in [
            vec![first_id.to_string()],
            vec![first_id.to_string(), unknown_id.to_string()],
            vec![first_id.to_string(), "not-a-uuid".to_string()],
            vec![
                first_id.to_string(),
                second_id.to_string(),
                "not-a-uuid".to_string(),
            ],
        ] {
            let state = state_with(doc.clone());
            let result = reorder_layers(&state, ReorderLayersArgs { layer_order }).await;

            assert_eq!(result.is_error, Some(true));
            let doc = state.document.lock().await;
            assert_eq!(doc.layer_order, original_order);
            assert_eq!(draw_order_names(&doc), ["first", "second"]);
            assert_eq!(state.history.lock().await.undo_depth(), 0);
        }
    }
}

// ─── Graphic Styles ───────────────────────────────────────────────────────────

#[cfg(test)]
mod export_artboards_tests {
    use super::select_artboards_for_export;
    use photonic_core::Artboard;

    fn boards() -> Vec<Artboard> {
        vec![
            Artboard::new("Cover", 0.0, 0.0, 100.0, 100.0),
            Artboard::new("Body", 120.0, 0.0, 100.0, 100.0),
            Artboard::new("Back", 240.0, 0.0, 100.0, 100.0),
        ]
    }

    #[test]
    fn all_flag_returns_every_board_in_order() {
        let b = boards();
        let out = select_artboards_for_export(&b, None, true, None, None, 24).unwrap();
        assert_eq!(
            out.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            ["Cover", "Body", "Back"]
        );
    }

    #[test]
    fn range_is_one_based_inclusive() {
        let b = boards();
        let out = select_artboards_for_export(&b, None, false, Some([2, 3]), None, 24).unwrap();
        assert_eq!(
            out.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            ["Body", "Back"]
        );
    }

    #[test]
    fn range_out_of_bounds_errors() {
        let b = boards();
        assert!(select_artboards_for_export(&b, None, false, Some([0, 2]), None, 24).is_err());
        assert!(select_artboards_for_export(&b, None, false, Some([2, 9]), None, 24).is_err());
        assert!(select_artboards_for_export(&b, None, false, Some([3, 2]), None, 24).is_err());
    }

    #[test]
    fn list_resolves_by_name_index_and_id_in_given_order() {
        let b = boards();
        let id2 = b[1].id.to_string();
        let sel = vec!["Back".to_string(), "1".to_string(), id2];
        let out = select_artboards_for_export(&b, None, false, None, Some(&sel), 24).unwrap();
        assert_eq!(
            out.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            ["Back", "Cover", "Body"]
        );
    }

    #[test]
    fn unknown_selector_errors() {
        let b = boards();
        let sel = vec!["Nope".to_string()];
        assert!(select_artboards_for_export(&b, None, false, None, Some(&sel), 24).is_err());
    }

    #[test]
    fn default_is_active_then_first() {
        let b = boards();
        // active set → that one
        let out = select_artboards_for_export(&b, Some(b[2].id), false, None, None, 24).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "Back");
        // no active → first
        let out = select_artboards_for_export(&b, None, false, None, None, 24).unwrap();
        assert_eq!(out[0].name, "Cover");
    }

    #[test]
    fn precedence_all_over_range_over_list() {
        let b = boards();
        let sel = vec!["Cover".to_string()];
        // all wins even when range + list provided
        let out =
            select_artboards_for_export(&b, None, true, Some([1, 1]), Some(&sel), 24).unwrap();
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn cap_is_enforced() {
        let b = boards();
        assert!(select_artboards_for_export(&b, None, true, None, None, 2).is_err());
    }

    #[test]
    fn empty_document_errors() {
        assert!(select_artboards_for_export(&[], None, true, None, None, 24).is_err());
    }
}

/// ROUND 2 — drive the EXACT MCP `export_pdf` handler the brief targets:
/// `export_pdf { artboards:["Card Back"], color_mode:"cmyk", outline_text:true,
/// marks:true }` on a card back (dark #0b0b12 card + transparent avatar + a
/// center-aligned wordmark). Verifies all three issues on the real handler path,
/// not just the core unit tests.
#[cfg(test)]
mod export_pdf_real_path_tests {
    use super::export_pdf;
    use crate::protocol::ExportPdfArgs;
    use crate::server::{AppState, McpServerConfig};
    use photonic_core::node::{
        PathNode, RasterNode, SceneNode, SceneNodeKind, TextAlign, TextNode,
    };
    use photonic_core::path::PathData;
    use photonic_core::raster::RasterImage;
    use photonic_core::style::{Fill, FillKind};
    use photonic_core::transform::Transform;
    use photonic_core::{Artboard, AuditLog, Color, Document};
    use std::sync::{Arc, Mutex as StdMutex};
    use tokio::sync::Mutex;

    /// A "Card Back" artboard: dark card, a transparent avatar placed over it
    /// (oversampled → must downsample), and an align=center wordmark whose origin
    /// sits on the right half of the card (mirrors tx≈1749.7 in the real doc).
    fn card_back_doc() -> (Document, photonic_core::node::NodeId) {
        let mut doc = Document::new("UNN", 1050.0, 600.0);
        doc.dpi = 300.0;
        doc.color_mode = photonic_core::document::ColorMode::Cmyk;
        doc.artboards = vec![Artboard::new("Card Back", 0.0, 0.0, 1050.0, 600.0)];
        doc.active_artboard = Some(doc.artboards[0].id);
        let layer = doc.active_layer_id.unwrap();

        // Dark card #0b0b12.
        let mut card = PathNode::new(PathData::rect(0.0, 0.0, 1050.0, 600.0));
        card.fill = Fill {
            kind: FillKind::Solid(Color::new(0.043, 0.043, 0.070, 1.0)),
            opacity: 1.0,
            enabled: true,
        };
        doc.add_node(
            SceneNode::new("card", layer, SceneNodeKind::Path(card)),
            None,
        );

        // Oversampled avatar (1200²) placed small (scale 0.3 → 1000 DPI effective
        // → downsampled to 300). Transparent border, opaque interior.
        let native = 1200u32;
        let mut px = vec![0u8; (native * native * 4) as usize];
        for y in 0..native {
            for x in 0..native {
                let i = ((y * native + x) * 4) as usize;
                let opaque = x > 360 && x < 840 && y > 360 && y < 840;
                px[i..i + 4].copy_from_slice(&[235, 225, 210, if opaque { 255 } else { 0 }]);
            }
        }
        let img = RasterImage::from_rgba(native, native, px).unwrap();
        let mut avatar =
            SceneNode::new("avatar", layer, SceneNodeKind::Raster(RasterNode::new(img)));
        avatar.transform = Transform::new(0.3, 0.0, 0.0, 0.3, 120.0, 160.0);
        doc.add_node(avatar, None);

        // Center-aligned wordmark whose origin is on the right half of the card.
        let mut wm = TextNode::new("UNNAMED DEVELOPMENT");
        wm.font_family = "sans-serif".into();
        wm.font_size = 40.0;
        wm.align = TextAlign::Center;
        wm.fill = Fill {
            kind: FillKind::Solid(Color::new(0.9, 0.9, 0.95, 1.0)),
            opacity: 1.0,
            enabled: true,
        };
        let mut wm_node = SceneNode::new("wordmark", layer, SceneNodeKind::Text(wm));
        wm_node.transform = Transform::translate(760.0, 470.0);
        let wm_id = wm_node.id;
        doc.add_node(wm_node, None);
        (doc, wm_id)
    }

    fn state_with(doc: Document) -> AppState {
        let (tx, _rx) = std::sync::mpsc::channel();
        AppState {
            document: Arc::new(Mutex::new(doc)),
            history: Arc::new(Mutex::new(photonic_core::history::CommandHistory::new(100))),
            document_path: Arc::new(StdMutex::new(None)),
            capture_tx: Arc::new(StdMutex::new(tx)),
            config: McpServerConfig::default(),
            path_policy: photonic_core::PathPolicy::test_default(),
            audit_log: Arc::new(StdMutex::new(AuditLog::new())),
            clipboard_ring: Arc::new(crate::handlers::clipboard::new_clipboard_ring()),
            video_engine: Arc::new(crate::handlers::video_jobs::VideoEngineHandle::new()),
            video_jobs: Arc::new(StdMutex::new(
                crate::handlers::video_jobs::JobRegistry::new(),
            )),
        }
    }

    #[tokio::test]
    async fn export_pdf_card_back_cmyk_outline_marks_real_path() {
        let (doc, wm_id) = card_back_doc();

        // ── Issue A (position) on the real path: the handler outlines text via
        // `outline_document_text`; the center wordmark must come out LEFT-anchored
        // at its origin (x≈760), NOT shifted left by half its width. ──
        {
            let mut fs = glyphon::FontSystem::new();
            let outlined = photonic_render::outline_document_text(&doc, &mut fs);
            if let SceneNodeKind::Path(p) = &outlined.nodes.get(&wm_id).unwrap().kind {
                if let Some(bb) = p.path_data.bounding_box() {
                    // Node origin is 0 in local space; left-anchored text starts at
                    // ~0 and extends right. A center-shifted run would start near
                    // -width/2 (strongly negative).
                    assert!(
                        bb.min_x() > -5.0,
                        "wordmark must be left-anchored at its origin (min_x≈0), got {} \
                         (center-shift regression)",
                        bb.min_x()
                    );
                } else {
                    eprintln!("no font available — skipping wordmark position check");
                }
            } else {
                panic!("wordmark should be outlined to a Path node");
            }
        }

        let tmp = std::env::temp_dir().join("photonic_r2_card_back.pdf");
        let args: ExportPdfArgs = serde_json::from_value(serde_json::json!({
            "artboards": ["Card Back"],
            "color_mode": "cmyk",
            "outline_text": true,
            "marks": true,
            "path": tmp.to_string_lossy(),
        }))
        .unwrap();

        let state = state_with(doc);
        let res = export_pdf(&state, args).await;
        assert_ne!(
            res.is_error,
            Some(true),
            "export_pdf handler returned an error: {res:?}"
        );

        let bytes = std::fs::read(&tmp).expect("handler must write the PDF");
        let text = String::from_utf8_lossy(&bytes).into_owned();

        // Issue B — compressed + downsampled, tiny (not the ~11 MB raw dump).
        assert!(
            text.contains("/FlateDecode") || text.contains("/DCTDecode"),
            "embedded raster must be compressed (Flate/DCT), found neither"
        );
        assert!(
            bytes.len() < 2_000_000,
            "card back should be a small fraction of 11 MB, got {} bytes",
            bytes.len()
        );
        // Downsampled: 1200 px native at 0.3× on a 300-dpi doc = 1000 dpi effective
        // → capped to 300 → 360 px. The native 1200 must NOT survive.
        assert!(
            !text.contains("/Width 1200"),
            "oversampled raster must be downsampled below its native 1200 px width"
        );

        // Issue C / X-1a — no live transparency (no SMask); CMYK output.
        assert!(
            !text.contains("/SMask"),
            "CMYK/X-1a must not carry a soft mask"
        );
        assert!(
            text.contains("/DeviceCMYK"),
            "CMYK export must be DeviceCMYK"
        );
        // X-1a is PDF 1.3.
        assert!(
            bytes.starts_with(b"%PDF-1.3"),
            "X-1a export must be PDF 1.3"
        );

        // Outlined text ⇒ no embedded/referenced fonts.
        assert!(
            !text.contains("/Type /Font"),
            "outline_text must leave zero fonts"
        );

        // If the preflight tooling is present, the real artifact must PASS X-1a.
        // The repository's dedicated preflight gate runs this shell script on
        // Ubuntu. On Windows, `bash` resolves to the WSL launcher in CI and can
        // fail before the script is reached, so keep the structural PDF checks
        // above portable and run this optional shell check on Unix only.
        #[cfg(not(windows))]
        {
            let script = concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../scripts/preflight-pdfx.sh"
            );
            if std::path::Path::new(script).exists() {
                if let Ok(out) = std::process::Command::new("bash")
                    .arg(script)
                    .arg(&tmp)
                    .arg("--quiet")
                    .output()
                {
                    // Only assert when the script's own deps (qpdf/pdfinfo) are present;
                    // exit code 2 means a tool was missing → skip, don't fail CI.
                    let code = out.status.code().unwrap_or(2);
                    if code != 2 {
                        assert_eq!(
                            code,
                            0,
                            "preflight-pdfx.sh must PASS on the CMYK card back:\n{}\n{}",
                            String::from_utf8_lossy(&out.stdout),
                            String::from_utf8_lossy(&out.stderr),
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod export_blocking_tests {
    use super::export_artboards;
    use crate::protocol::ExportArtboardsArgs;
    use crate::server::{AppState, McpServerConfig};
    use photonic_core::node::{PathNode, SceneNode, SceneNodeKind};
    use photonic_core::path::PathData;
    use photonic_core::style::{Fill, FillKind};
    use photonic_core::{Artboard, AuditLog, Color, Document};
    use std::sync::{Arc, Mutex as StdMutex};
    use tokio::sync::Mutex;

    fn state_with(doc: Document) -> AppState {
        let (tx, _rx) = std::sync::mpsc::channel();
        AppState {
            document: Arc::new(Mutex::new(doc)),
            history: Arc::new(Mutex::new(photonic_core::history::CommandHistory::new(100))),
            document_path: Arc::new(StdMutex::new(None)),
            capture_tx: Arc::new(StdMutex::new(tx)),
            config: McpServerConfig::default(),
            path_policy: photonic_core::PathPolicy::test_default(),
            audit_log: Arc::new(StdMutex::new(AuditLog::new())),
            clipboard_ring: Arc::new(crate::handlers::clipboard::new_clipboard_ring()),
            video_engine: Arc::new(crate::handlers::video_jobs::VideoEngineHandle::new()),
            video_jobs: Arc::new(StdMutex::new(
                crate::handlers::video_jobs::JobRegistry::new(),
            )),
        }
    }

    /// Regression for the "Cannot block the current thread from within a runtime"
    /// panic: `export_artboards` lazily constructs the export renderer, which takes
    /// a `blocking_lock()` / drives a blocking GPU device request. Before the fix
    /// that construction ran on the tokio runtime thread and this `#[tokio::test]`
    /// panicked. After the fix it runs via `spawn_blocking`, so the handler
    /// COMPLETES and (on a GPU host) writes a PNG.
    #[tokio::test]
    async fn export_artboards_from_runtime_writes_png_without_panicking() {
        let mut doc = Document::new("blk", 64.0, 48.0);
        doc.artboards = vec![Artboard::new("Board", 0.0, 0.0, 64.0, 48.0)];
        doc.active_artboard = Some(doc.artboards[0].id);
        let layer = doc.active_layer_id.unwrap();
        let mut rect = PathNode::new(PathData::rect(0.0, 0.0, 64.0, 48.0));
        rect.fill = Fill {
            kind: FillKind::Solid(Color::new(0.10, 0.30, 0.90, 1.0)),
            opacity: 1.0,
            enabled: true,
        };
        doc.add_node(SceneNode::new("bg", layer, SceneNodeKind::Path(rect)), None);

        let out = std::env::temp_dir().join("photonic_export_blocking_test.png");
        let _ = std::fs::remove_file(&out);
        let state = state_with(doc);

        // The key assertion is simply that this call RETURNS (does not panic) while
        // running on the tokio runtime.
        let res = export_artboards(
            &state,
            ExportArtboardsArgs {
                path: Some(out.to_string_lossy().into_owned()),
                scale: Some(1.0),
                ..Default::default()
            },
        )
        .await;

        // On a GPU host the export succeeds and a valid PNG lands on disk. On a
        // headless host with no GPU adapter the renderer is unavailable and the
        // handler returns a clean error (still no panic) — accept that too.
        if res.is_error != Some(true) {
            let bytes = std::fs::read(&out).expect("export PNG should exist on success");
            assert_eq!(&bytes[0..8], b"\x89PNG\r\n\x1a\n", "output must be a PNG");
            let _ = std::fs::remove_file(&out);
        } else {
            eprintln!("no GPU adapter — export returned an error, but did not panic (ok)");
        }
    }
}

#[cfg(test)]
mod raster_export_tests {
    use super::{export_raster, validate_export_dimensions};
    use crate::handlers::clipboard::new_clipboard_ring;
    use crate::protocol::{ExportRasterArgs, ToolResult};
    use crate::server::{AppState, McpServerConfig};
    use photonic_core::{history::CommandHistory, AuditLog, Document};
    use std::sync::{mpsc, Arc, Mutex as StdMutex};
    use tokio::sync::Mutex;

    fn state_with_capture_receiver() -> (
        AppState,
        mpsc::Receiver<tokio::sync::oneshot::Sender<Vec<u8>>>,
    ) {
        let (capture_tx, capture_rx) = mpsc::channel();
        let state = AppState {
            document: Arc::new(Mutex::new(Document::new("raster export test", 64.0, 48.0))),
            history: Arc::new(Mutex::new(CommandHistory::new(100))),
            document_path: Arc::new(StdMutex::new(None)),
            capture_tx: Arc::new(StdMutex::new(capture_tx)),
            config: McpServerConfig::default(),
            audit_log: Arc::new(StdMutex::new(AuditLog::new())),
            clipboard_ring: Arc::new(new_clipboard_ring()),
            ..AppState::headless_for_test()
        };
        (state, capture_rx)
    }

    fn first_text(result: &ToolResult) -> &str {
        match result.content.first() {
            Some(crate::protocol::ContentItem::Text { text }) => text,
            _ => panic!("expected a text tool result"),
        }
    }

    #[test]
    fn export_dimension_validator_rejects_invalid_optional_dimensions() {
        assert!(validate_export_dimensions(Some(0), Some(1)).is_err());
        assert!(validate_export_dimensions(Some(100_000), None).is_err());
        assert!(validate_export_dimensions(Some(8193), Some(8192)).is_err());
        assert!(validate_export_dimensions(None, None).is_ok());
    }

    #[tokio::test]
    async fn export_raster_rejects_oversized_resize_before_capture() {
        let (state, capture_rx) = state_with_capture_receiver();
        let result = export_raster(
            &state,
            ExportRasterArgs {
                width: Some(100_000),
                height: Some(100_000),
                ..Default::default()
            },
        )
        .await;

        assert_eq!(result.is_error, Some(true));
        assert!(first_text(&result).contains("Invalid output dimensions"));
        assert!(
            capture_rx.try_recv().is_err(),
            "rejected resize must not capture"
        );
    }
}
