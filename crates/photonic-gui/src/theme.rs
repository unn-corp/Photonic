use egui::{style::WidgetVisuals, Color32, Rounding, Shadow, Stroke};

/// Build the "Deep Violet" dark theme.
pub fn build_dark_theme() -> egui::Visuals {
    let bg_base = Color32::from_rgb(7, 7, 11); // #07070B window fill
    let bg_panel = Color32::from_rgb(12, 12, 21); // #0C0C15 panel fill
    let bg_elevated = Color32::from_rgb(19, 19, 31); // #13131F hover / input bg
    let bg_widget = Color32::from_rgb(26, 26, 40); // #1A1A28 text inputs
    let border = Color32::from_rgb(30, 30, 50); // #1E1E32 decorative panel/card chrome
    let border_interactive = Color32::from_rgb(102, 102, 144); // #666690 operable-control boundary (SC 1.4.11)
    let border_focus = Color32::from_rgb(110, 86, 207); // #6E56CF focused border
    let accent = Color32::from_rgb(110, 86, 207); // #6E56CF electric violet
    let accent_dim = Color32::from_rgb(61, 48, 128); // #3D3080 accent at ~40%
    let accent_light = Color32::from_rgb(144, 119, 224); // #9077E0 hover / glow
    let text_primary = Color32::from_rgb(232, 232, 242); // #E8E8F2
    let text_muted = Color32::from_rgb(138, 138, 168); // #8A8AA8 secondary labels (WCAG AA)

    let _ = accent_light;

    let rounding = Rounding::same(3.0);
    let mut v = egui::Visuals::dark();

    v.window_fill = bg_base;
    v.panel_fill = bg_panel;
    v.faint_bg_color = bg_elevated;
    v.extreme_bg_color = bg_widget;
    v.code_bg_color = bg_elevated;

    v.override_text_color = Some(text_primary);

    v.window_rounding = Rounding::same(4.0);
    v.window_stroke = Stroke::new(1.0, border);
    v.window_shadow = Shadow::NONE;
    v.popup_shadow = Shadow::NONE;
    v.menu_rounding = Rounding::same(4.0);

    v.selection.bg_fill = accent_dim;
    v.selection.stroke = Stroke::new(1.0, accent);

    v.hyperlink_color = Color32::from_rgb(144, 119, 224);
    v.warn_fg_color = Color32::from_rgb(251, 191, 36);
    v.error_fg_color = Color32::from_rgb(248, 113, 113);

    // `noninteractive` keeps the decorative `border` (panel/card chrome, exempt);
    // `inactive` — the boundary of every unfocused *operable* widget — uses the
    // stronger `border_interactive` to clear WCAG SC 1.4.11 (41 §5.1 R-12).
    v.widgets.noninteractive = WidgetVisuals {
        bg_fill: bg_panel,
        weak_bg_fill: bg_elevated,
        bg_stroke: Stroke::new(1.0, border),
        rounding,
        fg_stroke: Stroke::new(1.0, text_muted),
        expansion: 0.0,
    };
    v.widgets.inactive = WidgetVisuals {
        bg_fill: bg_elevated,
        weak_bg_fill: bg_panel,
        bg_stroke: Stroke::new(1.0, border_interactive),
        rounding,
        fg_stroke: Stroke::new(1.0, text_primary),
        expansion: 0.0,
    };
    v.widgets.hovered = WidgetVisuals {
        bg_fill: bg_widget,
        weak_bg_fill: bg_elevated,
        bg_stroke: Stroke::new(1.0, border_focus),
        rounding,
        fg_stroke: Stroke::new(1.5, text_primary),
        expansion: 1.0,
    };
    v.widgets.active = WidgetVisuals {
        bg_fill: accent_dim,
        weak_bg_fill: bg_elevated,
        bg_stroke: Stroke::new(1.0, accent),
        rounding,
        fg_stroke: Stroke::new(2.0, Color32::WHITE),
        expansion: 1.0,
    };
    v.widgets.open = WidgetVisuals {
        bg_fill: bg_elevated,
        weak_bg_fill: bg_panel,
        bg_stroke: Stroke::new(1.0, border),
        rounding,
        fg_stroke: Stroke::new(1.5, text_primary),
        expansion: 0.0,
    };

    v
}

/// The `secondary` token colour for the current theme — `noninteractive`
/// foreground, i.e. `text_muted` (#8A8AA8 dark / #6E6496 light). Section headings
/// and other muted labels take their recession from *this* AA-passing tone plus
/// the smaller `RichText::small()` size, not from a dimmer below-AA grey.
pub fn section_header_color(ui: &egui::Ui) -> Color32 {
    ui.visuals().widgets.noninteractive.fg_stroke.color
}

/// A small, muted small-caps section heading with the app's standard 4px-above /
/// 2px-below spacing (DESIGN.md `components.panel-section-header`). The single
/// definition every drawer/section heading shares, so the header tone can't drift
/// per-panel the way the old hard-coded `#50506E` did across 13 call sites.
pub fn section_header(ui: &mut egui::Ui, text: &str) {
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(text)
            .small()
            .color(section_header_color(ui)),
    );
    ui.add_space(2.0);
}

/// Apply the app's spacing overrides to a [`egui::Style`].
///
/// Called once at startup (`ctx.style_mut`); [`egui::Context::set_visuals`]
/// replaces only `Style::visuals`, so these persist across theme switches. The
/// `interact_size` height is held at 24px for the WCAG 2.2 SC 2.5.8 target-size
/// floor (41 §5 R-9) — egui 0.29's default is 40×18.
pub fn apply_spacing(style: &mut egui::Style) {
    style.spacing.item_spacing = egui::vec2(6.0, 4.0);
    style.spacing.button_padding = egui::vec2(8.0, 3.0);
    style.spacing.interact_size = egui::vec2(40.0, 24.0);
}

/// Build the "Soft Lavender" light theme — much lighter purple companion.
pub fn build_light_theme() -> egui::Visuals {
    let bg_base = Color32::from_rgb(250, 249, 255); // #FAF9FF near-white with violet tint
    let bg_panel = Color32::from_rgb(243, 240, 255); // #F3F0FF soft lavender panels
    let bg_elevated = Color32::from_rgb(234, 228, 255); // #EAE4FF elevated surfaces
    let bg_widget = Color32::from_rgb(255, 255, 255); // #FFFFFF inputs
    let border = Color32::from_rgb(210, 200, 240); // #D2C8F0 soft violet border
    let border_focus = Color32::from_rgb(110, 86, 207); // #6E56CF accent border (same violet)
    let accent = Color32::from_rgb(110, 86, 207); // #6E56CF electric violet
    let accent_dim = Color32::from_rgb(210, 198, 245); // #D2C6F5 light selection fill
    let text_primary = Color32::from_rgb(25, 20, 60); // #19143C near-black with violet
    let text_muted = Color32::from_rgb(110, 100, 150); // #6E6496 muted labels

    let rounding = Rounding::same(3.0);
    let mut v = egui::Visuals::light();

    v.window_fill = bg_base;
    v.panel_fill = bg_panel;
    v.faint_bg_color = bg_elevated;
    v.extreme_bg_color = bg_widget;
    v.code_bg_color = bg_elevated;

    v.override_text_color = Some(text_primary);

    v.window_rounding = Rounding::same(4.0);
    v.window_stroke = Stroke::new(1.0, border);
    v.window_shadow = Shadow::NONE;
    v.popup_shadow = Shadow::NONE;
    v.menu_rounding = Rounding::same(4.0);

    v.selection.bg_fill = accent_dim;
    v.selection.stroke = Stroke::new(1.0, accent);

    v.hyperlink_color = Color32::from_rgb(110, 86, 207);
    v.warn_fg_color = Color32::from_rgb(143, 94, 0); // #8F5E00 — 4.97:1 on light-surface (WCAG AA)
    v.error_fg_color = Color32::from_rgb(176, 45, 45); // #B02D2D — 5.74:1 on light-surface (WCAG AA)

    v.widgets.noninteractive = WidgetVisuals {
        bg_fill: bg_panel,
        weak_bg_fill: bg_elevated,
        bg_stroke: Stroke::new(1.0, border),
        rounding,
        fg_stroke: Stroke::new(1.0, text_muted),
        expansion: 0.0,
    };
    v.widgets.inactive = WidgetVisuals {
        bg_fill: bg_elevated,
        weak_bg_fill: bg_panel,
        bg_stroke: Stroke::new(1.0, border),
        rounding,
        fg_stroke: Stroke::new(1.0, text_primary),
        expansion: 0.0,
    };
    v.widgets.hovered = WidgetVisuals {
        bg_fill: bg_widget,
        weak_bg_fill: bg_elevated,
        bg_stroke: Stroke::new(1.0, border_focus),
        rounding,
        fg_stroke: Stroke::new(1.5, text_primary),
        expansion: 1.0,
    };
    v.widgets.active = WidgetVisuals {
        bg_fill: accent_dim,
        weak_bg_fill: bg_elevated,
        bg_stroke: Stroke::new(1.0, accent),
        rounding,
        fg_stroke: Stroke::new(2.0, Color32::from_rgb(25, 20, 60)),
        expansion: 1.0,
    };
    v.widgets.open = WidgetVisuals {
        bg_fill: bg_elevated,
        weak_bg_fill: bg_panel,
        bg_stroke: Stroke::new(1.0, border),
        rounding,
        fg_stroke: Stroke::new(1.5, text_primary),
        expansion: 0.0,
    };

    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Minimal `colors:` frontmatter parser (mirrors
    /// `tests/design_contrast.rs`; the crate can't import an integration test).
    fn design_colors() -> BTreeMap<String, [u8; 3]> {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../DESIGN.md")
            .canonicalize()
            .expect("resolve DESIGN.md");
        let src = std::fs::read_to_string(&path).expect("read DESIGN.md");
        let mut out = BTreeMap::new();
        let mut in_colors = false;
        for line in src.lines() {
            if line.starts_with("colors:") {
                in_colors = true;
                continue;
            }
            if !in_colors {
                continue;
            }
            // A frontmatter fence or an unindented key ends the block.
            if line == "---" || (!line.is_empty() && !line.starts_with(' ')) {
                break;
            }
            let t = line.trim_start();
            if t.is_empty() || t.starts_with('#') {
                continue;
            }
            let Some((key, rest)) = t.split_once(':') else {
                continue;
            };
            let Some(h) = rest.find('#') else { continue };
            let hex = &rest[h + 1..];
            if hex.len() < 6 || !hex[..6].bytes().all(|b| b.is_ascii_hexdigit()) {
                continue;
            }
            out.insert(
                key.trim().to_string(),
                [
                    u8::from_str_radix(&hex[0..2], 16).unwrap(),
                    u8::from_str_radix(&hex[2..4], 16).unwrap(),
                    u8::from_str_radix(&hex[4..6], 16).unwrap(),
                ],
            );
        }
        out
    }

    fn rgb(c: Color32) -> [u8; 3] {
        [c.r(), c.g(), c.b()]
    }

    /// Drift gate: the compiled themes must reproduce the DESIGN.md tokens exactly.
    /// This is what stops `theme.rs` and `DESIGN.md` diverging again (41 §5).
    #[test]
    fn theme_matches_design_md() {
        let c = design_colors();
        let tok = |name: &str| {
            *c.get(name)
                .unwrap_or_else(|| panic!("missing token {name}"))
        };

        let dark = build_dark_theme();
        assert_eq!(rgb(dark.override_text_color.unwrap()), tok("on-surface"));
        assert_eq!(
            rgb(dark.widgets.noninteractive.fg_stroke.color),
            tok("secondary")
        );
        assert_eq!(
            rgb(dark.widgets.noninteractive.bg_stroke.color),
            tok("border")
        );
        assert_eq!(
            rgb(dark.widgets.inactive.bg_stroke.color),
            tok("border-interactive")
        );
        assert_eq!(rgb(dark.error_fg_color), tok("error"));
        assert_eq!(rgb(dark.warn_fg_color), tok("warning"));

        let light = build_light_theme();
        assert_eq!(
            rgb(light.override_text_color.unwrap()),
            tok("light-on-surface")
        );
        assert_eq!(
            rgb(light.widgets.noninteractive.fg_stroke.color),
            tok("light-secondary")
        );
        assert_eq!(
            rgb(light.widgets.noninteractive.bg_stroke.color),
            tok("light-border")
        );
        assert_eq!(rgb(light.error_fg_color), tok("light-error"));
        assert_eq!(rgb(light.warn_fg_color), tok("light-warning"));
    }

    /// 41 §5 R-9: the interactive hit-target height must clear the 24px WCAG 2.2
    /// SC 2.5.8 floor after `apply_spacing`.
    #[test]
    fn interact_size_clears_wcag_floor() {
        let mut style = egui::Style::default();
        apply_spacing(&mut style);
        assert!(style.spacing.interact_size.y >= 24.0);
        assert!(style.spacing.interact_size.x >= 24.0);
    }
}
