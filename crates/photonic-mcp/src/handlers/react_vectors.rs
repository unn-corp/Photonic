//! Bounded local JSX/Tailwind adapter for the CSS vector compiler.
//!
//! React itself is intentionally not executed.  This adapter accepts only a
//! static JSX fragment of intrinsic elements and a small, documented Tailwind
//! utility subset, then delegates all geometry lowering to the core compiler.

use crate::handlers::css_vectors::create_vectors_from_css;
use crate::handlers::lucide_assets::{
    append_lucide_icon, resolve_lucide_icon_set, LucideAsset, LucideDiagnostic,
};
use crate::protocol::{CreateVectorsFromCssArgs, CreateVectorsFromReactArgs, ToolResult};
use crate::server::AppState;
use glyphon::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Weight};
use photonic_core::{
    color::Color,
    history::Command,
    node::{GroupNode, PathNode, SceneNode, SceneNodeKind, TextNode},
    path::PathData,
    style::{Fill, Stroke},
    transform::Transform,
    Document,
};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

const MAX_JSX_BYTES: usize = 256 * 1024;
const MAX_ELEMENTS: usize = 512;
const MAX_SOURCE_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_SVG_FILE_BYTES: u64 = 512 * 1024;
const MAX_SVG_NODES: usize = 4096;
const MAX_SVG_DEPTH: usize = 64;
const MAX_TILES: usize = 64;

pub async fn create_vectors_from_react(
    state: &AppState,
    args: CreateVectorsFromReactArgs,
) -> ToolResult {
    let has_file_snapshot = args.source_path.is_some();
    let has_inline_fragment = args.jsx.is_some();
    let has_legacy_snapshot = args.source.is_some() || args.snapshot.is_some();
    if has_file_snapshot && (has_inline_fragment || has_legacy_snapshot) {
        return ToolResult::error("React source import rejected").with_data(serde_json::json!({
            "diagnostics":[diag(
                "INPUT_CONFLICT",
                "source_path cannot be combined with jsx, source, or snapshot",
                "source_path"
            )],
            "contract_version":2
        }));
    }
    if args.source_path.is_some() {
        return create_source_path_snapshot(state, &args).await;
    }
    if has_legacy_snapshot {
        return ToolResult::error("React source import rejected").with_data(serde_json::json!({
            "diagnostics":[diag(
                "SOURCE_PATH_REQUIRED",
                "static React snapshots must name a local source_path and module_roots; inline source/snapshot input is not executable",
                "source_path"
            )],
            "contract_version":2
        }));
    }
    let Some(jsx) = &args.jsx else {
        return ToolResult::error("provide exactly one supported input: jsx or source_path");
    };
    let css = match jsx_to_css(jsx) {
        Ok(css) => css,
        Err(errors) => {
            return ToolResult::error("React component conversion rejected")
                .with_data(serde_json::json!({"diagnostics": errors, "contract_version": 1}))
        }
    };
    let result = create_vectors_from_css(
        state,
        CreateVectorsFromCssArgs {
            css,
            selector: Some(".component".into()),
            origin: args.origin,
            viewport: args.viewport,
            layer_id: args.layer_id,
            group_name: args.group_name,
            strict: args.strict,
            dry_run: args.dry_run,
        },
    )
    .await;
    result
}

#[derive(Debug, Clone)]
struct ImportedTile {
    name: String,
    description: String,
    icon: String,
    url: String,
    asset: Option<ImportedSvgAsset>,
}
#[derive(Debug, Clone)]
struct ImportedSvgAsset {
    path: PathBuf,
    document: Document,
}
#[derive(Debug, Clone)]
struct LayoutSpec {
    gap: f64,
    desktop_columns: usize,
}
#[derive(Debug, Clone)]
struct ParsedPage {
    tiles: Vec<ImportedTile>,
    layout: LayoutSpec,
    resolved_files: Vec<String>,
    fingerprint: String,
    tile_style: TileStyle,
    source_theme: Theme,
}
#[derive(Debug, Clone)]
struct TileStyle {
    padding: f64,
    content_gap: f64,
    badge: f64,
    icon_radius: f64,
    card_radius: f64,
    card_border_width: f64,
    title_size: f64,
    title_weight: u16,
    description_size: f64,
}
#[derive(Debug, Clone)]
struct Theme {
    card: String,
    foreground: String,
    muted: String,
    border: String,
}

#[derive(Debug, Clone)]
struct ImportedSocialLink {
    href: String,
    label: String,
    handle: String,
    icon: String,
    asset: Option<LucideAsset>,
}

#[derive(Debug, Clone)]
struct ConnectPageSnapshot {
    title: String,
    subtitle: String,
    mailing_title: String,
    mailing_body: String,
    follow_title: String,
    follow_body: String,
    links: Vec<ImportedSocialLink>,
    resolved_files: Vec<String>,
    fingerprint: String,
}

#[derive(Debug, Clone)]
struct ResponsivePx {
    base: f64,
    /// Tailwind min-width rules ordered from narrowest to widest.
    variants: Vec<(f64, f64)>,
}

impl ResponsivePx {
    fn resolve(&self, viewport_width: f64) -> f64 {
        self.variants
            .iter()
            .filter(|(breakpoint, _)| viewport_width >= *breakpoint)
            .map(|(_, value)| *value)
            .next_back()
            .unwrap_or(self.base)
    }
}
fn theme(args: &CreateVectorsFromReactArgs, source: &Theme) -> Result<Theme, String> {
    let t = args.theme_tokens.as_ref();
    let val = |v: Option<&String>, d: &str| {
        let s = v.cloned().unwrap_or_else(|| d.into());
        if Color::from_hex(&s).is_none() {
            Err(format!("theme token must be hex: {s}"))
        } else {
            Ok(s)
        }
    };
    Ok(Theme {
        card: val(t.and_then(|x| x.card.as_ref()), &source.card)?,
        foreground: val(t.and_then(|x| x.foreground.as_ref()), &source.foreground)?,
        muted: val(t.and_then(|x| x.muted_foreground.as_ref()), &source.muted)?,
        border: val(t.and_then(|x| x.border.as_ref()), &source.border)?,
    })
}

/// Safe, source-driven entry point for the first bounded static React page.
/// This is a closed parser for AppDirectory's declarative `tiles.map(AppTile)`
/// form, not a JavaScript interpreter: expressions outside that form fail
/// before document mutation.
async fn create_source_path_snapshot(
    state: &AppState,
    args: &CreateVectorsFromReactArgs,
) -> ToolResult {
    if args.export_name.as_deref() == Some("ModeSelector") {
        return create_checkin_mode_selector(state, args).await;
    }
    if args.export_name.as_deref() == Some("ConnectPage") {
        return create_connect_page(state, args).await;
    }
    let parsed = match read_app_directory(args) {
        Ok(parsed) => parsed,
        Err(mut d) => {
            if let Some(object) = d.as_object_mut() {
                let path = args.source_path.as_deref().unwrap_or("");
                let needle = object
                    .get("value")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                object.insert("source_path".into(), serde_json::json!(path));
                object.insert("span".into(), actual_span(path, &needle));
            }
            return ToolResult::error("React source import rejected")
                .with_data(serde_json::json!({"diagnostics":[d],"contract_version":2}));
        }
    };
    let props = args.props.as_ref().and_then(|v| v.as_object());
    if let Some(props) = props {
        if let Some(unsupported) = props
            .keys()
            .find(|key| !matches!(key.as_str(), "isSuperAdmin" | "loading"))
        {
            return ToolResult::error("React source import rejected").with_data(serde_json::json!({"diagnostics":[diag("SNAPSHOT_PROPS_UNSUPPORTED", "this bounded AppDirectory branch does not silently ignore props that change its visible tree", unsupported)],"contract_version":2}));
        }
    }
    let is_admin = props
        .and_then(|p| p.get("isSuperAdmin"))
        .and_then(|v| v.as_bool())
        == Some(true);
    let loading = props
        .and_then(|p| p.get("loading"))
        .and_then(|v| v.as_bool())
        == Some(false);
    if !is_admin || !loading {
        return ToolResult::error("React source import rejected").with_data(serde_json::json!({"diagnostics":[diag("SNAPSHOT_PROPS", "only the pinned AppDirectory super-admin non-loading snapshot is supported", "props")],"contract_version":2}));
    }
    create_snapshot_nodes(state, args, &parsed, "React source snapshot").await
}

fn actual_span(path: &str, needle: &str) -> serde_json::Value {
    let text = read_bounded_text(
        std::path::Path::new(path),
        "DIAGNOSTIC_SOURCE",
        MAX_SOURCE_FILE_BYTES,
    )
    .unwrap_or_default();
    let start = if needle.is_empty() {
        0
    } else {
        text.find(needle).unwrap_or(0)
    };
    let end = (start + needle.len().max(1)).min(text.len());
    let line = |offset: usize| text[..offset].bytes().filter(|b| *b == b'\n').count() + 1;
    let column =
        |offset: usize| offset - text[..offset].rfind('\n').map(|n| n + 1).unwrap_or(0) + 1;
    serde_json::json!({"byte_start":start,"byte_end":end,"line_start":line(start),"column_start":column(start),"line_end":line(end),"column_end":column(end)})
}

fn read_bounded_bytes(
    path: &std::path::Path,
    code: &str,
    limit: u64,
) -> Result<Vec<u8>, serde_json::Value> {
    let size = std::fs::metadata(path)
        .map_err(|_| {
            diag(
                code,
                "source file metadata is unavailable",
                path.display().to_string(),
            )
        })?
        .len();
    if size > limit {
        return Err(diag(
            &format!("{code}_LIMIT"),
            format!("source file exceeds the {limit}-byte parser limit"),
            path.display().to_string(),
        ));
    }
    let bytes = std::fs::read(path).map_err(|_| {
        diag(
            code,
            "source file is not readable",
            path.display().to_string(),
        )
    })?;
    if bytes.len() as u64 > limit {
        return Err(diag(
            &format!("{code}_LIMIT"),
            format!("source file exceeds the {limit}-byte parser limit"),
            path.display().to_string(),
        ));
    }
    Ok(bytes)
}

fn read_bounded_text(
    path: &std::path::Path,
    code: &str,
    limit: u64,
) -> Result<String, serde_json::Value> {
    let bytes = read_bounded_bytes(path, code, limit)?;
    String::from_utf8(bytes).map_err(|_| {
        diag(
            code,
            "source file is not readable UTF-8",
            path.display().to_string(),
        )
    })
}

#[derive(Debug, Clone)]
struct CheckinPage {
    texts: Vec<String>,
    source_files: Vec<PathBuf>,
    fingerprint: String,
    blue: String,
    gold: String,
    primary_blue: String,
    card_width: f64,
    card_padding: f64,
    card_padding_responsive: ResponsivePx,
    outer_padding: f64,
    card_radius: f64,
    section_gap: f64,
    button_height: f64,
    button_horizontal_padding: f64,
    button_gap: f64,
    button_icon_size: f64,
    button_icon_margin_right: f64,
    card_top_border_width: f64,
    warnings: Vec<serde_json::Value>,
    interactions: Vec<String>,
    icons: Vec<LucideAsset>,
}

fn lucide_diag(error: LucideDiagnostic) -> serde_json::Value {
    diag(error.code, &error.message, &error.value)
}

/// Bounded second vertical slice: the Check-In kiosk mode selector. This is
/// deliberately a static resolver, not React execution. It resolves the local
/// `@/` modules used by the rendered tree, evaluates the two pinned KioskLayout
/// branches, reads literal text/classes/tokens, and lowers that model directly
/// into editable Photonic nodes.
async fn create_checkin_mode_selector(
    state: &AppState,
    args: &CreateVectorsFromReactArgs,
) -> ToolResult {
    let page = match read_checkin_mode_selector(args) {
        Ok(page) => page,
        Err(diagnostic) => {
            let path = args.source_path.as_deref().unwrap_or("");
            let needle = diagnostic
                .get("value")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let mut diagnostic = diagnostic;
            if let Some(object) = diagnostic.as_object_mut() {
                object.insert("source_path".into(), serde_json::json!(path));
                object.insert("span".into(), actual_span(path, &needle));
            }
            return ToolResult::error("React source import rejected")
                .with_data(serde_json::json!({"diagnostics":[diagnostic],"contract_version":2}));
        }
    };
    create_checkin_nodes(state, args, &page).await
}

fn read_checkin_mode_selector(
    args: &CreateVectorsFromReactArgs,
) -> Result<CheckinPage, serde_json::Value> {
    if let Some(dynamic) = args
        .dynamic_content
        .as_ref()
        .and_then(|value| value.as_object())
    {
        if let Some(unsupported) = dynamic
            .keys()
            .find(|key| !matches!(key.as_str(), "backgroundImage" | "enableInactivity"))
        {
            return Err(diag(
                "DYNAMIC_CONTENT_UNSUPPORTED",
                "dynamic_content contains a branch this bounded wrapper does not model",
                unsupported,
            ));
        }
        if dynamic
            .get("backgroundImage")
            .is_some_and(|value| !value.is_null())
            || dynamic
                .get("enableInactivity")
                .and_then(|value| value.as_bool())
                != Some(false)
        {
            return Err(diag(
                "DYNAMIC_CONTENT_UNSUPPORTED",
                "ModeSelector requires backgroundImage=null and enableInactivity=false",
                "dynamic_content",
            ));
        }
    }
    if !matches!(
        args.interaction_policy.as_deref().unwrap_or("reject"),
        "reject" | "strip"
    ) {
        return Err(diag(
            "INTERACTION_POLICY",
            "interaction_policy must be reject or strip",
            args.interaction_policy.as_deref().unwrap_or(""),
        ));
    }
    let source_name = args
        .source_path
        .as_deref()
        .ok_or_else(|| diag("SOURCE_PATH", "source_path is required", ""))?;
    let roots: Vec<_> = args
        .module_roots
        .iter()
        .map(std::fs::canonicalize)
        .collect::<Result<_, _>>()
        .map_err(|_| {
            diag(
                "MODULE_ROOTS",
                "a module root does not exist",
                "module_roots",
            )
        })?;
    if roots.is_empty() {
        return Err(diag(
            "MODULE_ROOTS",
            "module_roots is required",
            "module_roots",
        ));
    }
    let source = std::fs::canonicalize(source_name).map_err(|_| {
        diag(
            "SOURCE_NOT_FOUND",
            "source_path does not exist",
            source_name,
        )
    })?;
    let root = roots
        .iter()
        .filter(|root| source.starts_with(root))
        .max_by_key(|root| root.components().count())
        .ok_or_else(|| {
            diag(
                "SOURCE_OUTSIDE_ROOT",
                "source_path must be inside module_roots",
                source_name,
            )
        })?;
    let app_root = bounded_file(root, "apps/checkin/src", "CHECKIN_SOURCE_ROOT")?;
    if !source.starts_with(&app_root) {
        return Err(diag(
            "SOURCE_UNSUPPORTED",
            "ModeSelector must resolve beneath apps/checkin/src",
            source_name,
        ));
    }
    let mode = read_bounded_text(&source, "SOURCE_READ", MAX_SOURCE_FILE_BYTES)?;
    for required in [
        "export function ModeSelector",
        "<KioskLayout enableInactivity={false}>",
        "from '@/components/ui/button'",
        "from '@/components/layout'",
        "from 'lucide-react'",
    ] {
        if !mode.contains(required) {
            return Err(diag(
                "SOURCE_UNSUPPORTED",
                "source is outside the bounded ModeSelector form",
                required,
            ));
        }
    }

    let rendered = return_jsx(&mode)?;
    if rendered.contains("{activeBackground") || rendered.contains("{enableInactivity") {
        return Err(diag(
            "JSX_UNSUPPORTED_EXPRESSION",
            "ModeSelector contains an unsupported rendered expression",
            "{activeBackground",
        ));
    }
    let handlers = event_handlers(rendered);
    if !handlers.is_empty() && args.interaction_policy.as_deref().unwrap_or("reject") != "strip" {
        return Err(diag(
            "JSX_INTERACTION_UNSUPPORTED",
            "rendered event handlers require interaction_policy=strip",
            &handlers[0],
        ));
    }
    let stripped = strip_jsx_comments_and_handlers(rendered);
    if let Some(token) = first_unsupported_rendered_expression(&stripped) {
        return Err(diag(
            "JSX_UNSUPPORTED_EXPRESSION",
            "rendered expression is outside the bounded static snapshot",
            &token,
        ));
    }
    let texts = literal_jsx_text(&stripped);
    if texts.is_empty() {
        return Err(diag(
            "JSX_EMPTY",
            "ModeSelector has no literal visible text",
            "ModeSelector",
        ));
    }

    let layout_index = resolve_checkin_import(root, "apps/checkin/src/components/layout/index.js")?;
    let layout =
        resolve_checkin_import(root, "apps/checkin/src/components/layout/KioskLayout.jsx")?;
    let button = resolve_checkin_import(root, "apps/checkin/src/components/ui/button.jsx")?;
    let card = resolve_checkin_import(root, "apps/checkin/src/components/ui/card.jsx")?;
    let css = resolve_checkin_import(root, "apps/checkin/src/index.css")?;
    let layout_index_text = read_bounded_text(&layout_index, "IMPORT_READ", MAX_SOURCE_FILE_BYTES)?;
    if !layout_index_text.contains("export { KioskLayout } from './KioskLayout'") {
        return Err(diag(
            "IMPORT_UNRESOLVED",
            "KioskLayout re-export is unsupported",
            "KioskLayout",
        ));
    }
    let layout_text = read_bounded_text(&layout, "IMPORT_READ", MAX_SOURCE_FILE_BYTES)?;
    if !layout_text.contains("{activeBackground && (")
        || !layout_text.contains("{enableInactivity && (")
        || !layout_text.contains("{children}")
    {
        return Err(diag(
            "KIOSK_LAYOUT_UNSUPPORTED",
            "KioskLayout branch structure changed",
            "KioskLayout",
        ));
    }
    // enableInactivity=false and absent backgroundImage make these branches
    // statically unreachable in the accepted snapshot. No hook is invoked.
    let layout_visible = layout_text
        .split("{/* Background layer 2")
        .next()
        .unwrap_or(&layout_text)
        .to_string()
        + layout_text
            .split("{/* Exit button */}")
            .nth(1)
            .unwrap_or("")
            .split("{/* Inactivity monitor */}")
            .next()
            .unwrap_or("");
    let layout_texts = literal_jsx_text(&strip_jsx_comments_and_handlers(&layout_visible));
    let exit_text = layout_texts
        .into_iter()
        .find(|text| text == "Exit Kiosk")
        .ok_or_else(|| {
            diag(
                "KIOSK_LAYOUT_UNSUPPORTED",
                "visible exit label is missing",
                "Exit Kiosk",
            )
        })?;

    let button_text = read_bounded_text(&button, "BUTTON_PRIMITIVE_READ", MAX_SOURCE_FILE_BYTES)?;
    let button_base = string_after(&button_text, "const buttonVariants = cva(")?;
    for class in ["inline-flex", "rounded-lg"] {
        if !button_base.split_whitespace().any(|value| value == class) {
            return Err(diag(
                "BUTTON_PRIMITIVE_UNSUPPORTED",
                "button base class is missing",
                class,
            ));
        }
    }
    let default_classes = object_string(&button_text, "default:")?;
    let large_classes = object_string(&button_text, "lg:")?;
    let primary_token = default_classes
        .split_whitespace()
        .find_map(|class| class.strip_prefix("bg-"))
        .ok_or_else(|| {
            diag(
                "BUTTON_PRIMITIVE_UNSUPPORTED",
                "default button background is missing",
                "default",
            )
        })?;
    let button_height = class_px(large_classes, "h-")?;
    let button_horizontal_padding = class_px(large_classes, "px-")?;
    let button_gap = class_px(button_base, "gap-")?;
    let button_icon_size = descendant_size_px(button_base)?;
    let icon_classes = format!("{mode}\n{layout_text}");
    let button_icon_margin_right = largest_space_class(&icon_classes, "mr-")?;

    let css_text = read_bounded_text(&css, "CSS_READ", MAX_SOURCE_FILE_BYTES)?;
    let blue = css_hex_token(&css_text, "bgch-blue")?;
    let gold = css_hex_token(&css_text, "bgch-gold")?;
    let primary_blue = css_hex_token(&css_text, primary_token)?;
    let card_width = arbitrary_px(&layout_text, "max-w-[")?;
    let card_classes = literal_class_containing(&layout_text, "bg-white p-")?;
    let card_padding = class_px(card_classes, "p-")?;
    let card_padding_responsive = responsive_px_rules(card_classes, card_padding);
    let outer_classes = literal_class_containing(&layout_text, "justify-center p-")?;
    let outer_padding = class_px(outer_classes, "p-")?;
    let card_radius = radius_px(card_classes)?;
    let card_top_border_width = arbitrary_px(&layout_text, "border-t-[")?;
    let section_gap = largest_space_class(&mode, "mb-")?;

    let mut all_texts = vec![exit_text];
    all_texts.extend(texts);
    let icons = resolve_lucide_icon_set(&args.module_roots, &["Calendar", "Users", "LogOut"])
        .map_err(lucide_diag)?;
    let mut source_files = vec![source, layout_index, layout, button, card, css];
    source_files.extend(icons.iter().map(|asset| asset.source_path.clone()));
    source_files.sort();
    source_files.dedup();
    let fingerprint = source_fingerprint(&source_files)?;
    let warnings = handlers
        .iter()
        .map(|handler| serde_json::json!({"severity":"warning","code":"JSX_EVENT_STRIPPED","message":"event handler was removed; JavaScript was not executed","value":handler}))
        .collect();
    Ok(CheckinPage {
        texts: all_texts,
        source_files,
        fingerprint,
        blue,
        gold,
        primary_blue,
        card_width,
        card_padding,
        card_padding_responsive,
        outer_padding,
        card_radius,
        section_gap,
        button_height,
        button_horizontal_padding,
        button_gap,
        button_icon_size,
        button_icon_margin_right,
        card_top_border_width,
        warnings,
        interactions: handlers,
        icons,
    })
}

fn resolve_checkin_import(
    root: &std::path::Path,
    relative: &str,
) -> Result<PathBuf, serde_json::Value> {
    let unresolved = root.join(relative);
    let path = std::fs::canonicalize(&unresolved).map_err(|_| {
        diag(
            "IMPORT_UNRESOLVED",
            "required local import was not found",
            relative,
        )
    })?;
    if !path.starts_with(root) {
        return Err(diag(
            "IMPORT_OUTSIDE_ROOT",
            "resolved import is outside module_roots",
            relative,
        ));
    }
    Ok(path)
}

fn return_jsx(source: &str) -> Result<&str, serde_json::Value> {
    let export = source.find("export function ModeSelector").ok_or_else(|| {
        diag(
            "EXPORT_UNSUPPORTED",
            "ModeSelector export is missing",
            "ModeSelector",
        )
    })?;
    let body = &source[export..];
    let start = body.find("return (").ok_or_else(|| {
        diag(
            "SOURCE_UNSUPPORTED",
            "ModeSelector return is missing",
            "return (",
        )
    })?;
    let rendered = &body[start + "return (".len()..];
    let end = rendered.rfind(");").ok_or_else(|| {
        diag(
            "SOURCE_UNSUPPORTED",
            "ModeSelector return is unterminated",
            ");",
        )
    })?;
    Ok(&rendered[..end])
}

#[derive(Debug, Clone)]
struct JsxAttributeExpression {
    name: String,
    start: usize,
    end: usize,
    expression: String,
}

fn scan_braced_expression(source: &str, start: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut depth = 0usize;
    let mut quote = None;
    let mut i = start;
    while i < bytes.len() {
        let byte = bytes[i];
        if let Some(delimiter) = quote {
            if byte == b'\\' {
                i = i.saturating_add(2);
                continue;
            }
            if byte == delimiter {
                quote = None;
            }
            i += 1;
            continue;
        }
        match byte {
            b'\'' | b'"' | b'`' => quote = Some(byte),
            b'{' => depth += 1,
            b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn jsx_attribute_expressions(source: &str) -> Vec<JsxAttributeExpression> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut in_tag = false;
    let mut quote = None;
    while i < bytes.len() {
        let byte = bytes[i];
        if let Some(delimiter) = quote {
            if byte == b'\\' {
                i = i.saturating_add(2);
                continue;
            }
            if byte == delimiter {
                quote = None;
            }
            i += 1;
            continue;
        }
        if !in_tag {
            if byte == b'<' {
                in_tag = true;
            }
            i += 1;
            continue;
        }
        match byte {
            b'\'' | b'"' => quote = Some(byte),
            b'>' => in_tag = false,
            b if b.is_ascii_alphabetic() || b == b'_' || b == b':' => {
                let start = i;
                i += 1;
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'_' | b':' | b'-'))
                {
                    i += 1;
                }
                let name = &source[start..i];
                let mut value_start = i;
                while value_start < bytes.len() && bytes[value_start].is_ascii_whitespace() {
                    value_start += 1;
                }
                if value_start >= bytes.len() || bytes[value_start] != b'=' {
                    continue;
                }
                value_start += 1;
                while value_start < bytes.len() && bytes[value_start].is_ascii_whitespace() {
                    value_start += 1;
                }
                if value_start >= bytes.len() || bytes[value_start] != b'{' {
                    continue;
                }
                let Some(close) = scan_braced_expression(source, value_start) else {
                    out.push(JsxAttributeExpression {
                        name: name.to_string(),
                        start,
                        end: bytes.len(),
                        expression: source[value_start + 1..].trim().to_string(),
                    });
                    break;
                };
                out.push(JsxAttributeExpression {
                    name: name.to_string(),
                    start,
                    end: close + 1,
                    expression: source[value_start + 1..close].trim().to_string(),
                });
                i = close + 1;
            }
            _ => i += 1,
        }
    }
    out
}

fn event_handlers(source: &str) -> Vec<String> {
    jsx_attribute_expressions(source)
        .into_iter()
        .filter(|expression| expression.name == "onClick")
        .map(|expression| source[expression.start..expression.end].to_string())
        .collect()
}

fn strip_jsx_comments_and_handlers(source: &str) -> String {
    let mut result = source.to_string();
    while let Some(start) = result.find("{/*") {
        let Some(end) = result[start + 3..].find("*/}") else {
            break;
        };
        result.replace_range(start..start + 3 + end + 3, "");
    }
    let mut ranges: Vec<_> = jsx_attribute_expressions(&result)
        .into_iter()
        .filter(|expression| expression.name == "onClick")
        .map(|expression| (expression.start, expression.end))
        .collect();
    ranges.sort_unstable_by_key(|(start, _)| *start);
    for (start, end) in ranges.into_iter().rev() {
        result.replace_range(start..end, "");
    }
    result
}

fn first_unsupported_rendered_expression(source: &str) -> Option<String> {
    let bytes = source.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'{' {
            i += 1;
            continue;
        }
        let end = scan_braced_expression(source, i)?;
        let token = &source[i..=end];
        if token.trim() != "{false}" {
            return Some(token.to_string());
        }
        i = end + 1;
    }
    None
}

fn literal_jsx_text(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = source;
    while let Some(close) = rest.find('>') {
        rest = &rest[close + 1..];
        let Some(open) = rest.find('<') else {
            break;
        };
        let value = rest[..open]
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if !value.is_empty() && !value.contains('{') && !value.contains('}') {
            out.push(value);
        }
        rest = &rest[open..];
    }
    out
}

fn string_after<'a>(source: &'a str, marker: &str) -> Result<&'a str, serde_json::Value> {
    let rest = source
        .find(marker)
        .map(|i| &source[i + marker.len()..])
        .ok_or_else(|| diag("SOURCE_UNSUPPORTED", "required literal is missing", marker))?
        .trim_start();
    let quote = rest
        .chars()
        .next()
        .filter(|c| *c == '\'' || *c == '"')
        .ok_or_else(|| {
            diag(
                "SOURCE_UNSUPPORTED",
                "required value must be a string literal",
                marker,
            )
        })?;
    let end = rest[1..].find(quote).ok_or_else(|| {
        diag(
            "SOURCE_UNSUPPORTED",
            "string literal is unterminated",
            marker,
        )
    })?;
    Ok(&rest[1..end + 1])
}

fn object_string<'a>(source: &'a str, marker: &str) -> Result<&'a str, serde_json::Value> {
    let rest = source
        .find(marker)
        .map(|i| &source[i + marker.len()..])
        .ok_or_else(|| diag("SOURCE_UNSUPPORTED", "variant literal is missing", marker))?
        .trim_start();
    let quote = rest
        .chars()
        .next()
        .filter(|c| *c == '\'' || *c == '"')
        .ok_or_else(|| {
            diag(
                "SOURCE_UNSUPPORTED",
                "variant must be a string literal",
                marker,
            )
        })?;
    let end = rest[1..].find(quote).ok_or_else(|| {
        diag(
            "SOURCE_UNSUPPORTED",
            "variant literal is unterminated",
            marker,
        )
    })?;
    Ok(&rest[1..end + 1])
}

fn bounded_number(raw: &str, min: f64, max: f64) -> Option<f64> {
    let value = raw.parse::<f64>().ok()?;
    value
        .is_finite()
        .then_some(value)
        .filter(|value| *value >= min && *value <= max)
}

fn class_px(classes: &str, prefix: &str) -> Result<f64, serde_json::Value> {
    classes
        .split_whitespace()
        .find_map(|class| class.strip_prefix(prefix))
        .and_then(|n| bounded_number(n, 0., 96.))
        .map(|n| n * 4.0)
        .ok_or_else(|| {
            diag(
                "TAILWIND_UNSUPPORTED",
                "numeric spacing class is required",
                prefix,
            )
        })
}

fn descendant_size_px(classes: &str) -> Result<f64, serde_json::Value> {
    classes
        .split_whitespace()
        .find_map(|class| class.rsplit_once(":size-").map(|(_, value)| value))
        .and_then(|value| bounded_number(value, 0., 96.))
        .map(|value| value * 4.)
        .ok_or_else(|| {
            diag(
                "TAILWIND_UNSUPPORTED",
                "descendant SVG size class is required",
                "[&_svg]:size-",
            )
        })
}

fn responsive_px_rules(classes: &str, base: f64) -> ResponsivePx {
    let mut variants: Vec<(f64, f64)> = Vec::new();
    for class in classes.split_whitespace() {
        let Some((prefix, utility)) = class.split_once(':') else {
            continue;
        };
        let Some(raw) = utility.strip_prefix("px-") else {
            continue;
        };
        let Some(value) = bounded_number(raw, 0., 96.).map(|n| n * 4.) else {
            continue;
        };
        let breakpoint = match prefix {
            "sm" => 640.,
            "md" => 768.,
            "lg" => 1024.,
            "xl" => 1280.,
            "2xl" => 1536.,
            _ => continue,
        };
        variants.push((breakpoint, value));
    }
    variants.sort_by(|a, b| a.0.total_cmp(&b.0));
    ResponsivePx { base, variants }
}

fn radius_px(classes: &str) -> Result<f64, serde_json::Value> {
    for (class, px) in [
        ("rounded-lg", 8.),
        ("rounded-xl", 12.),
        ("rounded-2xl", 16.),
    ] {
        if classes.split_whitespace().any(|value| value == class) {
            return Ok(px);
        }
    }
    Err(diag(
        "TAILWIND_UNSUPPORTED",
        "bounded card radius is required",
        classes,
    ))
}

fn largest_space_class(source: &str, prefix: &str) -> Result<f64, serde_json::Value> {
    source
        .split(|c: char| c.is_whitespace() || c == '"' || c == '\'')
        .filter_map(|class| class.strip_prefix(prefix))
        .filter_map(|value| bounded_number(value, 0., 96.))
        .map(|value| value * 4.)
        .reduce(f64::max)
        .ok_or_else(|| {
            diag(
                "TAILWIND_UNSUPPORTED",
                "required spacing class is missing",
                prefix,
            )
        })
}

fn arbitrary_px(source: &str, marker: &str) -> Result<f64, serde_json::Value> {
    let rest = source
        .find(marker)
        .map(|i| &source[i + marker.len()..])
        .ok_or_else(|| {
            diag(
                "TAILWIND_UNSUPPORTED",
                "arbitrary pixel class is missing",
                marker,
            )
        })?;
    rest.split("px]")
        .next()
        .and_then(|n| bounded_number(n, 0., 4096.))
        .ok_or_else(|| diag("TAILWIND_UNSUPPORTED", "arbitrary size must use px", marker))
}

fn literal_class_containing<'a>(
    source: &'a str,
    needle: &str,
) -> Result<&'a str, serde_json::Value> {
    let at = source.find(needle).ok_or_else(|| {
        diag(
            "TAILWIND_UNSUPPORTED",
            "required literal class list is missing",
            needle,
        )
    })?;
    let before = &source[..at];
    let quote = before
        .rfind(['\'', '"'])
        .ok_or_else(|| diag("TAILWIND_UNSUPPORTED", "class list is malformed", needle))?;
    let delimiter = before.as_bytes()[quote] as char;
    let after = &source[quote + 1..];
    let end = after
        .find(delimiter)
        .ok_or_else(|| diag("TAILWIND_UNSUPPORTED", "class list is unterminated", needle))?;
    Ok(&after[..end])
}

fn css_hex_token(source: &str, name: &str) -> Result<String, serde_json::Value> {
    let marker = format!("--color-{name}:");
    let rest = source
        .find(&marker)
        .map(|i| &source[i + marker.len()..])
        .ok_or_else(|| {
            diag(
                "THEME_TOKEN_MISSING",
                "required color token is missing",
                name,
            )
        })?
        .trim_start();
    let value = rest.split(';').next().unwrap_or("").trim();
    Color::from_hex(value)
        .map(|_| value.to_ascii_lowercase())
        .ok_or_else(|| {
            diag(
                "THEME_TOKEN_UNSUPPORTED",
                "color token must be literal hex",
                value,
            )
        })
}

async fn create_checkin_nodes(
    state: &AppState,
    args: &CreateVectorsFromReactArgs,
    page: &CheckinPage,
) -> ToolResult {
    let (doc_w, doc_h, board_origin, active_layer) = {
        let doc = state.document.lock().await;
        let board = doc.active_artboard();
        (
            board.map_or(doc.width, |artboard| artboard.width),
            board.map_or(doc.height, |artboard| artboard.height),
            board.map_or((0., 0.), |artboard| (artboard.x, artboard.y)),
            doc.active_layer_id,
        )
    };
    let Some(layer) = args.layer_id.or(active_layer) else {
        return ToolResult::error("Document has no active layer");
    };
    let viewport = args
        .viewport
        .as_ref()
        .map(|v| (v.width, v.height))
        .unwrap_or((doc_w, doc_h));
    if viewport.0 < 500. || viewport.1 < 600. || !viewport.0.is_finite() || !viewport.1.is_finite()
    {
        return ToolResult::error("ModeSelector viewport must be finite and at least 500 by 600");
    }
    {
        let doc = state.document.lock().await;
        if !doc.layers.get(&layer).is_some_and(|target| !target.locked) {
            return ToolResult::error("destination layer is missing or locked");
        }
    }
    let origin = args
        .origin
        .as_ref()
        .map(|p| (p.x, p.y))
        .unwrap_or(board_origin);
    if !origin.0.is_finite() || !origin.1.is_finite() {
        return ToolResult::error("ModeSelector origin must be finite");
    }
    let card_padding = page.card_padding_responsive.resolve(viewport.0);
    let mut font_system = FontSystem::new();
    let mut nodes = Vec::new();
    let mut children = vec![rect_node(
        "Kiosk blue background",
        origin.0,
        origin.1,
        viewport.0,
        viewport.1,
        0.,
        &page.blue,
        &page.blue,
        0.,
        layer,
        &mut nodes,
    )];
    let card_w = page.card_width.min(viewport.0 - 40.);
    let card_x = origin.0 + (viewport.0 - card_w) / 2.;
    let card_y = origin.1 + page.outer_padding.max(20.) + 44.;
    let card_h = (viewport.1 - 128.).min(650.);
    let card_id = rect_node(
        "Kiosk content surface",
        card_x,
        card_y,
        card_w,
        card_h,
        page.card_radius,
        "#ffffff",
        "#e5e7eb",
        1.,
        layer,
        &mut nodes,
    );
    if let Some(card) = nodes.iter_mut().find(|node| node.id == card_id) {
        card.tags
            .push("react-css:box-shadow=0_6px_24px_rgba(0,0,0,0.08)".into());
    }
    children.push(card_id);
    children.push(rounded_top_border_node(
        "Kiosk gold accent",
        card_x,
        card_y,
        card_w,
        page.card_radius,
        page.card_top_border_width,
        &page.gold,
        layer,
        &mut nodes,
    ));
    let exit = &page.texts[0];
    let (exit_text_width, exit_text_height) = measure_text(&mut font_system, exit, 16., 500);
    let exit_top = origin.1 + page.outer_padding;
    let exit_gap = page.button_gap + page.button_icon_margin_right;
    let exit_inner_width = page.button_icon_size + exit_gap + exit_text_width;
    let exit_content_width = exit_inner_width + page.button_horizontal_padding * 2.;
    let exit_x = origin.0 + viewport.0 - page.outer_padding - exit_content_width;
    let exit_content_x = exit_x + page.button_horizontal_padding;
    let exit_text = text_node(
        exit,
        exit_content_x + page.button_icon_size + exit_gap,
        exit_top + (page.button_height - exit_text_height) / 2.,
        16.,
        500,
        "#ffffff",
        layer,
        &mut nodes,
    );
    children.push(exit_text);
    let icon_asset = |name: &str| page.icons.iter().find(|asset| asset.name == name);
    let Some(logout) = icon_asset("LogOut") else {
        return ToolResult::error("preflighted LogOut icon is missing");
    };
    match append_lucide_icon(
        logout,
        (
            exit_content_x,
            exit_top + (page.button_height - page.button_icon_size) / 2.,
        ),
        page.button_icon_size,
        Color::WHITE,
        layer,
        &mut nodes,
    ) {
        Ok(id) => children.push(id),
        Err(error) => return ToolResult::error(error.message),
    }
    let content_x = card_x + card_padding;
    let content_w = card_w - card_padding * 2.;
    let mut y = card_y + 48.;
    for text in page.texts.iter().skip(1) {
        let mut positioned_text = None;
        let (size, weight, color, margin) = match text.as_str() {
            "Welcome!" => (28.8, 600, page.blue.as_str(), 46.),
            "Please select your check-in type" => (17.6, 400, "#374151", 50.),
            "Event Check-in" | "General Visit" => (24., 600, page.blue.as_str(), 38.),
            "Checking in for a scheduled event" | "General visit or walk-in" => {
                (17.6, 400, "#4b5563", 60.)
            }
            "Select Event" | "Continue" => {
                let button_top = y - 26.;
                children.push(rect_node(
                    &format!("Button: {text}"),
                    content_x,
                    button_top,
                    content_w,
                    page.button_height,
                    8.,
                    &page.primary_blue,
                    &page.primary_blue,
                    0.,
                    layer,
                    &mut nodes,
                ));
                let icon = if text == "Select Event" {
                    "Calendar"
                } else {
                    "Users"
                };
                let Some(asset) = icon_asset(icon) else {
                    return ToolResult::error(format!("preflighted {icon} icon is missing"));
                };
                let (label_width, label_height) = measure_text(&mut font_system, text, 16., 500);
                let inline_gap = page.button_gap + page.button_icon_margin_right;
                let inline_width = page.button_icon_size + inline_gap + label_width;
                let inline_x = content_x + (content_w - inline_width) / 2.;
                match append_lucide_icon(
                    asset,
                    (
                        inline_x,
                        button_top + (page.button_height - page.button_icon_size) / 2.,
                    ),
                    page.button_icon_size,
                    Color::WHITE,
                    layer,
                    &mut nodes,
                ) {
                    Ok(id) => children.push(id),
                    Err(error) => return ToolResult::error(error.message),
                }
                positioned_text = Some((
                    inline_x + page.button_icon_size + inline_gap,
                    button_top + (page.button_height - label_height) / 2.,
                ));
                (16., 500, "#ffffff", page.button_height + page.section_gap)
            }
            _ => (16., 400, "#111827", 32.),
        };
        let (text_width, _) = measure_text(&mut font_system, text, size, weight);
        let (text_x, text_y) =
            positioned_text.unwrap_or((content_x + (content_w - text_width) / 2., y));
        children.push(text_node(
            text, text_x, text_y, size, weight, color, layer, &mut nodes,
        ));
        y += margin;
        if matches!(
            text.as_str(),
            "Please select your check-in type" | "Select Event"
        ) {
            children.push(rect_node(
                "Section divider",
                content_x,
                y - 16.,
                content_w,
                1.,
                0.,
                "#e5e7eb",
                "#e5e7eb",
                0.,
                layer,
                &mut nodes,
            ));
            y += 20.;
        }
    }
    let root = group_node("BGCH Check-In ModeSelector", children, layer, &mut nodes);
    if let Some(node) = nodes.iter_mut().find(|node| node.id == root) {
        node.name = args
            .group_name
            .clone()
            .unwrap_or_else(|| "BGCH Check-In ModeSelector".into());
        node.tags.push("react-role:page".into());
        node.tags.push("source-export:ModeSelector".into());
        for handler in &page.interactions {
            node.tags.push(format!("stripped-interaction:{handler}"));
        }
    }
    let planned_node_count = nodes.len();
    let created: Vec<_> = if args.dry_run {
        Vec::new()
    } else {
        nodes.iter().map(|node| node.id).collect()
    };
    let data = serde_json::json!({
        "root_node_ids": if args.dry_run { serde_json::json!([]) } else { serde_json::json!([root]) }, "created_node_ids":created,
        "planned_node_count":planned_node_count,
        "node_counts":{"nodes":planned_node_count,"text":page.texts.len(),"interactions_stripped":page.interactions.len()},
        "visible_text":page.texts, "theme":{"bgch_blue":page.blue,"bgch_gold":page.gold,"primary_blue":page.primary_blue},
        "layout":{"card_width_px":page.card_width,"card_padding_px":card_padding,"card_padding_base_px":page.card_padding,"card_padding_breakpoints_px":page.card_padding_responsive.variants.iter().map(|(breakpoint,value)| serde_json::json!({"min_width_px":breakpoint,"value_px":value})).collect::<Vec<_>>(),"outer_padding_px":page.outer_padding,"card_radius_px":page.card_radius,"card_top_border_width_px":page.card_top_border_width,"section_gap_px":page.section_gap,"button_height_px":page.button_height,"button_horizontal_padding_px":page.button_horizontal_padding,"button_gap_px":page.button_gap,"button_icon_size_px":page.button_icon_size,"button_icon_margin_right_px":page.button_icon_margin_right},
        "styles":{"backdrop":page.blue,"card_fill":"#ffffff","card_top_border":page.gold,"button_fill":page.primary_blue},
        "semantic_tree":page.texts.iter().map(|text| serde_json::json!({"kind":"text","value":text})).collect::<Vec<_>>(),
        "stripped_interactions":page.interactions,
        "resolved_files":page.source_files.iter().map(|p|p.display().to_string()).collect::<Vec<_>>(),
        "source_fingerprint":page.fingerprint,"interaction_policy":args.interaction_policy.as_deref().unwrap_or("reject"),
        "diagnostics":page.warnings,"dry_run":args.dry_run,"contract_version":2
    });
    if args.dry_run {
        return ToolResult::text("React source import plan").with_data(data);
    }
    let mut doc = state.document.lock().await;
    if doc.layers.get(&layer).is_none_or(|target| target.locked) {
        return ToolResult::error("destination layer is missing or locked");
    }
    let mut history = state.history.lock().await;
    history.execute_discrete(
        Command::AddSubtree {
            layer_id: layer,
            roots: vec![root],
            nodes,
        },
        &mut doc,
    );
    ToolResult::text("Created editable Check-In vectors and text from React sources")
        .with_data(data)
}

fn read_app_directory(args: &CreateVectorsFromReactArgs) -> Result<ParsedPage, serde_json::Value> {
    let source = args
        .source_path
        .as_deref()
        .ok_or_else(|| diag("SOURCE_PATH", "source_path is required", ""))?;
    if args.export_name.as_deref() != Some("AppDirectory") {
        return Err(diag(
            "STATIC_BRANCH_REQUIRED",
            "this bounded importer supports the static AppDirectory branch only; application roots with hooks, auth, or Firebase require a separately supplied static component branch",
            args.export_name.as_deref().unwrap_or(""),
        ));
    }
    if args.module_roots.is_empty() {
        return Err(diag(
            "MODULE_ROOTS",
            "module_roots is required for file-backed import",
            "",
        ));
    }
    let roots: Result<Vec<PathBuf>, _> = args
        .module_roots
        .iter()
        .map(|r| std::fs::canonicalize(r))
        .collect();
    let roots = roots.map_err(|_| {
        diag(
            "MODULE_ROOTS",
            "a module root does not exist",
            "module_roots",
        )
    })?;
    let source = std::fs::canonicalize(source)
        .map_err(|_| diag("SOURCE_NOT_FOUND", "source_path does not exist", source))?;
    if !roots.iter().any(|root| source.starts_with(root)) {
        return Err(diag(
            "SOURCE_OUTSIDE_ROOT",
            "source_path must be inside module_roots",
            &source.display().to_string(),
        ));
    }
    let text = read_bounded_text(&source, "SOURCE_READ", MAX_SOURCE_FILE_BYTES)?;
    if text.contains("className={") {
        return Err(diag(
            "JSX_UNSUPPORTED_EXPRESSION",
            "dynamic className expressions are not supported",
            "className={…}",
        ));
    }
    // This supplied snapshot has loading=false, super-admin=true and a
    // non-empty literal catalog. The Alert/EmptyState branches are therefore
    // statically unreachable; remove only their known component tags before
    // validating rendered attribute expressions.
    let rendered = strip_unreachable_branches(&text);
    if let Some(token) = unsupported_attribute_expression(&rendered) {
        return Err(diag(
            "JSX_UNSUPPORTED_EXPRESSION",
            "JSX attribute expressions are not supported by this static importer",
            &token,
        ));
    }
    let required = [
        "from '@bgch/waffle'",
        "export function AppDirectory",
        "filterApps(apps",
        "tiles.map",
        "<AppTile",
    ];
    if let Some(missing) = required.iter().find(|token| !text.contains(**token)) {
        return Err(diag(
            "SOURCE_UNSUPPORTED",
            "source is outside the bounded AppDirectory static form",
            missing,
        ));
    }
    let root = roots
        .iter()
        .filter(|root| source.starts_with(root))
        .max_by_key(|root| root.components().count())
        .expect("checked above");
    let catalog = root.join("packages/waffle/src/suiteApps.js");
    let catalog = std::fs::canonicalize(&catalog).map_err(|_| {
        diag(
            "IMPORT_UNRESOLVED",
            "pinned @bgch/waffle suiteApps module was not found",
            "@bgch/waffle",
        )
    })?;
    if !catalog.starts_with(root) {
        return Err(diag(
            "IMPORT_OUTSIDE_ROOT",
            "resolved import is outside module_roots",
            "@bgch/waffle",
        ));
    }
    let catalog_text = read_bounded_text(&catalog, "IMPORT_READ", MAX_SOURCE_FILE_BYTES)?;
    let card_module = bounded_file(root, "packages/ui/src/card.jsx", "CARD_PRIMITIVE")?;
    let card_text = read_bounded_text(&card_module, "CARD_PRIMITIVE_READ", MAX_SOURCE_FILE_BYTES)?;
    let theme_module = bounded_file(root, "packages/theme/tokens.css", "THEME_TOKENS")?;
    let theme_text = read_bounded_text(&theme_module, "THEME_TOKENS_READ", MAX_SOURCE_FILE_BYTES)?;
    let layout = parse_grid_layout(&text)?;
    let tile_style = parse_tile_style(&text, &card_text)?;
    let source_theme = parse_light_theme(&theme_text)?;
    let mut tiles = parse_suite_apps(&catalog_text)?;
    let asset_root = std::fs::canonicalize(root.join("apps/core/public")).map_err(|_| {
        diag(
            "ASSET_ROOT_NOT_FOUND",
            "allowlisted SVG asset root apps/core/public was not found",
            "apps/core/public",
        )
    })?;
    if !asset_root.starts_with(root) {
        return Err(diag(
            "ASSET_ROOT_OUTSIDE_ROOT",
            "allowlisted SVG asset root resolves outside module_roots",
            &asset_root.display().to_string(),
        ));
    }
    for tile in &mut tiles {
        tile.asset = Some(resolve_svg_asset(&tile.icon, &asset_root)?);
    }
    let mut resolved_paths = vec![source, catalog, card_module, theme_module];
    resolved_paths.extend(
        tiles
            .iter()
            .filter_map(|tile| tile.asset.as_ref())
            .map(|asset| asset.path.clone()),
    );
    let fingerprint = source_fingerprint(&resolved_paths)?;
    let resolved_files = resolved_paths
        .iter()
        .map(|path| path.display().to_string())
        .collect();
    Ok(ParsedPage {
        tiles,
        layout,
        resolved_files,
        fingerprint,
        tile_style,
        source_theme,
    })
}

fn bounded_file(
    root: &std::path::Path,
    relative: &str,
    code: &str,
) -> Result<PathBuf, serde_json::Value> {
    let path = std::fs::canonicalize(root.join(relative)).map_err(|_| {
        diag(
            &format!("{code}_NOT_FOUND"),
            "required allowlisted source file was not found",
            relative,
        )
    })?;
    if !path.starts_with(root) {
        return Err(diag(
            &format!("{code}_OUTSIDE_ROOT"),
            "required source file resolves outside module_roots",
            relative,
        ));
    }
    Ok(path)
}

fn read_connect_page(
    args: &CreateVectorsFromReactArgs,
) -> Result<ConnectPageSnapshot, serde_json::Value> {
    if !args.module_roots.is_empty() {
        let roots: Result<Vec<_>, _> = args
            .module_roots
            .iter()
            .map(std::fs::canonicalize)
            .collect();
        let roots = roots.map_err(|_| {
            diag(
                "MODULE_ROOTS",
                "a module root does not exist",
                "module_roots",
            )
        })?;
        let source_path = args
            .source_path
            .as_deref()
            .ok_or_else(|| diag("SOURCE_PATH", "source_path is required", ""))?;
        let source = std::fs::canonicalize(source_path).map_err(|_| {
            diag(
                "SOURCE_NOT_FOUND",
                "source_path does not exist",
                source_path,
            )
        })?;
        if !roots.iter().any(|root| source.starts_with(root)) {
            return Err(diag(
                "SOURCE_OUTSIDE_ROOT",
                "source_path must be inside module_roots",
                &source.display().to_string(),
            ));
        }
        let text = read_bounded_text(&source, "SOURCE_READ", MAX_SOURCE_FILE_BYTES)?;
        if !text.contains("export function ConnectPage") {
            return Err(diag(
                "EXPORT_UNSUPPORTED",
                "source is outside the bounded One Day Dance ConnectPage form",
                "ConnectPage",
            ));
        }
        for required in [
            "const socialLinks = [",
            "<NewsletterSignupWidget />",
            "<SlotImage",
            "Stay in the Loop",
            "Follow along",
        ] {
            if !text.contains(required) {
                return Err(diag(
                    "SOURCE_UNSUPPORTED",
                    "ConnectPage source changed outside the bounded static branch",
                    required,
                ));
            }
        }
        if args
            .props
            .as_ref()
            .and_then(|value| value.as_object())
            .is_some_and(|props| !props.is_empty())
        {
            return Err(diag(
                "SNAPSHOT_PROPS_UNSUPPORTED",
                "ConnectPage does not accept visible props in this static branch",
                "props",
            ));
        }
        let array_start =
            text.find("const socialLinks = [").unwrap() + "const socialLinks = [".len();
        let array_end = text[array_start..]
            .find("]")
            .map(|offset| array_start + offset)
            .ok_or_else(|| {
                diag(
                    "SOURCE_UNSUPPORTED",
                    "socialLinks array is unterminated",
                    "socialLinks",
                )
            })?;
        let array = &text[array_start..array_end];
        let mut links = Vec::new();
        for object in array.split("  {").skip(1) {
            let object = object.split("},").next().unwrap_or(object);
            let href = static_source_field(object, "href")?;
            let label = static_source_field(object, "label")?;
            let handle = static_source_field(object, "handle")?;
            let icon = static_source_identifier(object, "Icon")?;
            links.push(ImportedSocialLink {
                href,
                label,
                handle,
                icon,
                asset: None,
            });
        }
        if links.is_empty() || links.len() > 8 {
            return Err(diag(
                "SOURCE_UNSUPPORTED",
                "ConnectPage requires between one and eight literal social links",
                "socialLinks",
            ));
        }
        let icon_names = links
            .iter()
            .map(|link| link.icon.as_str())
            .collect::<Vec<_>>();
        let assets =
            resolve_lucide_icon_set(&args.module_roots, &icon_names).map_err(lucide_diag)?;
        for (link, asset) in links.iter_mut().zip(assets) {
            link.asset = Some(asset);
        }
        let title = static_source_field_after(
            &text,
            "<h1 className=\"mb-3 text-4xl font-bold tracking-tight\">",
        )?;
        let subtitle = static_source_field_after(&text, "<p className=\"text-muted-foreground\">")?;
        let mailing_title =
            static_source_field_after(&text, "<h2 className=\"mb-1 text-lg font-semibold\">")?;
        let follow_title = static_source_field_after_nth(
            &text,
            "<h2 className=\"mb-1 text-lg font-semibold\">",
            1,
        )?;
        let mailing_body = static_source_field_after(
            &text,
            "<p className=\"mb-4 text-sm text-muted-foreground\">",
        )?;
        let follow_body = static_source_field_after_nth(
            &text,
            "<p className=\"mb-4 text-sm text-muted-foreground\">",
            1,
        )?;
        let mut resolved_paths = vec![source.clone()];
        resolved_paths.extend(
            links
                .iter()
                .filter_map(|link| link.asset.as_ref())
                .map(|asset| asset.source_path.clone()),
        );
        let fingerprint = source_fingerprint(&resolved_paths)?;
        let resolved_files = resolved_paths
            .iter()
            .map(|path| path.display().to_string())
            .collect();
        return Ok(ConnectPageSnapshot {
            title,
            subtitle,
            mailing_title,
            mailing_body,
            follow_title,
            follow_body,
            links,
            resolved_files,
            fingerprint,
        });
    }
    Err(diag(
        "MODULE_ROOTS",
        "module_roots is required for file-backed import",
        "module_roots",
    ))
}

fn static_source_field(object: &str, field: &str) -> Result<String, serde_json::Value> {
    let marker = format!("{field}:");
    let start = object
        .find(&marker)
        .map(|offset| offset + marker.len())
        .ok_or_else(|| {
            diag(
                "SOURCE_UNSUPPORTED",
                "literal social link field is missing",
                field,
            )
        })?;
    let value = object[start..].trim_start();
    let quote = value
        .chars()
        .next()
        .filter(|quote| *quote == '\'' || *quote == '"')
        .ok_or_else(|| {
            diag(
                "SOURCE_DYNAMIC",
                "social link fields must be string literals",
                field,
            )
        })?;
    let end = value[1..].find(quote).ok_or_else(|| {
        diag(
            "SOURCE_UNSUPPORTED",
            "social link string literal is unterminated",
            field,
        )
    })?;
    Ok(value[1..end + 1].to_string())
}

fn static_source_identifier(object: &str, field: &str) -> Result<String, serde_json::Value> {
    let marker = format!("{field}:");
    let start = object
        .find(&marker)
        .map(|offset| offset + marker.len())
        .ok_or_else(|| {
            diag(
                "SOURCE_UNSUPPORTED",
                "social link icon field is missing",
                field,
            )
        })?;
    let identifier = object[start..]
        .trim_start()
        .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
        .next()
        .unwrap_or("");
    if identifier.is_empty() {
        return Err(diag(
            "SOURCE_DYNAMIC",
            "social link icon must be a static identifier",
            field,
        ));
    }
    Ok(identifier.to_string())
}

fn static_source_field_after(source: &str, marker: &str) -> Result<String, serde_json::Value> {
    static_source_field_after_nth(source, marker, 0)
}

fn static_source_field_after_nth(
    source: &str,
    marker: &str,
    occurrence: usize,
) -> Result<String, serde_json::Value> {
    let mut search_from = 0usize;
    let mut start = None;
    for _ in 0..=occurrence {
        let offset = source[search_from..]
            .find(marker)
            .map(|offset| search_from + offset)
            .ok_or_else(|| diag("SOURCE_UNSUPPORTED", "literal page text is missing", marker))?;
        start = Some(offset + marker.len());
        search_from = offset + marker.len();
    }
    let start = start.expect("occurrence loop always sets start");
    let end = source[start..]
        .find("</")
        .map(|offset| start + offset)
        .ok_or_else(|| {
            diag(
                "SOURCE_UNSUPPORTED",
                "literal page text is unterminated",
                marker,
            )
        })?;
    let value = source[start..end].trim();
    if value.is_empty() || value.contains('<') || value.contains('{') {
        return Err(diag(
            "SOURCE_DYNAMIC",
            "page text must be a non-empty literal",
            value,
        ));
    }
    Ok(value.replace("&amp;", "&").replace("&apos;", "'"))
}

async fn create_connect_page(state: &AppState, args: &CreateVectorsFromReactArgs) -> ToolResult {
    let page = match read_connect_page(args) {
        Ok(page) => page,
        Err(diagnostic) => {
            let path = args.source_path.as_deref().unwrap_or("");
            let needle = diagnostic
                .get("value")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            let mut diagnostic = diagnostic;
            if let Some(object) = diagnostic.as_object_mut() {
                object.insert("source_path".into(), serde_json::json!(path));
                object.insert("span".into(), actual_span(path, &needle));
            }
            return ToolResult::error("React source import rejected")
                .with_data(serde_json::json!({"diagnostics":[diagnostic],"contract_version":2}));
        }
    };
    create_connect_nodes(state, args, &page).await
}

fn connect_page_card_height(viewport_height: f64, link_count: usize) -> f64 {
    let base_height = (viewport_height - 236.0).max(360.0);
    let link_count = link_count.max(1) as f64;
    // Social rows begin 116px below the card top. Leave 24px of bottom
    // breathing room after the final 54px row so every accepted link stays
    // inside the card instead of silently overflowing it.
    let required_height = 116.0 + (link_count - 1.0) * 70.0 + 54.0 + 24.0;
    base_height.max(required_height)
}

fn connect_page_canvas_height(viewport_height: f64, card_height: f64) -> f64 {
    // The original layout placed cards 188px below the page top and left 48px
    // below them. Keep that framing when an accepted social-link list makes a
    // card taller than the requested viewport.
    viewport_height.max(188.0 + card_height + 48.0)
}

async fn create_connect_nodes(
    state: &AppState,
    args: &CreateVectorsFromReactArgs,
    page: &ConnectPageSnapshot,
) -> ToolResult {
    let (document_w, document_h, doc_w, doc_h, board_origin, active_artboard_id, active_layer) = {
        let doc = state.document.lock().await;
        let board = doc.active_artboard();
        (
            doc.width,
            doc.height,
            board.map_or(doc.width, |artboard| artboard.width),
            board.map_or(doc.height, |artboard| artboard.height),
            board.map_or((0., 0.), |artboard| (artboard.x, artboard.y)),
            board.map(|artboard| artboard.id),
            doc.active_layer_id,
        )
    };
    let Some(layer) = args.layer_id.or(active_layer) else {
        return ToolResult::error("Document has no active layer");
    };
    let viewport = args
        .viewport
        .as_ref()
        .map(|viewport| (viewport.width, viewport.height))
        .unwrap_or((doc_w, doc_h));
    if !viewport.0.is_finite()
        || !viewport.1.is_finite()
        || viewport.0 < 480.0
        || viewport.1 < 420.0
    {
        return ToolResult::error("viewport must be finite and at least 480 by 420");
    }
    {
        let doc = state.document.lock().await;
        if !doc.layers.get(&layer).is_some_and(|target| !target.locked) {
            return ToolResult::error("destination layer is missing or locked");
        }
    }
    let origin = args
        .origin
        .as_ref()
        .map(|point| (point.x, point.y))
        .unwrap_or(board_origin);
    if !origin.0.is_finite() || !origin.1.is_finite() {
        return ToolResult::error("origin must be finite");
    }
    let content_w = (viewport.0 - 64.0).min(768.0);
    let content_x = origin.0 + (viewport.0 - content_w) / 2.0;
    let gap = 24.0;
    let card_w = (content_w - gap) / 2.0;
    let card_y = origin.1 + 188.0;
    let card_h = connect_page_card_height(viewport.1, page.links.len());
    let canvas_h = connect_page_canvas_height(viewport.1, card_h);
    let page_right = origin.0 + viewport.0;
    let page_bottom = origin.1 + canvas_h;
    let document_width = document_w.max(page_right);
    let document_height = document_h.max(page_bottom);
    let mut nodes = Vec::new();
    let mut children = Vec::new();
    children.push(rect_node(
        "One Day Dance page background",
        origin.0,
        origin.1,
        viewport.0,
        canvas_h,
        0.0,
        "#f2f2f3",
        "#f2f2f3",
        0.0,
        layer,
        &mut nodes,
    ));
    children.push(text_node(
        &page.title,
        content_x,
        origin.1 + 64.0,
        36.0,
        700,
        "#171717",
        layer,
        &mut nodes,
    ));
    children.push(text_node(
        &page.subtitle,
        content_x,
        origin.1 + 120.0,
        16.0,
        400,
        "#6b7280",
        layer,
        &mut nodes,
    ));
    let left_card = rect_node(
        "Join mailing list card",
        content_x,
        card_y,
        card_w,
        card_h,
        12.0,
        "#ffffff",
        "#d9d9de",
        1.0,
        layer,
        &mut nodes,
    );
    let right_card = rect_node(
        "Follow along card",
        content_x + card_w + gap,
        card_y,
        card_w,
        card_h,
        12.0,
        "#ffffff",
        "#d9d9de",
        1.0,
        layer,
        &mut nodes,
    );
    children.push(left_card);
    children.push(right_card);
    children.push(text_node(
        &page.mailing_title,
        content_x + 24.0,
        card_y + 32.0,
        18.0,
        600,
        "#171717",
        layer,
        &mut nodes,
    ));
    children.push(text_node(
        &page.mailing_body,
        content_x + 24.0,
        card_y + 68.0,
        14.0,
        400,
        "#6b7280",
        layer,
        &mut nodes,
    ));
    let input_y = card_y + 126.0;
    let input = rect_node(
        "Newsletter email input (static fallback)",
        content_x + 24.0,
        input_y,
        card_w - 48.0,
        42.0,
        10.0,
        "#f2f2f3",
        "#d9d9de",
        1.0,
        layer,
        &mut nodes,
    );
    children.push(input);
    children.push(text_node(
        "Email address",
        content_x + 38.0,
        input_y + 13.0,
        14.0,
        400,
        "#9b9ba3",
        layer,
        &mut nodes,
    ));
    let subscribe = rect_node(
        "Subscribe button (static fallback)",
        content_x + 24.0,
        input_y + 62.0,
        card_w - 48.0,
        46.0,
        10.0,
        "#ff9d76",
        "#ff9d76",
        0.0,
        layer,
        &mut nodes,
    );
    children.push(subscribe);
    children.push(text_node(
        "Subscribe",
        content_x + card_w / 2.0 - 32.0,
        input_y + 76.0,
        15.0,
        600,
        "#171717",
        layer,
        &mut nodes,
    ));
    children.push(text_node(
        &page.follow_title,
        content_x + card_w + gap + 24.0,
        card_y + 32.0,
        18.0,
        600,
        "#171717",
        layer,
        &mut nodes,
    ));
    children.push(text_node(
        &page.follow_body,
        content_x + card_w + gap + 24.0,
        card_y + 68.0,
        14.0,
        400,
        "#6b7280",
        layer,
        &mut nodes,
    ));
    let social_x = content_x + card_w + gap + 24.0;
    let social_w = card_w - 48.0;
    for (index, link) in page.links.iter().enumerate() {
        let row_y = card_y + 116.0 + index as f64 * 70.0;
        let row = rect_node(
            &format!("Social link: {}", link.label),
            social_x,
            row_y,
            social_w,
            54.0,
            27.0,
            "#f2f2f3",
            "#e4e4e7",
            1.0,
            layer,
            &mut nodes,
        );
        children.push(row);
        let Some(asset) = link.asset.as_ref() else {
            return ToolResult::error(format!("preflighted {} icon is missing", link.icon));
        };
        match append_lucide_icon(
            asset,
            (social_x + 9.0, row_y + 9.0),
            36.0,
            Color::from_hex("#171717").unwrap_or(Color::BLACK),
            layer,
            &mut nodes,
        ) {
            Ok(id) => children.push(id),
            Err(error) => return ToolResult::error(error.message),
        }
        children.push(text_node(
            &link.label,
            social_x + 58.0,
            row_y + 13.0,
            15.0,
            600,
            "#171717",
            layer,
            &mut nodes,
        ));
        children.push(text_node(
            &link.handle,
            social_x + 58.0,
            row_y + 33.0,
            12.0,
            400,
            "#6b7280",
            layer,
            &mut nodes,
        ));
    }
    let root = group_node("One Day Dance ConnectPage", children, layer, &mut nodes);
    if let Some(node) = nodes.iter_mut().find(|node| node.id == root) {
        node.name = args
            .group_name
            .clone()
            .unwrap_or_else(|| "One Day Dance ConnectPage".into());
        node.tags.push("react-role:page".into());
        node.tags.push("source-export:ConnectPage".into());
        node.tags.push("source-fallback:SlotImage".into());
        node.tags
            .push("source-fallback:NewsletterSignupWidget".into());
        for link in &page.links {
            node.tags.push(format!("href:{}", link.href));
        }
    }
    let planned_node_count = nodes.len();
    let created: Vec<_> = if args.dry_run {
        Vec::new()
    } else {
        nodes.iter().map(|node| node.id).collect()
    };
    let diagnostics = vec![
        serde_json::json!({"severity":"warning","code":"STATIC_COMPONENT_FALLBACK","message":"SlotImage has no pinned asset in this source snapshot; the bounded importer used the source fallback background","value":"SlotImage"}),
        serde_json::json!({"severity":"warning","code":"STATIC_COMPONENT_FALLBACK","message":"NewsletterSignupWidget is interactive; the bounded importer emitted a non-interactive editable field/button preview","value":"NewsletterSignupWidget"}),
    ];
    let mut visible_text = vec![
        page.title.clone(),
        page.subtitle.clone(),
        page.mailing_title.clone(),
        page.mailing_body.clone(),
        page.follow_title.clone(),
        page.follow_body.clone(),
    ];
    visible_text.extend(
        page.links
            .iter()
            .flat_map(|link| [link.label.clone(), link.handle.clone()]),
    );
    let semantic_tree = serde_json::json!([
        {"kind":"heading","value":page.title},
        {"kind":"text","value":page.subtitle},
        {"kind":"section","value":page.mailing_title},
        {"kind":"section","value":page.follow_title},
        {"kind":"links","children":page.links.iter().map(|link| serde_json::json!({"kind":"link","href":link.href,"value":link.label,"handle":link.handle})).collect::<Vec<_>>()}
    ]);
    let data = serde_json::json!({
        "root_node_ids":if args.dry_run { serde_json::json!([]) } else { serde_json::json!([root]) },
        "created_node_ids":created,
        "planned_node_count":planned_node_count,
        "node_counts":{"nodes":planned_node_count,"text":6 + page.links.len() * 2,"links":page.links.len(),"interactions_stripped":0},
        "visible_text":visible_text,
        "layout":{"container_width_px":content_w,"canvas_height_px":canvas_h,"document_width_px":document_width,"document_height_px":document_height,"card_width_px":card_w,"card_height_px":card_h,"card_gap_px":gap,"card_radius_px":12.0,"card_padding_px":24.0},
        "styles":{"background":"#f2f2f3","card":"#ffffff","accent":"#ff9d76","foreground":"#171717","muted":"#6b7280"},
        "semantic_tree":semantic_tree,
        "resolved_files":page.resolved_files,
        "source_fingerprint":page.fingerprint,
        "stripped_interactions":[],
        "diagnostics":diagnostics,
        "dry_run":args.dry_run,
        "contract_version":2
    });
    if args.dry_run {
        return ToolResult::text("React source import plan").with_data(data);
    }
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let old_artboards = doc.artboards.clone();
    let mut new_artboards = old_artboards.clone();
    if let Some(active_artboard_id) = active_artboard_id {
        if let Some(artboard) = new_artboards
            .iter_mut()
            .find(|artboard| artboard.id == active_artboard_id)
        {
            let right = (artboard.x + artboard.width).max(page_right);
            let bottom = (artboard.y + artboard.height).max(page_bottom);
            artboard.x = artboard.x.min(origin.0);
            artboard.y = artboard.y.min(origin.1);
            artboard.width = right - artboard.x;
            artboard.height = bottom - artboard.y;
        }
    }
    let mut commands = Vec::new();
    if document_width != doc.width || document_height != doc.height {
        commands.push(Command::ResizeCanvas {
            old_width: doc.width,
            old_height: doc.height,
            new_width: document_width,
            new_height: document_height,
        });
    }
    if new_artboards != old_artboards {
        commands.push(Command::SetArtboards {
            old: old_artboards,
            new: new_artboards,
        });
    }
    commands.push(Command::AddSubtree {
        layer_id: layer,
        roots: vec![root],
        nodes,
    });
    history.execute_discrete(Command::Batch(commands), &mut doc);
    ToolResult::text("Created editable One Day Dance ConnectPage vectors from React sources")
        .with_data(data)
}

fn primitive_classes<'a>(source: &'a str, primitive: &str) -> Result<&'a str, serde_json::Value> {
    let marker = format!("const {primitive} =");
    let body = source
        .find(&marker)
        .map(|start| &source[start..])
        .ok_or_else(|| {
            diag(
                "CARD_PRIMITIVE_UNSUPPORTED",
                "shared Card primitive declaration is missing",
                primitive,
            )
        })?;
    let cn = body
        .find("className={cn(\"")
        .map(|start| &body[start + "className={cn(\"".len()..])
        .ok_or_else(|| {
            diag(
                "CARD_PRIMITIVE_UNSUPPORTED",
                "primitive requires a literal base class merge",
                primitive,
            )
        })?;
    let end = cn.find('"').ok_or_else(|| {
        diag(
            "CARD_PRIMITIVE_UNSUPPORTED",
            "primitive base class literal is unterminated",
            primitive,
        )
    })?;
    Ok(&cn[..end])
}

fn parse_card_primitive(source: &str) -> Result<(f64, f64, &str), serde_json::Value> {
    let classes = primitive_classes(source, "Card")?;
    for required in ["bg-card", "text-card-foreground", "shadow"] {
        if !classes.split_whitespace().any(|class| class == required) {
            return Err(diag(
                "CARD_PRIMITIVE_UNSUPPORTED",
                "Card base classes are outside the bounded primitive",
                required,
            ));
        }
    }
    let radius = if classes
        .split_whitespace()
        .any(|class| class == "rounded-xl")
    {
        12.0
    } else if classes
        .split_whitespace()
        .any(|class| class == "rounded-lg")
    {
        8.0
    } else {
        return Err(diag(
            "CARD_PRIMITIVE_UNSUPPORTED",
            "Card requires rounded-lg or rounded-xl",
            classes,
        ));
    };
    let border = if classes.split_whitespace().any(|class| class == "border-2") {
        2.0
    } else if classes.split_whitespace().any(|class| class == "border") {
        1.0
    } else {
        return Err(diag(
            "CARD_PRIMITIVE_UNSUPPORTED",
            "Card requires a bounded border utility",
            classes,
        ));
    };
    Ok((radius, border, classes))
}

fn parse_light_theme(source: &str) -> Result<Theme, serde_json::Value> {
    let start = source.find(":root").ok_or_else(|| {
        diag(
            "THEME_TOKENS_UNSUPPORTED",
            "theme requires a light :root block",
            ":root",
        )
    })?;
    let body = &source[start..];
    let open = body.find('{').ok_or_else(|| {
        diag(
            "THEME_TOKENS_UNSUPPORTED",
            "light :root block is malformed",
            ":root",
        )
    })?;
    let body = &body[open + 1..];
    let close = body.find('}').ok_or_else(|| {
        diag(
            "THEME_TOKENS_UNSUPPORTED",
            "light :root block is unterminated",
            ":root",
        )
    })?;
    let body = &body[..close];
    let token = |name: &str| -> Result<String, serde_json::Value> {
        let marker = format!("--{name}:");
        let value = body
            .lines()
            .find_map(|line| {
                let line = line.trim();
                line.strip_prefix(&marker)
                    .and_then(|tail| tail.split(';').next())
            })
            .ok_or_else(|| {
                diag(
                    "THEME_TOKEN_MISSING",
                    "required light theme token is missing",
                    name,
                )
            })?;
        hsl_to_hex(value.trim()).ok_or_else(|| {
            diag(
                "THEME_TOKEN_UNSUPPORTED",
                "theme token must be a literal H S% L% value",
                value.trim(),
            )
        })
    };
    Ok(Theme {
        card: token("card")?,
        foreground: token("foreground")?,
        muted: token("muted-foreground")?,
        border: token("border")?,
    })
}

fn hsl_to_hex(value: &str) -> Option<String> {
    let parts: Vec<_> = value.split_whitespace().collect();
    if parts.len() != 3 {
        return None;
    }
    let h = parts[0].parse::<f64>().ok()?;
    let s = parts[1].strip_suffix('%')?.parse::<f64>().ok()? / 100.0;
    let l = parts[2].strip_suffix('%')?.parse::<f64>().ok()? / 100.0;
    if !h.is_finite() || !(0.0..=1.0).contains(&s) || !(0.0..=1.0).contains(&l) {
        return None;
    }
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let h_prime = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (h_prime.rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match h_prime as u8 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    Some(format!(
        "#{:02x}{:02x}{:02x}",
        ((r + m) * 255.0).round() as u8,
        ((g + m) * 255.0).round() as u8,
        ((b + m) * 255.0).round() as u8
    ))
}

fn parse_tile_style(source: &str, card_source: &str) -> Result<TileStyle, serde_json::Value> {
    let content = source
        .find("className=\"flex items-center")
        .map(|i| &source[i..])
        .ok_or_else(|| {
            diag(
                "TAILWIND_UNSUPPORTED",
                "missing literal CardContent layout classes",
                "CardContent",
            )
        })?;
    let end = content
        .find('"')
        .and_then(|i| content[i + 11..].find('"').map(|j| i + 11 + j))
        .ok_or_else(|| {
            diag(
                "JSX_UNSUPPORTED",
                "unterminated CardContent className",
                "CardContent",
            )
        })?;
    let classes = &content[11..end];
    let space = |prefix: &str| {
        classes
            .split_whitespace()
            .find_map(|c| c.strip_prefix(prefix))
            .map(tailwind_space)
            .transpose()
    };
    let base_content = primitive_classes(card_source, "CardContent")?;
    let mut sides = [None; 4];
    for class in base_content
        .split_whitespace()
        .chain(classes.split_whitespace())
    {
        if let Some(token) = class.strip_prefix("p-") {
            let value = tailwind_space(token)?;
            sides = [Some(value); 4];
        } else if let Some(token) = class.strip_prefix("pt-") {
            sides[0] = Some(tailwind_space(token)?);
        }
    }
    let padding = sides[0]
        .filter(|top| sides.iter().all(|side| *side == Some(*top)))
        .ok_or_else(|| {
            diag(
                "CARD_CONTENT_UNSUPPORTED",
                "merged CardContent padding must be uniform",
                base_content,
            )
        })?;
    let content_gap = space("gap-")?
        .ok_or_else(|| diag("TAILWIND_UNSUPPORTED", "CardContent requires gap-N", "gap"))?;
    let badge = source
        .contains("h-12 w-12")
        .then_some(48.)
        .or_else(|| source.contains("h-16 w-16").then_some(64.))
        .ok_or_else(|| {
            diag(
                "TAILWIND_UNSUPPORTED",
                "icon requires matching h-N w-N",
                "img",
            )
        })?;
    let icon_radius = if source.contains("rounded-lg") {
        8.
    } else if source.contains("rounded-xl") {
        12.
    } else {
        return Err(diag(
            "TAILWIND_UNSUPPORTED",
            "icon requires rounded-lg/xl",
            "img",
        ));
    };
    let title_weight = if source.contains("font-bold") {
        700
    } else if source.contains("font-semibold") {
        600
    } else {
        400
    };
    let description_size = if source.contains("text-sm") { 14. } else { 16. };
    let (card_radius, card_border_width, _) = parse_card_primitive(card_source)?;
    Ok(TileStyle {
        padding,
        content_gap,
        badge,
        icon_radius,
        card_radius,
        card_border_width,
        title_size: 16.,
        title_weight,
        description_size,
    })
}

fn resolve_svg_asset(
    icon_url: &str,
    asset_root: &std::path::Path,
) -> Result<ImportedSvgAsset, serde_json::Value> {
    const ORIGIN: &str = "https://people.bgcharlem.org/";
    let basename = icon_url.strip_prefix(ORIGIN).ok_or_else(|| {
        diag(
            "ASSET_URL_UNSUPPORTED",
            "icon URL must use the allowlisted people.bgcharlem.org origin",
            icon_url,
        )
    })?;
    if basename.is_empty()
        || basename.contains('/')
        || basename.contains('\\')
        || basename.contains('?')
        || basename.contains('#')
        || basename == "."
        || basename == ".."
        || !basename.ends_with("-icon.svg")
    {
        return Err(diag(
            "ASSET_BASENAME_UNSAFE",
            "icon URL path must be one literal *-icon.svg basename",
            basename,
        ));
    }
    let candidate = asset_root.join(basename);
    let path = std::fs::canonicalize(&candidate).map_err(|_| {
        diag(
            "ASSET_NOT_FOUND",
            "local SVG matching the icon URL basename was not found",
            basename,
        )
    })?;
    if !path.starts_with(asset_root) || path.parent() != Some(asset_root) {
        return Err(diag(
            "ASSET_OUTSIDE_ROOT",
            "resolved SVG asset is outside the allowlisted asset root",
            &path.display().to_string(),
        ));
    }
    let svg = read_bounded_text(&path, "ASSET_READ", MAX_SVG_FILE_BYTES)?;
    let document = photonic_core::import_svg(&svg).map_err(|error| {
        diag(
            "ASSET_SVG_INVALID",
            &format!("local SVG could not be imported: {error}"),
            &path.display().to_string(),
        )
    })?;
    if !document
        .nodes
        .values()
        .all(|node| matches!(node.kind, SceneNodeKind::Path(_) | SceneNodeKind::Group(_)))
    {
        return Err(diag(
            "ASSET_SVG_UNSUPPORTED",
            "local SVG must contain editable path/group geometry only",
            &path.display().to_string(),
        ));
    }
    validate_svg_document(&document, &path.display().to_string())?;
    if !document.width.is_finite()
        || !document.height.is_finite()
        || document.width <= 0.0
        || document.height <= 0.0
    {
        return Err(diag(
            "ASSET_SVG_VIEWPORT",
            "local SVG must have a finite positive viewport",
            &path.display().to_string(),
        ));
    }
    Ok(ImportedSvgAsset { path, document })
}

fn validate_svg_document(document: &Document, value: &str) -> Result<(), serde_json::Value> {
    if document.nodes.len() > MAX_SVG_NODES {
        return Err(diag(
            "ASSET_SVG_LIMIT",
            format!("SVG contains more than {MAX_SVG_NODES} nodes"),
            value,
        ));
    }
    if document.nodes.values().any(|node| {
        node.transform
            .matrix
            .iter()
            .any(|component| !component.is_finite())
    }) {
        return Err(diag(
            "ASSET_SVG_NONFINITE",
            "SVG geometry contains a non-finite transform",
            value,
        ));
    }
    let mut pending = Vec::new();
    for layer in document
        .layer_order
        .iter()
        .filter_map(|id| document.layers.get(id))
    {
        pending.extend(layer.node_ids.iter().copied().map(|id| (id, 1usize)));
    }
    let mut seen = HashSet::new();
    while let Some((id, depth)) = pending.pop() {
        if depth > MAX_SVG_DEPTH {
            return Err(diag(
                "ASSET_SVG_DEPTH",
                format!("SVG nesting exceeds {MAX_SVG_DEPTH} levels"),
                value,
            ));
        }
        if !seen.insert(id) {
            continue;
        }
        if let Some(SceneNode {
            kind: SceneNodeKind::Group(group),
            ..
        }) = document.nodes.get(&id)
        {
            pending.extend(
                group
                    .children
                    .iter()
                    .copied()
                    .map(|child| (child, depth + 1)),
            );
        }
    }
    Ok(())
}

fn strip_unreachable_branches(source: &str) -> String {
    let mut out = source.to_string();
    // EmptyState is a self-closing component in the bounded AppDirectory
    // grammar. Keep this narrowly scoped: arbitrary component expressions are
    // not silently accepted elsewhere.
    while let Some(start) = out.find("<EmptyState") {
        let Some(end) = out[start..].find("/>") else {
            break;
        };
        out.replace_range(start..start + end + 2, "");
    }
    out
}

fn parse_grid_layout(source: &str) -> Result<LayoutSpec, serde_json::Value> {
    // Bind to the grid in the successful `tiles.map` branch rather than any
    // loading/skeleton or later unrelated grid in the module.
    let branch = source.rfind("tiles.map").ok_or_else(|| {
        diag(
            "JSX_UNSUPPORTED",
            "missing tiles.map success branch",
            "tiles.map",
        )
    })?;
    let start = source[..branch].rfind("className=\"grid ").ok_or_else(|| {
        diag(
            "TAILWIND_UNSUPPORTED",
            "AppDirectory requires a literal grid className",
            "className",
        )
    })?;
    let rest = &source[start + "className=\"".len()..];
    let end = rest.find('"').ok_or_else(|| {
        diag(
            "JSX_UNSUPPORTED",
            "unterminated className literal",
            "className",
        )
    })?;
    let classes = &rest[..end];
    let mut gap = None;
    let mut columns = None;
    for token in classes.split_whitespace() {
        if let Some(n) = token.strip_prefix("gap-") {
            gap = Some(tailwind_space(n)?);
        }
        if let Some(n) = token.strip_prefix("lg:grid-cols-") {
            columns = n.parse::<usize>().ok().filter(|n| *n > 0 && *n <= 12);
        }
    }
    Ok(LayoutSpec {
        gap: gap.ok_or_else(|| diag("TAILWIND_UNSUPPORTED", "grid must declare gap-N", classes))?,
        desktop_columns: columns.ok_or_else(|| {
            diag(
                "TAILWIND_UNSUPPORTED",
                "grid must declare lg:grid-cols-N",
                classes,
            )
        })?,
    })
}

fn unsupported_attribute_expression(source: &str) -> Option<String> {
    for expression in jsx_attribute_expressions(source) {
        // These references are the exact values consumed by the
        // catalog-to-tile lowering; every other expression fails.
        if matches!(
            (expression.name.as_str(), expression.expression.as_str()),
            ("href", "app.url") | ("src", "app.icon") | ("key", "app.id") | ("app", "app")
        ) {
            continue;
        }
        return Some(format!("{}={{{}}}", expression.name, expression.expression));
    }
    None
}
fn tailwind_space(token: &str) -> Result<f64, serde_json::Value> {
    token
        .parse::<f64>()
        .ok()
        .filter(|n| *n >= 0. && *n <= 96.)
        .map(|n| n * 4.)
        .ok_or_else(|| {
            diag(
                "TAILWIND_UNSUPPORTED",
                "gap must be a bounded numeric Tailwind spacing token",
                token,
            )
        })
}
fn source_fingerprint(paths: &[PathBuf]) -> Result<String, serde_json::Value> {
    let mut hash = 14695981039346656037u64;
    for path in paths {
        let bytes = read_bounded_bytes(path, "FINGERPRINT_READ", MAX_SOURCE_FILE_BYTES)?;
        for byte in (bytes.len() as u64).to_le_bytes().into_iter().chain(bytes) {
            hash = (hash ^ byte as u64).wrapping_mul(1099511628211);
        }
    }
    Ok(format!("{hash:016x}"))
}

/// Parses only `const SUITE_APPS = [{ id:'', name:'', icon:'', url:'',
/// description:'' }, ...]`. Bracket tracking handles optional literal arrays
/// such as requiredCapabilities, while every emitted field must be a literal.
fn parse_suite_apps(source: &str) -> Result<Vec<ImportedTile>, serde_json::Value> {
    let origin = literal_binding(source, "ICON_ORIGIN")?;
    let start = source.find("const SUITE_APPS = [").ok_or_else(|| {
        diag(
            "CATALOG_UNSUPPORTED",
            "missing literal SUITE_APPS declaration",
            "SUITE_APPS",
        )
    })?;
    let tail = &source[start..];
    let end = tail.find("];\n").ok_or_else(|| {
        diag(
            "CATALOG_UNSUPPORTED",
            "unterminated SUITE_APPS literal",
            "SUITE_APPS",
        )
    })?;
    let body = &tail[..end];
    let mut objects = Vec::new();
    let mut depth = 0usize;
    let mut object_start = None;
    for (i, c) in body.char_indices() {
        match c {
            '{' => {
                if depth == 0 {
                    object_start = Some(i);
                }
                depth += 1;
            }
            '}' => {
                if depth == 0 {
                    return Err(diag(
                        "CATALOG_UNSUPPORTED",
                        "unbalanced object literal",
                        "SUITE_APPS",
                    ));
                }
                depth -= 1;
                if depth == 0 {
                    objects.push(&body[object_start.expect("set at opening brace")..=i]);
                }
            }
            _ => {}
        }
    }
    if depth != 0 || objects.is_empty() {
        return Err(diag(
            "CATALOG_UNSUPPORTED",
            "SUITE_APPS must contain object literals",
            "SUITE_APPS",
        ));
    }
    if objects.len() > MAX_TILES {
        return Err(diag(
            "CATALOG_LIMIT",
            format!("SUITE_APPS contains more than {MAX_TILES} entries"),
            "SUITE_APPS",
        ));
    }
    objects
        .into_iter()
        .map(|object| {
            Ok(ImportedTile {
                name: literal_field(object, "name")?,
                description: literal_field(object, "description")?,
                icon: icon_field(object, &origin)?,
                url: literal_field(object, "url")?,
                asset: None,
            })
        })
        .collect()
}

fn literal_binding(source: &str, name: &str) -> Result<String, serde_json::Value> {
    let marker = format!("const {name} =");
    let rest = source
        .find(&marker)
        .map(|i| &source[i + marker.len()..])
        .ok_or_else(|| {
            diag(
                "CATALOG_UNSUPPORTED",
                "missing required literal binding",
                name,
            )
        })?
        .trim_start();
    let quote = rest
        .chars()
        .next()
        .filter(|c| *c == '\'' || *c == '"')
        .ok_or_else(|| diag("CATALOG_DYNAMIC", "binding must be a string literal", name))?;
    let end = rest[1..]
        .find(quote)
        .ok_or_else(|| diag("CATALOG_UNSUPPORTED", "unterminated binding literal", name))?;
    Ok(rest[1..end + 1].to_string())
}

fn literal_field(object: &str, field: &str) -> Result<String, serde_json::Value> {
    let marker = format!("{field}:");
    let rest = object
        .find(&marker)
        .map(|i| &object[i + marker.len()..])
        .ok_or_else(|| {
            diag(
                "CATALOG_UNSUPPORTED",
                "missing required literal field",
                field,
            )
        })?
        .trim_start();
    let quote = rest
        .chars()
        .next()
        .filter(|c| *c == '\'' || *c == '"')
        .ok_or_else(|| {
            diag(
                "CATALOG_DYNAMIC",
                "catalog fields must be string literals",
                field,
            )
        })?;
    let end = rest[1..]
        .find(quote)
        .ok_or_else(|| diag("CATALOG_UNSUPPORTED", "unterminated string literal", field))?;
    Ok(rest[1..end + 1].to_string())
}

fn icon_field(object: &str, origin: &str) -> Result<String, serde_json::Value> {
    let marker = "icon:";
    let rest = object
        .find(marker)
        .map(|i| &object[i + marker.len()..])
        .ok_or_else(|| diag("CATALOG_UNSUPPORTED", "missing required icon field", "icon"))?
        .trim_start();
    // `ICON_ORIGIN` is a top-level literal binding in the supported catalog.
    let suffix_start = rest.find("}/").ok_or_else(|| {
        diag(
            "CATALOG_DYNAMIC",
            "icon must use the bounded ICON_ORIGIN template",
            "icon",
        )
    })? + 1;
    let suffix = rest[suffix_start..]
        .trim_start_matches('/')
        .split('`')
        .next()
        .unwrap_or("");
    if suffix.is_empty() {
        return Err(diag(
            "CATALOG_DYNAMIC",
            "icon template must have a literal suffix",
            "icon",
        ));
    }
    Ok(format!("{}/{}", origin.trim_end_matches('/'), suffix))
}

async fn create_snapshot_nodes(
    state: &AppState,
    args: &CreateVectorsFromReactArgs,
    parsed: &ParsedPage,
    root_label: &str,
) -> ToolResult {
    let (doc_w, doc_h, board_origin, active_layer) = {
        let doc = state.document.lock().await;
        let board = doc.active_artboard();
        (
            board.map_or(doc.width, |artboard| artboard.width),
            board.map_or(doc.height, |artboard| artboard.height),
            board.map_or((0., 0.), |artboard| (artboard.x, artboard.y)),
            doc.active_layer_id,
        )
    };
    let Some(layer_id) = args.layer_id.or(active_layer) else {
        return ToolResult::error("Document has no active layer");
    };
    let viewport = args
        .viewport
        .as_ref()
        .map(|v| (v.width, v.height))
        .unwrap_or((doc_w, doc_h));
    if !viewport.0.is_finite()
        || !viewport.1.is_finite()
        || viewport.0 < 240.0
        || viewport.1 < 180.0
    {
        return ToolResult::error("viewport must be finite and at least 240 by 180");
    }
    {
        let doc = state.document.lock().await;
        if !doc.layers.get(&layer_id).is_some_and(|l| !l.locked) {
            return ToolResult::error("destination layer is missing or locked");
        }
    }
    let origin = args
        .origin
        .as_ref()
        .map(|p| (p.x, p.y))
        .unwrap_or(board_origin);
    if !origin.0.is_finite() || !origin.1.is_finite() {
        return ToolResult::error("origin must be finite");
    }
    let theme = match theme(args, &parsed.source_theme) {
        Ok(v) => v,
        Err(e) => return ToolResult::error(e),
    };
    let mut nodes = Vec::new();
    let root = layout_app_directory(
        &parsed.tiles,
        &parsed.layout,
        &parsed.tile_style,
        &theme,
        origin,
        viewport,
        layer_id,
        &mut nodes,
    );
    if let Some(n) = nodes.iter_mut().find(|n| n.id == root) {
        n.name = args.group_name.clone().unwrap_or_else(|| root_label.into());
        n.tags.push("react-role:page".into());
    }
    let planned_node_count = nodes.len();
    let created: Vec<_> = if args.dry_run {
        Vec::new()
    } else {
        nodes.iter().map(|n| n.id).collect()
    };
    let text_count = parsed.tiles.len() * 2 + 1;
    let semantic_tree: Vec<_> = parsed.tiles.iter().map(|tile| serde_json::json!({"kind":"link","href":tile.url,"children":[{"kind":"image","src":tile.icon},{"kind":"text","value":tile.name},{"kind":"text","value":tile.description}]})).collect();
    let data = serde_json::json!({"root_node_ids": if args.dry_run { serde_json::json!([]) } else { serde_json::json!([root]) },"created_node_ids":created,"planned_node_count":planned_node_count,"node_counts":{"nodes":planned_node_count,"tiles":parsed.tiles.len(),"text":text_count,"images":parsed.tiles.len(),"links":parsed.tiles.len()},"layout":{"columns":parsed.layout.desktop_columns,"gap_px":parsed.layout.gap},"card":{"radius_px":parsed.tile_style.card_radius,"border_width_px":parsed.tile_style.card_border_width,"icon_radius_px":parsed.tile_style.icon_radius},"theme":{"card":theme.card,"foreground":theme.foreground,"muted_foreground":theme.muted,"border":theme.border},"semantic_tree":semantic_tree,"source_fingerprint":parsed.fingerprint,"resolved_files":parsed.resolved_files,"dry_run":args.dry_run,"contract_version":2,"diagnostics":[]});
    if args.dry_run {
        return ToolResult::text("React source import plan").with_data(data);
    }
    let cmd = Command::AddSubtree {
        layer_id,
        roots: vec![root],
        nodes,
    };
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    history.execute_discrete(cmd, &mut doc);
    ToolResult::text("Created editable vectors and text from React source snapshot").with_data(data)
}

fn layout_app_directory(
    tiles: &[ImportedTile],
    layout: &LayoutSpec,
    style: &TileStyle,
    theme: &Theme,
    origin: (f64, f64),
    viewport: (f64, f64),
    layer: uuid::Uuid,
    out: &mut Vec<SceneNode>,
) -> uuid::Uuid {
    // The section has vertical `space-y-*`, not horizontal padding; imported
    // page bounds come directly from origin + viewport.
    let padding = 0.0;
    let gap = layout.gap;
    let cols = if viewport.0 >= 1024.0 {
        layout.desktop_columns
    } else if viewport.0 >= 640.0 {
        2
    } else {
        1
    };
    let card_w = (viewport.0 - padding * 2.0 - gap * (cols - 1) as f64) / cols as f64;
    let card_h = style.badge + style.padding * 2.0;
    let mut children = Vec::new();
    for (i, tile) in tiles.iter().enumerate() {
        let col = i % cols;
        let row = i / cols;
        let x = origin.0 + padding + col as f64 * (card_w + gap);
        let y = origin.1 + padding + row as f64 * (card_h + gap);
        let mut card_children = vec![rect_node(
            "Card surface",
            x,
            y,
            card_w,
            card_h,
            style.card_radius,
            &theme.card,
            &theme.border,
            style.card_border_width,
            layer,
            out,
        )];
        let icon = append_svg_asset(
            tile.asset
                .as_ref()
                .expect("assets are preflighted before layout"),
            &tile.name,
            &tile.icon,
            (x + style.padding, y + style.padding),
            style.badge,
            layer,
            out,
        );
        card_children.push(icon);
        card_children.push(text_node(
            &tile.name,
            x + style.padding + style.badge + style.content_gap,
            y + style.padding + 14.0,
            style.title_size,
            style.title_weight,
            &theme.foreground,
            layer,
            out,
        ));
        card_children.push(text_node(
            &tile.description,
            x + style.padding + style.badge + style.content_gap,
            y + style.padding + 14.0 + style.title_size + 5.0,
            style.description_size,
            400,
            &theme.muted,
            layer,
            out,
        ));
        let tile_group = group_node(
            &format!("App tile: {}", tile.name),
            card_children,
            layer,
            out,
        );
        if let Some(group) = out.iter_mut().find(|n| n.id == tile_group) {
            group.tags.push("react-role:link".into());
            group.tags.push(format!("href:{}", tile.url));
        }
        children.push(tile_group);
    }
    let note_y =
        origin.1 + padding + ((tiles.len() + cols - 1) / cols) as f64 * (card_h + gap) + 10.0;
    children.push(text_node(
        "You'll confirm your Google account once when you open an app.",
        origin.0 + padding,
        note_y,
        12.0,
        400,
        &theme.muted,
        layer,
        out,
    ));
    group_node("BGCH Hub AppDirectory snapshot", children, layer, out)
}

fn append_svg_asset(
    asset: &ImportedSvgAsset,
    app_name: &str,
    source_url: &str,
    origin: (f64, f64),
    size: f64,
    layer: uuid::Uuid,
    out: &mut Vec<SceneNode>,
) -> uuid::Uuid {
    let scale = (size / asset.document.width).min(size / asset.document.height);
    let tx = origin.0 + (size - asset.document.width * scale) / 2.0;
    let ty = origin.1 + (size - asset.document.height * scale) / 2.0;
    let placement = Transform::new(scale, 0.0, 0.0, scale, tx, ty);
    let mut id_map = HashMap::new();
    for old_id in asset.document.nodes.keys() {
        id_map.insert(*old_id, uuid::Uuid::new_v4());
    }
    let source_roots: Vec<_> = asset
        .document
        .layer_order
        .iter()
        .filter_map(|layer_id| asset.document.layers.get(layer_id))
        .flat_map(|source_layer| source_layer.node_ids.iter().copied())
        .collect();
    let mut transforms = HashMap::new();
    let mut import_order = Vec::new();
    for root in &source_roots {
        collect_svg_transforms(
            &asset.document,
            *root,
            placement,
            &mut transforms,
            &mut import_order,
        );
    }
    for old_id in import_order {
        let old_node = &asset.document.nodes[&old_id];
        let mut node = old_node.clone();
        node.id = id_map[&old_node.id];
        node.layer_id = layer;
        let world = transforms[&old_node.id];
        node.transform = world;
        node.transform_user_space_gradients(&world);
        if let SceneNodeKind::Path(path) = &mut node.kind {
            // Photonic strokes are intentionally non-scaling: the renderer
            // cancels the node transform's determinant. SVG strokes scale with
            // their element/viewBox, so bake that scale into stroke metrics.
            let [a, b, c, d, _, _] = world.matrix;
            let stroke_scale = (a * d - b * c).abs().sqrt();
            if path.stroke.enabled && stroke_scale.is_finite() {
                path.stroke.width *= stroke_scale;
                path.stroke.dash_offset *= stroke_scale;
                for dash in &mut path.stroke.dash_array {
                    *dash *= stroke_scale;
                }
            }
        }
        if let SceneNodeKind::Group(group) = &mut node.kind {
            node.transform = Transform::IDENTITY;
            group.children = group
                .children
                .iter()
                .filter_map(|child| id_map.get(child).copied())
                .collect();
            group.clip_node_id = group.clip_node_id.and_then(|id| id_map.get(&id).copied());
            group.blend_spine_id = group.blend_spine_id.and_then(|id| id_map.get(&id).copied());
        }
        out.push(node);
    }
    let roots: Vec<_> = source_roots
        .iter()
        .filter_map(|id| id_map.get(id).copied())
        .collect();
    let mut group = GroupNode::new();
    group.children = roots;
    let mut node = SceneNode::new(
        format!("App icon: {app_name}"),
        layer,
        SceneNodeKind::Group(group),
    );
    node.tags.push("react-role:image".into());
    node.tags.push(format!("source:{source_url}"));
    node.tags
        .push(format!("react-icon-box:{},{},{}", origin.0, origin.1, size));
    node.tags
        .push(format!("source-file:{}", asset.path.display()));
    let id = node.id;
    out.push(node);
    id
}

/// Photonic currently flattens groups for draw order without composing parent
/// transforms. Bake every SVG ancestor transform plus the icon placement into
/// each imported leaf while leaving all imported groups at identity. If native
/// group-transform rendering is added later, descendants will not be doubled.
fn collect_svg_transforms(
    document: &Document,
    node_id: uuid::Uuid,
    parent: Transform,
    transforms: &mut HashMap<uuid::Uuid, Transform>,
    order: &mut Vec<uuid::Uuid>,
) {
    if transforms.contains_key(&node_id) {
        return;
    }
    let Some(node) = document.nodes.get(&node_id) else {
        return;
    };
    let world = parent.then(&node.transform);
    transforms.insert(node_id, world);
    order.push(node_id);
    if let SceneNodeKind::Group(group) = &node.kind {
        for child in &group.children {
            collect_svg_transforms(document, *child, world, transforms, order);
        }
    }
}

fn rect_node(
    name: &str,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    r: f64,
    color: &str,
    stroke_color: &str,
    stroke_width: f64,
    layer: uuid::Uuid,
    out: &mut Vec<SceneNode>,
) -> uuid::Uuid {
    let mut p = PathNode::new(PathData::rounded_rect(x, y, w, h, r));
    p.fill = Fill::solid(Color::from_hex(color).unwrap_or(Color::BLACK));
    p.stroke = Stroke::solid(
        Color::from_hex(stroke_color).unwrap_or(Color::BLACK),
        stroke_width,
    );
    let n = SceneNode::new(name, layer, SceneNodeKind::Path(p));
    let id = n.id;
    out.push(n);
    id
}

fn rounded_top_border_node(
    name: &str,
    x: f64,
    y: f64,
    width: f64,
    radius: f64,
    stroke_width: f64,
    color: &str,
    layer: uuid::Uuid,
    out: &mut Vec<SceneNode>,
) -> uuid::Uuid {
    let half_stroke = stroke_width / 2.;
    let left = x + half_stroke;
    let right = x + width - half_stroke;
    let top = y + half_stroke;
    let inner_radius = (radius - half_stroke).max(0.).min((right - left) / 2.);
    let path = format!(
        "M {left} {} A {inner_radius} {inner_radius} 0 0 1 {} {top} L {} {top} A {inner_radius} {inner_radius} 0 0 1 {right} {}",
        top + inner_radius,
        left + inner_radius,
        right - inner_radius,
        top + inner_radius
    );
    let mut node = PathNode::new(
        PathData::from_svg(&path).expect("generated rounded top-border path must be valid"),
    );
    node.fill = Fill::none();
    node.stroke = Stroke::solid(Color::from_hex(color).unwrap_or(Color::BLACK), stroke_width);
    let mut node = SceneNode::new(name, layer, SceneNodeKind::Path(node));
    node.tags.push("react-css:border-top".into());
    node.tags.push("react-css:overflow-clipped".into());
    let id = node.id;
    out.push(node);
    id
}

fn measure_text(
    font_system: &mut FontSystem,
    content: &str,
    font_size: f64,
    font_weight: u16,
) -> (f64, f64) {
    let font_size = font_size as f32;
    let line_height = font_size * 1.2;
    let mut buffer = Buffer::new(font_system, Metrics::new(font_size, line_height));
    buffer.set_size(font_system, None, None);
    buffer.set_text(
        font_system,
        content,
        Attrs::new()
            .family(Family::Name("Inter"))
            .weight(Weight(font_weight)),
        Shaping::Advanced,
    );
    buffer.shape_until_scroll(font_system, false);
    let width = buffer
        .layout_runs()
        .map(|run| run.line_w)
        .fold(0.0_f32, f32::max);
    let height = buffer.layout_runs().map(|run| run.line_height).sum::<f32>();
    (
        width as f64,
        (if height == 0. { line_height } else { height }) as f64,
    )
}
fn text_node(
    content: &str,
    x: f64,
    y: f64,
    size: f64,
    weight: u16,
    color: &str,
    layer: uuid::Uuid,
    out: &mut Vec<SceneNode>,
) -> uuid::Uuid {
    let mut t = TextNode::new(content);
    t.font_family = "Inter".into();
    t.font_size = size;
    t.font_weight = weight;
    t.fill = Fill::solid(Color::from_hex(color).unwrap_or(Color::BLACK));
    let mut n = SceneNode::new(content, layer, SceneNodeKind::Text(t));
    n.transform = Transform::translate(x, y);
    let id = n.id;
    out.push(n);
    id
}
fn group_node(
    name: &str,
    children: Vec<uuid::Uuid>,
    layer: uuid::Uuid,
    out: &mut Vec<SceneNode>,
) -> uuid::Uuid {
    let mut g = GroupNode::new();
    g.children = children;
    let n = SceneNode::new(name, layer, SceneNodeKind::Group(g));
    let id = n.id;
    out.push(n);
    id
}

fn jsx_to_css(jsx: &str) -> Result<String, Vec<serde_json::Value>> {
    if jsx.len() > MAX_JSX_BYTES {
        return Err(vec![diag("JSX_LIMIT", "JSX input exceeds 256 KiB", "")]);
    }
    // Dynamic expressions are intentionally rejected, rather than evaluated
    // or silently omitted.  That also prohibits imports, hooks, and arbitrary
    // component execution at this boundary.
    if jsx.contains('{') || jsx.contains('}') || jsx.contains("import ") || jsx.contains("export ")
    {
        return Err(vec![diag(
            "JSX_DYNAMIC",
            "only a static JSX fragment is supported",
            "",
        )]);
    }
    let bytes = jsx.as_bytes();
    let mut i = 0;
    // Source tag name and generated selector are both retained: closing tags
    // validate against the former while children extend the latter.
    let mut stack: Vec<(String, String)> = Vec::new();
    let mut rules = Vec::new();
    let mut elements = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            let next = jsx[i..]
                .find('<')
                .map(|offset| i + offset)
                .unwrap_or(bytes.len());
            if !jsx[i..next].trim().is_empty() {
                return Err(vec![diag(
                    "JSX_TEXT_UNSUPPORTED",
                    "text children are not yet vectorizable; use editable text nodes separately",
                    jsx[i..next].trim(),
                )]);
            }
            i = next;
            continue;
        }
        let Some(end_rel) = jsx[i..].find('>') else {
            return Err(vec![diag("JSX_MALFORMED", "unterminated JSX tag", "")]);
        };
        let end = i + end_rel;
        let token = jsx[i + 1..end].trim();
        i = end + 1;
        if token.starts_with('!') {
            continue;
        }
        if let Some(close) = token.strip_prefix('/') {
            let close = close.trim();
            match stack.pop() {
                Some((tag, _)) if tag == close => continue,
                _ => {
                    return Err(vec![diag(
                        "JSX_MALFORMED",
                        "closing tag does not match an open tag",
                        close,
                    )])
                }
            }
        }
        let self_closing = token.ends_with('/');
        let token = token.trim_end_matches('/').trim();
        let tag_end = token.find(char::is_whitespace).unwrap_or(token.len());
        let tag = &token[..tag_end];
        if !matches!(
            tag,
            "div"
                | "section"
                | "main"
                | "article"
                | "header"
                | "footer"
                | "button"
                | "span"
                | "p"
                | "h1"
                | "h2"
                | "h3"
        ) {
            return Err(vec![diag(
                "JSX_UNSUPPORTED_ELEMENT",
                "only intrinsic layout elements are supported",
                tag,
            )]);
        }
        elements += 1;
        if elements > MAX_ELEMENTS {
            return Err(vec![diag(
                "JSX_LIMIT",
                "component contains more than 512 elements",
                tag,
            )]);
        }
        if stack.len() >= 32 {
            return Err(vec![diag(
                "JSX_LIMIT",
                "component nesting exceeds 32 levels",
                tag,
            )]);
        }
        let index = elements;
        let selector = if let Some((_, parent)) = stack.last() {
            format!("{parent} > .node-{index}")
        } else {
            if index != 1 {
                return Err(vec![diag(
                    "JSX_MULTIPLE_ROOTS",
                    "JSX fragment must have exactly one root element",
                    tag,
                )]);
            }
            ".component".into()
        };
        let classes = attr(token, "className")
            .or_else(|| attr(token, "class"))
            .unwrap_or_default();
        let declarations = tailwind(&classes, index == 1)?;
        rules.push(format!("{selector} {{ {declarations} }}"));
        if !self_closing {
            stack.push((tag.to_string(), selector));
        }
    }
    if !stack.is_empty() {
        return Err(vec![diag("JSX_MALFORMED", "unclosed JSX tag", "")]);
    }
    if rules.is_empty() {
        return Err(vec![diag("JSX_EMPTY", "JSX contains no elements", "")]);
    }
    Ok(rules.join("\n"))
}

fn attr(token: &str, name: &str) -> Option<String> {
    let start = token.find(name)? + name.len();
    let value = token[start..].trim_start().strip_prefix('=')?.trim_start();
    let quote = value.chars().next()?;
    if quote != '\'' && quote != '"' {
        return None;
    }
    value[1..]
        .find(quote)
        .map(|end| value[1..1 + end].to_string())
}

fn tailwind(classes: &str, root: bool) -> Result<String, Vec<serde_json::Value>> {
    let mut out = if root {
        vec!["width:100%".into(), "height:100%".into()]
    } else {
        vec!["width:100%".into(), "height:40px".into()]
    };
    for class in classes.split_whitespace() {
        let mapped = match class {
            "w-full" => Some("width:100%".into()),
            "h-full" => Some("height:100%".into()),
            "rounded" => Some("border-radius:4px".into()),
            "rounded-md" => Some("border-radius:6px".into()),
            "rounded-lg" => Some("border-radius:8px".into()),
            "rounded-xl" => Some("border-radius:12px".into()),
            "rounded-full" => Some("border-radius:9999px".into()),
            "border" => Some("border:1px solid #000000".into()),
            "border-2" => Some("border:2px solid #000000".into()),
            "opacity-50" => Some("opacity:0.5".into()),
            "opacity-75" => Some("opacity:0.75".into()),
            "opacity-100" => Some("opacity:1".into()),
            "bg-white" => Some("background:#ffffff".into()),
            "bg-black" => Some("background:#000000".into()),
            "bg-slate-900" => Some("background:#0f172a".into()),
            "bg-slate-100" => Some("background:#f1f5f9".into()),
            "bg-blue-500" => Some("background:#3b82f6".into()),
            "bg-indigo-600" => Some("background:#4f46e5".into()),
            "bg-emerald-500" => Some("background:#10b981".into()),
            "bg-red-500" => Some("background:#ef4444".into()),
            _ => arbitrary_size(class),
        };
        match mapped {
            Some(value) => out.push(value),
            None => {
                return Err(vec![diag(
                    "TAILWIND_UNSUPPORTED",
                    "utility is outside the bounded Tailwind subset",
                    class,
                )])
            }
        }
    }
    Ok(out.join(";"))
}

fn arbitrary_size(class: &str) -> Option<String> {
    let (property, value) = class
        .strip_prefix("w-[")
        .map(|v| ("width", v))
        .or_else(|| class.strip_prefix("h-[").map(|v| ("height", v)))?;
    let value = value.strip_suffix(']')?;
    if value.ends_with("px")
        && value[..value.len() - 2]
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite() && *n >= 0.0 && *n <= 1_000_000.0)
            .is_some()
    {
        Some(format!("{property}:{value}"))
    } else {
        None
    }
}

fn diag(
    code: impl AsRef<str>,
    message: impl AsRef<str>,
    value: impl AsRef<str>,
) -> serde_json::Value {
    serde_json::json!({"severity":"error", "code":code.as_ref(), "message":message.as_ref(), "value":value.as_ref()})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::CssViewportArg;
    use crate::server::McpServerConfig;
    use photonic_core::{history::CommandHistory, AuditLog, Document};
    use std::sync::{Arc, Mutex as StdMutex};
    use tokio::sync::Mutex;

    fn source_test_state() -> AppState {
        let (tx, _rx) = std::sync::mpsc::channel();
        AppState {
            document: Arc::new(Mutex::new(Document::new("t", 1120.0, 720.0))),
            history: Arc::new(Mutex::new(CommandHistory::new(100))),
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
    fn copied_root() -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("photonic-react-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("apps/hub/src/components")).unwrap();
        std::fs::create_dir_all(root.join("packages/waffle/src")).unwrap();
        std::fs::create_dir_all(root.join("packages/ui/src")).unwrap();
        std::fs::create_dir_all(root.join("packages/theme")).unwrap();
        std::fs::create_dir_all(root.join("apps/core/public")).unwrap();
        let app = "import { SUITE_APPS, filterApps } from '@bgch/waffle';\nfunction AppTile(){return <CardContent className=\"flex items-center gap-4 p-5\"><img className=\"h-12 w-12 rounded-lg\"/><span className=\"font-semibold\"/><p className=\"text-sm\"/></CardContent>}\nexport function AppDirectory(){ const tiles = filterApps(apps); return <section><div className=\"grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3\">{tiles.map((app) => (<AppTile key={app.id} app={app} />))}</div></section> }\n";
        let catalog = "const ICON_ORIGIN = 'https://people.bgcharlem.org';\nconst SUITE_APPS = [{ id: 'a', name: 'ONE', icon: `${ICON_ORIGIN}/one-icon.svg`, url: 'https://one.example', description: 'One' }, { id: 'b', name: 'TWO', icon: `${ICON_ORIGIN}/two-icon.svg`, url: 'https://two.example', description: 'Two' }];\n";
        std::fs::write(root.join("apps/hub/src/components/AppDirectory.jsx"), app).unwrap();
        std::fs::write(root.join("packages/waffle/src/suiteApps.js"), catalog).unwrap();
        std::fs::write(
            root.join("packages/ui/src/card.jsx"),
            "const Card = React.forwardRef(() => <div className={cn(\"rounded-xl border bg-card text-card-foreground shadow\", className)} />)\nconst CardContent = React.forwardRef(() => <div className={cn(\"p-6 pt-0\", className)} />)\n",
        ).unwrap();
        std::fs::write(
            root.join("packages/theme/tokens.css"),
            ":root {\n --card: 0 0% 100%;\n --foreground: 224 71.4% 4.1%;\n --muted-foreground: 220 8.9% 43%;\n --border: 220 13% 91%;\n}\n.dark { --card: 0 0% 0%; }\n",
        ).unwrap();
        std::fs::write(
            root.join("apps/core/public/one-icon.svg"),
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><path d="M0 0 L10 0 L10 10 Z" fill="#ff0000"/></svg>"##,
        )
        .unwrap();
        std::fs::write(
            root.join("apps/core/public/two-icon.svg"),
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 10"><g transform="translate(2 1)"><path d="M0 4 L16 4" fill="none" stroke="#00ff00" stroke-width="4"/></g></svg>"##,
        )
        .unwrap();
        root
    }
    fn source_args(root: &std::path::Path, dry_run: bool) -> CreateVectorsFromReactArgs {
        CreateVectorsFromReactArgs {
            jsx: None,
            source: None,
            snapshot: None,
            source_path: Some(
                root.join("apps/hub/src/components/AppDirectory.jsx")
                    .display()
                    .to_string(),
            ),
            export_name: Some("AppDirectory".into()),
            props: Some(serde_json::json!({"isSuperAdmin":true,"loading":false})),
            module_roots: vec![root.display().to_string()],
            theme_tokens: None,
            interaction_policy: None,
            dynamic_content: None,
            origin: None,
            viewport: None,
            layer_id: None,
            group_name: None,
            strict: true,
            dry_run,
        }
    }
    fn copied_checkin_root() -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("photonic-checkin-test-{}", uuid::Uuid::new_v4()));
        for directory in [
            "apps/checkin/src/features/mode-selection/components",
            "apps/checkin/src/components/layout",
            "apps/checkin/src/components/ui",
            "node_modules/lucide-react/dist/esm/icons",
        ] {
            std::fs::create_dir_all(root.join(directory)).unwrap();
        }
        std::fs::write(
            root.join("apps/checkin/src/features/mode-selection/components/ModeSelector.jsx"),
            r#"import { Calendar, Users } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { KioskLayout } from '@/components/layout';
export function ModeSelector() { return (
<KioskLayout enableInactivity={false}>
<h1 className="text-[1.8em] font-semibold mb-4 text-bgch-blue">Welcome!</h1>
<p className="text-[1.1em] mb-6 text-gray-700">Please select your check-in type</p>
<div className="mb-6"><h2 className="text-[1.5em] font-semibold mb-2 text-bgch-blue">Event Check-in</h2><p className="text-[1.1em] mb-4 text-gray-600">Checking in for a scheduled event</p><Button size="lg" className="w-full" onClick={() => event()}><Calendar className="mr-2 h-5 w-5" />Select Event</Button></div>
<div><h2 className="text-[1.5em] font-semibold mb-2 text-bgch-blue">General Visit</h2><p className="text-[1.1em] mb-4 text-gray-600">General visit or walk-in</p><Button size="lg" className="w-full" onClick={() => general()}><Users className="mr-2 h-5 w-5" />Continue</Button></div>
</KioskLayout> ); }"#,
        ).unwrap();
        std::fs::write(
            root.join("apps/checkin/src/components/layout/index.js"),
            "export { KioskLayout } from './KioskLayout';",
        )
        .unwrap();
        std::fs::write(
            root.join("apps/checkin/src/components/layout/KioskLayout.jsx"),
            r#"export function KioskLayout({children,enableInactivity=true,backgroundImage}) { return <div className={cn('min-h-screen w-full relative overflow-hidden','flex items-center justify-center p-5 box-border')}><div style={{backgroundColor:'#005a9c'}} />{/* Background layer 2 - image */}{activeBackground && (<div />)}{/* Exit button */}<Button onClick={handleExit}><LogOut/>Exit Kiosk</Button><div className={cn('relative z-10 w-full max-w-[500px]','bg-white p-5 md:px-10 rounded-xl','border border-gray-200 border-t-[5px] border-t-bgch-gold')}>{children}</div>{/* Inactivity monitor */}{enableInactivity && (<InactivityMonitor />)}</div> }"#,
        ).unwrap();
        std::fs::write(
            root.join("apps/checkin/src/components/ui/button.jsx"),
            r#"const buttonVariants = cva("inline-flex items-center justify-center gap-2 rounded-lg [&_svg]:size-4", { variants:{variant:{default:"bg-primary-blue text-white"},size:{lg:"h-12 px-10 text-base"}}}); export { Button, buttonVariants }"#,
        ).unwrap();
        std::fs::write(
            root.join("apps/checkin/src/components/ui/card.jsx"),
            "export const Card = 'unused';",
        )
        .unwrap();
        std::fs::write(
            root.join("apps/checkin/src/index.css"),
            "@theme { --color-bgch-blue: #005a9c; --color-bgch-gold: #FDB813; --color-primary-blue: #007bff; }",
        ).unwrap();
        for (slug, name, geometry) in [
            (
                "calendar",
                "Calendar",
                r#"["rect", { width: "18", height: "18", x: "3", y: "4", rx: "2", key: "a" }]"#,
            ),
            (
                "users",
                "Users",
                r#"["circle", { cx: "9", cy: "7", r: "4", key: "b" }]"#,
            ),
            (
                "log-out",
                "LogOut",
                r#"["path", { d: "m16 17 5-5-5-5", key: "c" }]"#,
            ),
        ] {
            std::fs::write(
                root.join(format!("node_modules/lucide-react/dist/esm/icons/{slug}.js")),
                format!("const __iconNode = [{geometry}];\nconst {name} = createLucideIcon(\"{slug}\", __iconNode);"),
            ).unwrap();
        }
        root
    }
    fn checkin_args(
        root: &std::path::Path,
        dry_run: bool,
        policy: &str,
    ) -> CreateVectorsFromReactArgs {
        CreateVectorsFromReactArgs {
            jsx: None,
            source: None,
            snapshot: None,
            source_path: Some(
                root.join("apps/checkin/src/features/mode-selection/components/ModeSelector.jsx")
                    .display()
                    .to_string(),
            ),
            export_name: Some("ModeSelector".into()),
            props: Some(serde_json::json!({})),
            module_roots: vec![root.display().to_string()],
            theme_tokens: None,
            interaction_policy: Some(policy.into()),
            dynamic_content: Some(
                serde_json::json!({"backgroundImage":null,"enableInactivity":false}),
            ),
            origin: None,
            viewport: Some(crate::protocol::CssViewportArg {
                width: 1024.,
                height: 768.,
            }),
            layer_id: None,
            group_name: None,
            strict: true,
            dry_run,
        }
    }
    fn plan_json(result: &ToolResult) -> serde_json::Value {
        let crate::protocol::ContentItem::Text { text } = &result.content[1] else {
            panic!("missing JSON plan")
        };
        serde_json::from_str(text).unwrap()
    }
    fn assert_all_icon_leaves_fit(doc: &Document) {
        fn leaves(doc: &Document, id: uuid::Uuid, out: &mut Vec<uuid::Uuid>) {
            let node = &doc.nodes[&id];
            if let SceneNodeKind::Group(group) = &node.kind {
                for child in &group.children {
                    leaves(doc, *child, out);
                }
            } else {
                out.push(id);
            }
        }
        let icons: Vec<_> = doc
            .nodes
            .values()
            .filter(|node| node.name.starts_with("App icon: "))
            .collect();
        assert!(!icons.is_empty());
        for icon in icons {
            let encoded = icon
                .tags
                .iter()
                .find_map(|tag| tag.strip_prefix("react-icon-box:"))
                .unwrap();
            let values: Vec<f64> = encoded
                .split(',')
                .map(|value| value.parse().unwrap())
                .collect();
            let (box_x, box_y, size) = (values[0], values[1], values[2]);
            let mut ids = Vec::new();
            leaves(doc, icon.id, &mut ids);
            assert!(!ids.is_empty(), "{} has no leaves", icon.name);
            for id in ids {
                let node = &doc.nodes[&id];
                let Some(bounds) = node.local_bounds() else {
                    continue;
                };
                let corners = [
                    node.transform.apply(bounds.x0, bounds.y0),
                    node.transform.apply(bounds.x1, bounds.y0),
                    node.transform.apply(bounds.x0, bounds.y1),
                    node.transform.apply(bounds.x1, bounds.y1),
                ];
                let mut min_x = corners.iter().map(|p| p.0).fold(f64::INFINITY, f64::min);
                let mut min_y = corners.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
                let mut max_x = corners
                    .iter()
                    .map(|p| p.0)
                    .fold(f64::NEG_INFINITY, f64::max);
                let mut max_y = corners
                    .iter()
                    .map(|p| p.1)
                    .fold(f64::NEG_INFINITY, f64::max);
                if let SceneNodeKind::Path(path) = &node.kind {
                    if path.stroke.enabled {
                        let half = path.stroke.width / 2.0;
                        min_x -= half;
                        min_y -= half;
                        max_x += half;
                        max_y += half;
                    }
                }
                let tolerance = 0.5;
                assert!(
                    min_x >= box_x - tolerance
                        && min_y >= box_y - tolerance
                        && max_x <= box_x + size + tolerance
                        && max_y <= box_y + size + tolerance,
                    "{} leaf {} bounds ({min_x},{min_y})-({max_x},{max_y}) exceed icon box ({box_x},{box_y}) size {size}",
                    icon.name,
                    node.name
                );
            }
        }
    }
    fn chromatic_pixel_count(png: &[u8]) -> usize {
        image::load_from_memory(png)
            .unwrap()
            .to_rgba8()
            .pixels()
            .filter(|pixel| {
                let [r, g, b, a] = pixel.0;
                a > 200 && r.max(g).max(b) - r.min(g).min(b) > 12
            })
            .count()
    }
    #[test]
    fn static_tailwind_component_becomes_css_tree() {
        let css = jsx_to_css(r#"<div className="w-[320px] h-[160px] bg-slate-900 rounded-xl"><button className="w-[120px] h-[40px] bg-blue-500 rounded" /></div>"#).unwrap();
        assert!(css.contains(".component"));
        assert!(css.contains(".component > .node-2"));
        assert!(css.contains("background:#3b82f6"));
    }
    #[test]
    fn dynamic_jsx_is_rejected_not_ignored() {
        assert!(jsx_to_css("<div>{label}</div>").is_err());
    }

    #[tokio::test]
    async fn checkin_copied_root_is_source_driven_across_content_layout_theme_and_events() {
        let root = copied_checkin_root();
        let state = source_test_state();
        let baseline =
            plan_json(&create_vectors_from_react(&state, checkin_args(&root, true, "strip")).await);
        assert_eq!(baseline["visible_text"][1], "Welcome!");
        assert_eq!(baseline["layout"]["outer_padding_px"], 20.0);
        assert_eq!(baseline["layout"]["card_radius_px"], 12.0);
        assert_eq!(baseline["layout"]["button_height_px"], 48.0);
        assert_eq!(baseline["layout"]["button_gap_px"], 8.0);
        assert_eq!(baseline["layout"]["button_icon_size_px"], 16.0);
        assert_eq!(baseline["layout"]["button_icon_margin_right_px"], 8.0);
        assert_eq!(baseline["layout"]["card_top_border_width_px"], 5.0);
        assert_eq!(baseline["styles"]["backdrop"], "#005a9c");
        assert!(baseline["stripped_interactions"].as_array().unwrap().len() >= 2);

        let mode_path =
            root.join("apps/checkin/src/features/mode-selection/components/ModeSelector.jsx");
        let changed = std::fs::read_to_string(&mode_path)
            .unwrap()
            .replace("Welcome!", "Hello from fixture!")
            .replace("mb-6", "mb-10");
        std::fs::write(&mode_path, changed).unwrap();
        let layout_path = root.join("apps/checkin/src/components/layout/KioskLayout.jsx");
        let layout = std::fs::read_to_string(&layout_path)
            .unwrap()
            .replace("justify-center p-5", "justify-center p-10")
            .replace("rounded-xl", "rounded-2xl");
        std::fs::write(&layout_path, layout).unwrap();
        let css_path = root.join("apps/checkin/src/index.css");
        let css = std::fs::read_to_string(&css_path)
            .unwrap()
            .replace("#005a9c", "#123456");
        std::fs::write(&css_path, css).unwrap();
        let changed =
            plan_json(&create_vectors_from_react(&state, checkin_args(&root, true, "strip")).await);
        assert!(changed["visible_text"]
            .to_string()
            .contains("Hello from fixture!"));
        assert_eq!(changed["layout"]["outer_padding_px"], 40.0);
        assert_eq!(changed["layout"]["card_radius_px"], 16.0);
        assert!(
            changed["layout"]["section_gap_px"].as_f64().unwrap()
                > baseline["layout"]["section_gap_px"].as_f64().unwrap()
        );
        assert_eq!(changed["styles"]["backdrop"], "#123456");
        assert_ne!(
            changed["source_fingerprint"],
            baseline["source_fingerprint"]
        );

        let before = state.document.lock().await.nodes.len();
        let undo = state.history.lock().await.undo_depth();
        let rejected =
            create_vectors_from_react(&state, checkin_args(&root, false, "reject")).await;
        assert_eq!(rejected.is_error, Some(true));
        assert!(format!("{rejected:?}").contains("JSX_INTERACTION_UNSUPPORTED"));
        assert_eq!(state.document.lock().await.nodes.len(), before);
        assert_eq!(state.history.lock().await.undo_depth(), undo);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn checkin_responsive_padding_follows_tailwind_breakpoints() {
        let root = copied_checkin_root();
        let state = source_test_state();
        let mut args = checkin_args(&root, true, "strip");
        args.viewport = Some(CssViewportArg {
            width: 640.,
            height: 800.,
        });
        let narrow = plan_json(&create_vectors_from_react(&state, args.clone()).await);
        assert_eq!(narrow["layout"]["card_padding_px"], 20.0);
        args.viewport = Some(CssViewportArg {
            width: 768.,
            height: 800.,
        });
        let medium = plan_json(&create_vectors_from_react(&state, args).await);
        assert_eq!(medium["layout"]["card_padding_px"], 40.0);
        assert_eq!(medium["layout"]["card_padding_base_px"], 20.0);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn checkin_unknown_rendered_expression_has_span_and_zero_mutation() {
        let root = copied_checkin_root();
        let path =
            root.join("apps/checkin/src/features/mode-selection/components/ModeSelector.jsx");
        let changed = std::fs::read_to_string(&path).unwrap().replace(
            "<h1 className=",
            "<h1 data-fixture = {unknownValue} className=",
        );
        std::fs::write(&path, changed).unwrap();
        let state = source_test_state();
        let result = create_vectors_from_react(&state, checkin_args(&root, false, "strip")).await;
        assert_eq!(result.is_error, Some(true));
        let debug = format!("{result:?}");
        assert!(debug.contains("JSX_UNSUPPORTED_EXPRESSION"), "{debug}");
        assert!(debug.contains("byte_start"), "{debug}");
        assert_eq!(state.document.lock().await.nodes.len(), 0);
        assert_eq!(state.history.lock().await.undo_depth(), 0);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn conflicting_react_inputs_reject_without_mutation() {
        let root = copied_checkin_root();
        let mut args = checkin_args(&root, false, "strip");
        args.jsx = Some("<div className=\"p-2\" />".into());
        let state = source_test_state();
        let result = create_vectors_from_react(&state, args).await;
        assert_eq!(result.is_error, Some(true));
        assert!(format!("{result:?}").contains("INPUT_CONFLICT"));
        assert_eq!(state.document.lock().await.nodes.len(), 0);
        assert_eq!(state.history.lock().await.undo_depth(), 0);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn dry_run_reports_plans_without_node_ids_or_history() {
        let root = copied_root();
        let state = source_test_state();
        let result = create_vectors_from_react(&state, source_args(&root, true)).await;
        assert_ne!(result.is_error, Some(true), "{result:?}");
        let plan = plan_json(&result);
        assert_eq!(plan["dry_run"], true);
        assert!(plan["root_node_ids"].as_array().unwrap().is_empty());
        assert!(plan["created_node_ids"].as_array().unwrap().is_empty());
        assert!(plan["planned_node_count"].as_u64().unwrap() > 0);
        assert_eq!(state.document.lock().await.nodes.len(), 0);
        assert_eq!(state.history.lock().await.undo_depth(), 0);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn numeric_parser_rejects_non_finite_values() {
        assert!(bounded_number("NaN", 0., 96.).is_none());
        assert!(bounded_number("inf", 0., 96.).is_none());
        assert!(class_px("h-NaN", "h-").is_err());
        assert!(arbitrary_px("max-w-[NaNpx]", "max-w-[").is_err());
    }

    #[tokio::test]
    async fn checkin_inline_flex_and_top_border_lower_to_source_geometry() {
        let root = copied_checkin_root();
        let args = checkin_args(&root, false, "strip");
        let page = read_checkin_mode_selector(&args).unwrap();
        let state = source_test_state();
        let result = create_vectors_from_react(&state, args).await;
        assert_ne!(result.is_error, Some(true), "{result:?}");
        let doc = state.document.lock().await;

        let path = |name: &str| {
            let node = doc.nodes.values().find(|node| node.name == name).unwrap();
            let SceneNodeKind::Path(path) = &node.kind else {
                panic!("{name} is not a path")
            };
            path
        };
        let card_bounds = path("Kiosk content surface")
            .path_data
            .bounding_box()
            .unwrap();
        let accent = path("Kiosk gold accent");
        let accent_bounds = accent.path_data.bounding_box().unwrap();
        let half_stroke = accent.stroke.width / 2.;
        let accent_svg = accent.path_data.as_svg().trim_end();
        assert!(!accent_svg.ends_with('Z') && !accent_svg.ends_with('z'));
        assert!((accent_bounds.x0 - half_stroke - card_bounds.x0).abs() < 0.01);
        assert!((accent_bounds.x1 + half_stroke - card_bounds.x1).abs() < 0.01);
        assert!((accent_bounds.y0 - half_stroke - card_bounds.y0).abs() < 0.01);
        assert!(accent_bounds.y1 + half_stroke <= card_bounds.y1);

        let button_bounds = path("Button: Select Event")
            .path_data
            .bounding_box()
            .unwrap();
        let label = doc
            .nodes
            .values()
            .find(|node| node.name == "Select Event")
            .unwrap();
        let icon = doc
            .nodes
            .values()
            .find(|node| node.name == "Lucide icon: Calendar")
            .unwrap();
        let icon_box: Vec<f64> = icon
            .tags
            .iter()
            .find_map(|tag| tag.strip_prefix("react-icon-box:"))
            .unwrap()
            .split(',')
            .map(|value| value.parse().unwrap())
            .collect();
        let mut font_system = FontSystem::new();
        let (label_width, label_height) = measure_text(&mut font_system, "Select Event", 16., 500);
        let inline_left = icon_box[0];
        let inline_right = label.transform.matrix[4] + label_width;
        assert!(((inline_left + inline_right) / 2. - button_bounds.center().x).abs() < 0.01);
        assert!((icon_box[1] + icon_box[2] / 2. - button_bounds.center().y).abs() < 0.01);
        assert!(
            (label.transform.matrix[5] + label_height / 2. - button_bounds.center().y).abs() < 0.01
        );
        assert!(
            (label.transform.matrix[4]
                - icon_box[0]
                - icon_box[2]
                - page.button_gap
                - page.button_icon_margin_right)
                .abs()
                < 0.01
        );
        drop(doc);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn canonical_checkin_mode_selector_renders_when_fixture_is_available() {
        let Ok(root) = std::env::var("PHOTONIC_BGCH_ACCEPTANCE_ROOT") else {
            return;
        };
        let root = std::path::PathBuf::from(root);
        let state = source_test_state();
        let result = create_vectors_from_react(&state, checkin_args(&root, false, "strip")).await;
        assert_ne!(result.is_error, Some(true), "{result:?}");
        let plan = plan_json(&result);
        for expected in [
            "Welcome!",
            "Please select your check-in type",
            "Event Check-in",
            "Checking in for a scheduled event",
            "Select Event",
            "General Visit",
            "General visit or walk-in",
            "Continue",
        ] {
            assert!(plan["visible_text"].to_string().contains(expected));
        }
        assert_eq!(plan["styles"]["backdrop"], "#005a9c");
        assert_eq!(plan["styles"]["button_fill"], "#007bff");
        assert_eq!(plan["layout"]["card_width_px"], 500.0);
        let doc = state.document.lock().await.clone();
        assert!(doc
            .nodes
            .values()
            .any(|node| node.name == "Lucide icon: Calendar"));
        assert!(doc
            .nodes
            .values()
            .any(|node| node.name == "Lucide icon: Users"));
        let renderer = photonic_render::HeadlessRenderer::new().await;
        let png = renderer.render_png_at_size(&doc, 1024, 768);
        assert!(chromatic_pixel_count(&png) > 500);
        std::fs::write("/tmp/photonic-252-checkin-mode-selector.png", png).unwrap();
    }
    #[test]
    fn text_is_rejected_not_silently_dropped() {
        assert!(jsx_to_css("<div>Save</div>").is_err());
    }

    #[test]
    fn catalog_literals_drive_imported_tile_content() {
        let source = "const ICON_ORIGIN = 'https://icons.example';\nconst SUITE_APPS = [\n{ id: 'x', name: 'ALPHA', icon: `${ICON_ORIGIN}/alpha-icon.svg`, url: 'https://alpha.example', description: 'First literal' },\n];\n";
        let mut tiles = parse_suite_apps(source).unwrap();
        assert_eq!(tiles[0].name, "ALPHA");
        assert_eq!(tiles[0].description, "First literal");
        assert_eq!(tiles[0].url, "https://alpha.example");
        assert_eq!(tiles[0].icon, "https://icons.example/alpha-icon.svg");
        // Simulates a copied catalog literal changed after the importer was built:
        // the parsed plan changes without any importer code change.
        let changed = source.replace("First literal", "Changed literal");
        tiles = parse_suite_apps(&changed).unwrap();
        assert_eq!(tiles[0].description, "Changed literal");
    }

    #[test]
    fn catalog_dynamic_field_is_a_diagnostic() {
        let source = "const ICON_ORIGIN = 'https://icons.example';\nconst SUITE_APPS = [\n{ name: app.name, icon: `${ICON_ORIGIN}/x.svg`, url: 'https://x.example', description: 'x' },\n];\n";
        assert_eq!(
            parse_suite_apps(source).unwrap_err()["code"],
            "CATALOG_DYNAMIC"
        );
    }

    #[test]
    fn literal_tailwind_gap_drives_layout_spec() {
        let baseline =
            r#"<div className="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3">{tiles.map("#;
        let changed = baseline.replace("gap-4", "gap-8");
        assert_eq!(parse_grid_layout(baseline).unwrap().gap, 16.0);
        assert_eq!(parse_grid_layout(&changed).unwrap().gap, 32.0);
        assert_eq!(parse_grid_layout(&changed).unwrap().desktop_columns, 3);
    }

    #[test]
    fn last_grid_is_the_active_non_loading_grid() {
        let source = r#"<div className="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3"><div className="grid grid-cols-1 gap-8 sm:grid-cols-2 lg:grid-cols-3">{tiles.map("#;
        assert_eq!(parse_grid_layout(source).unwrap().gap, 32.0);
    }

    #[test]
    fn icon_origin_literal_drives_url() {
        let source = "const ICON_ORIGIN = 'https://new-icons.example';\nconst SUITE_APPS = [{ name: 'A', icon: `${ICON_ORIGIN}/a.svg`, url: 'https://a.example', description: 'A' },\n];\n";
        assert_eq!(
            parse_suite_apps(source).unwrap()[0].icon,
            "https://new-icons.example/a.svg"
        );
    }

    #[test]
    fn unreachable_empty_state_expression_is_not_scanned() {
        let source = "<section><EmptyState icon={AlertCircle} title=\"none\" /><div className=\"grid\">{tiles.map(";
        assert!(unsupported_attribute_expression(&strip_unreachable_branches(source)).is_none());
    }

    #[test]
    fn rendered_unknown_attribute_expression_is_scanned() {
        assert_eq!(
            unsupported_attribute_expression("<section data-fixture={unknownStaticValue}>")
                .as_deref(),
            Some("data-fixture={unknownStaticValue}")
        );
    }

    #[test]
    fn tile_tailwind_literals_drive_style_model() {
        let baseline = r#"<CardContent className="flex items-center gap-4 p-5"><img className="h-12 w-12 rounded-lg"/><span className="font-semibold"/><p className="text-sm"/>"#;
        let changed = baseline
            .replace("gap-4", "gap-8")
            .replace("p-5", "p-8")
            .replace("h-12 w-12", "h-16 w-16")
            .replace("rounded-lg", "rounded-xl")
            .replace("text-sm", "text-base")
            .replace("font-semibold", "font-bold");
        let card = "const Card = <div className={cn(\"rounded-xl border bg-card text-card-foreground shadow\", className)} />; const CardContent = <div className={cn(\"p-6 pt-0\", className)} />;";
        let a = parse_tile_style(baseline, card).unwrap();
        let b = parse_tile_style(&changed, card).unwrap();
        assert_eq!(a.padding, 20.);
        assert_eq!(b.padding, 32.);
        assert_eq!(a.content_gap, 16.);
        assert_eq!(b.content_gap, 32.);
        assert_eq!(a.badge, 48.);
        assert_eq!(b.badge, 64.);
        assert_eq!(a.icon_radius, 8.);
        assert_eq!(b.icon_radius, 12.);
        assert_eq!(a.card_radius, 12.);
        assert_eq!(a.card_border_width, 1.);
        assert_eq!(a.description_size, 14.);
        assert_eq!(b.description_size, 16.);
        assert_eq!(a.title_weight, 600);
        assert_eq!(b.title_weight, 700);
    }

    #[test]
    fn explicit_theme_tokens_drive_paints() {
        let mut args = source_args(std::path::Path::new("/tmp"), true);
        args.theme_tokens = Some(crate::protocol::ReactThemeTokensArg {
            card: Some("#112233".into()),
            foreground: Some("#445566".into()),
            muted_foreground: Some("#778899".into()),
            border: Some("#aabbcc".into()),
        });
        let source = Theme {
            card: "#ffffff".into(),
            foreground: "#000000".into(),
            muted: "#111111".into(),
            border: "#222222".into(),
        };
        let parsed = theme(&args, &source).unwrap();
        assert_eq!(parsed.card, "#112233");
        assert_eq!(parsed.foreground, "#445566");
        assert_eq!(parsed.muted, "#778899");
        assert_eq!(parsed.border, "#aabbcc");
    }

    #[test]
    fn light_hsl_theme_tokens_become_hex() {
        let parsed = parse_light_theme(
            ":root {\n--card: 0 0% 100%;\n--foreground: 224 71.4% 4.1%;\n--muted-foreground: 220 8.9% 43%;\n--border: 220 13% 91%;\n}\n.dark { --card: 0 0% 0%; }",
        )
        .unwrap();
        assert_eq!(parsed.card, "#ffffff");
        assert_eq!(parsed.foreground, "#030712");
        assert_eq!(parsed.muted, "#646a77");
        assert_eq!(parsed.border, "#e5e7eb");
    }

    #[tokio::test]
    async fn copied_card_and_theme_sources_drive_style_and_fingerprint() {
        let root = copied_root();
        let state = source_test_state();
        let baseline =
            plan_json(&create_vectors_from_react(&state, source_args(&root, true)).await);
        assert_eq!(baseline["card"]["radius_px"], 12.0);
        assert_eq!(baseline["card"]["border_width_px"], 1.0);
        assert_eq!(baseline["theme"]["card"], "#ffffff");
        assert_eq!(baseline["theme"]["foreground"], "#030712");
        assert_eq!(baseline["theme"]["muted_foreground"], "#646a77");
        assert_eq!(baseline["theme"]["border"], "#e5e7eb");
        let resolved = baseline["resolved_files"].as_array().unwrap();
        assert!(resolved.iter().any(|path| path
            .as_str()
            .unwrap()
            .replace('\\', "/")
            .ends_with("packages/ui/src/card.jsx")));
        assert!(resolved.iter().any(|path| path
            .as_str()
            .unwrap()
            .replace('\\', "/")
            .ends_with("packages/theme/tokens.css")));

        let card_path = root.join("packages/ui/src/card.jsx");
        let card = std::fs::read_to_string(&card_path)
            .unwrap()
            .replace("rounded-xl border ", "rounded-lg border-2 ");
        std::fs::write(&card_path, card).unwrap();
        let changed_card =
            plan_json(&create_vectors_from_react(&state, source_args(&root, true)).await);
        assert_eq!(changed_card["card"]["radius_px"], 8.0);
        assert_eq!(changed_card["card"]["border_width_px"], 2.0);
        assert_ne!(
            baseline["source_fingerprint"],
            changed_card["source_fingerprint"]
        );

        let tokens_path = root.join("packages/theme/tokens.css");
        let tokens = std::fs::read_to_string(&tokens_path)
            .unwrap()
            .replace("--card: 0 0% 100%", "--card: 210 50% 20%")
            .replace("--border: 220 13% 91%", "--border: 0 100% 50%");
        std::fs::write(&tokens_path, tokens).unwrap();
        let changed_theme =
            plan_json(&create_vectors_from_react(&state, source_args(&root, true)).await);
        assert_eq!(changed_theme["theme"]["card"], "#1a334d");
        assert_eq!(changed_theme["theme"]["border"], "#ff0000");
        assert_ne!(
            changed_card["source_fingerprint"],
            changed_theme["source_fingerprint"]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn missing_or_outside_style_sources_reject_without_mutation() {
        let root = copied_root();
        let state = source_test_state();
        let tokens = root.join("packages/theme/tokens.css");
        std::fs::remove_file(&tokens).unwrap();
        let before = state.document.lock().await.nodes.len();
        let undo = state.history.lock().await.undo_depth();
        let missing = create_vectors_from_react(&state, source_args(&root, false)).await;
        assert_eq!(missing.is_error, Some(true));
        assert_eq!(state.document.lock().await.nodes.len(), before);
        assert_eq!(state.history.lock().await.undo_depth(), undo);

        std::fs::write(
            &tokens,
            ":root { --card: 0 0% 100%; --foreground: 0 0% 0%; --muted-foreground: 0 0% 40%; --border: 0 0% 90%; }",
        )
        .unwrap();
        let card = root.join("packages/ui/src/card.jsx");
        std::fs::remove_file(&card).unwrap();
        let outside = std::env::temp_dir().join(format!(
            "photonic-card-outside-{}.jsx",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&outside, "outside").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &card).unwrap();
        let rejected = create_vectors_from_react(&state, source_args(&root, false)).await;
        assert_eq!(rejected.is_error, Some(true));
        assert_eq!(state.document.lock().await.nodes.len(), before);
        assert_eq!(state.history.lock().await.undo_depth(), undo);
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_file(outside);
    }

    #[tokio::test]
    async fn copied_root_source_import_is_metamorphic_and_rejection_does_not_mutate() {
        let root = copied_root();
        let state = source_test_state();
        let baseline = create_vectors_from_react(&state, source_args(&root, true)).await;
        let baseline_json = plan_json(&baseline);
        assert_eq!(baseline_json["layout"]["gap_px"], 16.0, "{baseline_json}");
        assert!(baseline_json
            .to_string()
            .contains("https://people.bgcharlem.org/one-icon.svg"));
        let app_path = root.join("apps/hub/src/components/AppDirectory.jsx");
        let app = std::fs::read_to_string(&app_path)
            .unwrap()
            .replace("gap-4", "gap-8");
        std::fs::write(&app_path, app).unwrap();
        let gap_result = create_vectors_from_react(&state, source_args(&root, true)).await;
        assert_eq!(plan_json(&gap_result)["layout"]["gap_px"], 32.0);
        let cat = root.join("packages/waffle/src/suiteApps.js");
        let content = std::fs::read_to_string(&cat)
            .unwrap()
            .replace("/one-icon.svg", "/two-icon.svg");
        std::fs::write(&cat, content).unwrap();
        let icon_result = create_vectors_from_react(&state, source_args(&root, true)).await;
        let icon_plan = plan_json(&icon_result);
        assert!(icon_plan
            .to_string()
            .contains("https://people.bgcharlem.org/two-icon.svg"));
        assert!(!icon_plan["resolved_files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path.as_str().unwrap().ends_with("one-icon.svg")));
        let bad = std::fs::read_to_string(&app_path)
            .unwrap()
            .replace("<section>", "<section onClick={recordClick}>");
        std::fs::write(&app_path, bad).unwrap();
        let before = state.document.lock().await.nodes.len();
        let undo = state.history.lock().await.undo_depth();
        let rejected = create_vectors_from_react(&state, source_args(&root, false)).await;
        assert_eq!(rejected.is_error, Some(true));
        assert_eq!(state.document.lock().await.nodes.len(), before);
        assert_eq!(state.history.lock().await.undo_depth(), undo);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn local_svg_assets_become_editable_geometry_without_badges() {
        let root = copied_root();
        let state = source_test_state();
        let result = create_vectors_from_react(&state, source_args(&root, false)).await;
        assert_ne!(result.is_error, Some(true));
        let doc = state.document.lock().await;
        assert!(doc.nodes.values().any(|node| {
            node.name == "App icon: ONE" && matches!(node.kind, SceneNodeKind::Group(_))
        }));
        assert!(
            doc.nodes
                .values()
                .filter(|node| matches!(node.kind, SceneNodeKind::Path(_)))
                .count()
                >= 4
        );
        assert!(!doc.nodes.values().any(|node| node.name == "App badge"));
        let card = doc
            .nodes
            .values()
            .find(|node| node.name == "Card surface")
            .expect("card surface path");
        let SceneNodeKind::Path(card) = &card.kind else {
            panic!("card surface is not editable path geometry")
        };
        assert!(card.stroke.enabled);
        assert_eq!(card.stroke.width, 1.0);
        assert_eq!(card.stroke.color, Color::from_hex("#e5e7eb").unwrap());
        assert_all_icon_leaves_fit(&doc);
        let rendered_doc = doc.clone();
        drop(doc);
        let renderer = photonic_render::HeadlessRenderer::new().await;
        let png = renderer.render_png_at_size(&rendered_doc, 560, 360);
        let non_white = chromatic_pixel_count(&png);
        assert!(
            non_white > 100,
            "imported page rendered blank ({non_white} chromatic content pixels)"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn application_root_requires_a_static_component_branch_without_mutation() {
        let root = copied_root();
        let app_root = root.join("apps/hub/src/App.jsx");
        std::fs::write(
            &app_root,
            "import { useAuth } from './auth';\nexport function App() { const auth = useAuth(); return <main>{auth.user}</main>; }",
        )
        .unwrap();
        let mut args = source_args(&root, false);
        args.source_path = Some(app_root.display().to_string());
        args.export_name = Some("App".into());
        let state = source_test_state();
        let result = create_vectors_from_react(&state, args).await;
        assert_eq!(result.is_error, Some(true));
        let plan = plan_json(&result);
        let diagnostic = &plan["diagnostics"][0];
        assert_eq!(diagnostic["code"], "STATIC_BRANCH_REQUIRED");
        assert_eq!(diagnostic["source_path"], app_root.display().to_string());
        assert!(diagnostic["span"]["byte_end"].as_u64().unwrap_or(0) > 0);
        assert_eq!(state.document.lock().await.nodes.len(), 0);
        assert_eq!(state.history.lock().await.undo_depth(), 0);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn legacy_inline_snapshot_input_is_a_structured_preflight_error() {
        let state = source_test_state();
        let mut args = source_args(std::path::Path::new("/tmp"), false);
        args.source_path = None;
        args.source = Some("export function AppDirectory() { return null; }".into());
        args.snapshot = Some(crate::protocol::ReactSnapshotArg {
            template: "bgch-hub-app-directory-v1".into(),
            tiles: vec![],
        });
        let result = create_vectors_from_react(&state, args).await;
        assert_eq!(result.is_error, Some(true));
        let plan = plan_json(&result);
        assert_eq!(plan["diagnostics"][0]["code"], "SOURCE_PATH_REQUIRED");
        assert_eq!(state.document.lock().await.nodes.len(), 0);
        assert_eq!(state.history.lock().await.undo_depth(), 0);
    }

    #[tokio::test]
    async fn canonical_bgch_icon_leaves_fit_their_tiles_when_fixture_is_available() {
        let Ok(root) = std::env::var("PHOTONIC_BGCH_ACCEPTANCE_ROOT") else {
            return;
        };
        let root = std::path::PathBuf::from(root);
        let state = source_test_state();
        let result = create_vectors_from_react(&state, source_args(&root, false)).await;
        assert_ne!(result.is_error, Some(true), "{result:?}");
        let doc = state.document.lock().await.clone();
        assert_eq!(
            doc.nodes
                .values()
                .filter(|node| node.name.starts_with("App icon: "))
                .count(),
            7
        );
        assert_all_icon_leaves_fit(&doc);
        let renderer = photonic_render::HeadlessRenderer::new().await;
        let png = renderer.render_png_at_size(&doc, 1120, 720);
        let chromatic = chromatic_pixel_count(&png);
        assert!(
            chromatic > 500,
            "canonical AppDirectory rendered blank ({chromatic} chromatic content pixels)"
        );
        std::fs::write("/tmp/photonic-252-canonical-source-render.png", png).unwrap();
    }

    #[tokio::test]
    async fn missing_local_svg_rejects_with_zero_mutation() {
        let root = copied_root();
        let catalog = root.join("packages/waffle/src/suiteApps.js");
        let changed = std::fs::read_to_string(&catalog)
            .unwrap()
            .replace("/one-icon.svg", "/missing-icon.svg");
        std::fs::write(&catalog, changed).unwrap();
        let state = source_test_state();
        let before_nodes = state.document.lock().await.nodes.len();
        let before_undo = state.history.lock().await.undo_depth();
        let result = create_vectors_from_react(&state, source_args(&root, false)).await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(state.document.lock().await.nodes.len(), before_nodes);
        assert_eq!(state.history.lock().await.undo_depth(), before_undo);
        let debug = format!("{result:?}");
        assert!(debug.contains("ASSET_NOT_FOUND"), "{debug}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn connect_page_card_height_contains_all_accepted_social_rows() {
        let final_row_bottom = 116.0 + 7.0 * 70.0 + 54.0;
        let card_height = connect_page_card_height(768.0, 8);
        assert!(
            card_height >= final_row_bottom + 24.0,
            "eight-link layout exceeds card: height={card_height}, required={}",
            final_row_bottom + 24.0
        );
        assert_eq!(connect_page_card_height(768.0, 4), 532.0);
    }

    #[test]
    fn connect_page_canvas_contains_the_tallest_accepted_card() {
        let card_height = connect_page_card_height(768.0, 8);
        let canvas_height = connect_page_canvas_height(768.0, card_height);

        assert_eq!(canvas_height, 920.0);
        assert!(188.0 + card_height <= canvas_height - 48.0);
        assert_eq!(
            connect_page_canvas_height(768.0, connect_page_card_height(768.0, 4)),
            768.0
        );
    }

    fn connect_page_test_asset() -> LucideAsset {
        let mut document = Document::new("test icon", 24.0, 24.0);
        let layer = document.active_layer_id.unwrap();
        let path = PathNode::new(PathData::from_svg("M 0 0 L 24 24").unwrap());
        document.add_node(
            SceneNode::new("test icon path", layer, SceneNodeKind::Path(path)),
            Some(layer),
        );
        LucideAsset {
            name: "Instagram".into(),
            source_path: PathBuf::from("test-icon.svg"),
            document,
        }
    }

    fn eight_link_connect_page() -> ConnectPageSnapshot {
        let asset = connect_page_test_asset();
        ConnectPageSnapshot {
            title: "Connect".into(),
            subtitle: "Stay in touch".into(),
            mailing_title: "Stay in the Loop".into(),
            mailing_body: "Newsletter".into(),
            follow_title: "Follow along".into(),
            follow_body: "Social links".into(),
            links: (0..8)
                .map(|index| ImportedSocialLink {
                    href: format!("https://example.com/{index}"),
                    label: format!("Link {index}"),
                    handle: format!("@link{index}"),
                    icon: "Instagram".into(),
                    asset: Some(asset.clone()),
                })
                .collect(),
            resolved_files: vec![],
            fingerprint: "test".into(),
        }
    }

    fn connect_page_args() -> CreateVectorsFromReactArgs {
        CreateVectorsFromReactArgs {
            jsx: None,
            source: None,
            snapshot: None,
            source_path: None,
            export_name: Some("ConnectPage".into()),
            props: None,
            module_roots: vec![],
            theme_tokens: None,
            interaction_policy: None,
            dynamic_content: None,
            origin: None,
            viewport: Some(CssViewportArg {
                width: 1024.0,
                height: 768.0,
            }),
            layer_id: None,
            group_name: None,
            strict: true,
            dry_run: false,
        }
    }

    #[tokio::test]
    async fn eight_link_connect_page_expands_export_bounds_with_the_card() {
        let state = source_test_state();
        let result =
            create_connect_nodes(&state, &connect_page_args(), &eight_link_connect_page()).await;
        assert_ne!(result.is_error, Some(true), "{result:?}");

        let doc = state.document.lock().await;
        assert_eq!(doc.height, 920.0);
        let artboard = doc.active_artboard().unwrap();
        assert_eq!(artboard.height, 920.0);
        assert_eq!(
            188.0 + connect_page_card_height(768.0, 8),
            artboard.height - 48.0
        );
        drop(doc);

        let mut doc = state.document.lock().await;
        let mut history = state.history.lock().await;
        assert_eq!(history.undo_depth(), 1);
        assert!(history.undo(&mut doc));
        assert_eq!(doc.height, 720.0);
        assert_eq!(doc.active_artboard().unwrap().height, 720.0);
        assert!(doc.nodes.is_empty());
    }
}
