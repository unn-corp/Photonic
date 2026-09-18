use super::*;

/// A 32×32 checker tile used as the default pattern-fill tile.
pub(crate) fn default_checker_tile() -> photonic_core::RasterImage {
    let n = 32u32; // tile is 2×2 cells of 16px
    let cell = 16u32;
    let mut img = photonic_core::RasterImage::new(n, n);
    for y in 0..n {
        for x in 0..n {
            let on = ((x / cell) + (y / cell)).is_multiple_of(2);
            let rgba = if on {
                [40, 40, 48, 255]
            } else {
                [220, 220, 230, 255]
            };
            img.set_pixel(x, y, rgba);
        }
    }
    img
}

/// Draw a compact stroke editor. Returns `Some(new_stroke)` if the user changed anything.
/// Sets `*dropper` to `true` when the eyedropper button is clicked.
pub(crate) fn draw_stroke_editor(
    ui: &mut Ui,
    stroke: &Stroke,
    dropper: &mut bool,
) -> Option<Stroke> {
    use photonic_core::style::{LineCap, LineJoin, StrokeAlign};

    let mut new_stroke = stroke.clone();
    let mut changed = false;

    // Enable / disable toggle
    let mut enabled = new_stroke.enabled;
    if ui.checkbox(&mut enabled, "Enabled").changed() {
        new_stroke.enabled = enabled;
        changed = true;
    }

    if new_stroke.enabled {
        // Color — gamma-sRGB `Color` → shared sRGBA picker (issue #185).
        ui.horizontal(|ui| {
            if ColorPopup::swatch_color(ui, &mut new_stroke.color).changed() {
                changed = true;
            }
            if eyedropper_btn(ui) {
                *dropper = true;
            }
        });

        // Width
        ui.horizontal(|ui| {
            ui.label("Width");
            let mut w = new_stroke.width as f32;
            if ui
                .add(egui::DragValue::new(&mut w).range(0.0..=500.0).speed(0.5))
                .changed()
            {
                new_stroke.width = w as f64;
                changed = true;
            }
        });

        // Opacity
        ui.horizontal(|ui| {
            ui.label("Opacity");
            let mut op = new_stroke.opacity;
            if ui.add(egui::Slider::new(&mut op, 0.0..=1.0)).changed() {
                new_stroke.opacity = op;
                changed = true;
            }
        });

        // Line cap
        ui.horizontal(|ui| {
            ui.label("Cap");
            for (label, cap) in [
                ("Butt", LineCap::Butt),
                ("Round", LineCap::Round),
                ("Square", LineCap::Square),
            ] {
                if ui
                    .selectable_label(new_stroke.line_cap == cap, label)
                    .clicked()
                {
                    new_stroke.line_cap = cap;
                    changed = true;
                }
            }
        });

        // Line join
        ui.horizontal(|ui| {
            ui.label("Join");
            for (label, join) in [
                ("Miter", LineJoin::Miter),
                ("Round", LineJoin::Round),
                ("Bevel", LineJoin::Bevel),
            ] {
                if ui
                    .selectable_label(new_stroke.line_join == join, label)
                    .clicked()
                {
                    new_stroke.line_join = join;
                    changed = true;
                }
            }
        });

        // Stroke alignment
        ui.horizontal(|ui| {
            ui.label("Align");
            for (label, align) in [
                ("Center", StrokeAlign::Center),
                ("Inside", StrokeAlign::Inside),
                ("Outside", StrokeAlign::Outside),
            ] {
                if ui
                    .selectable_label(new_stroke.align == align, label)
                    .clicked()
                {
                    new_stroke.align = align;
                    changed = true;
                }
            }
        });

        // Dash controls
        let mut dashes_on = !new_stroke.dash_array.is_empty();
        if ui.checkbox(&mut dashes_on, "Dashed").changed() {
            if dashes_on {
                new_stroke.dash_array = vec![8.0, 4.0];
            } else {
                new_stroke.dash_array.clear();
            }
            changed = true;
        }
        if dashes_on {
            // Ensure pairs up to 3 (6 values); pad if needed for the UI.
            while new_stroke.dash_array.len() < 2 {
                new_stroke.dash_array.push(4.0);
            }
            ui.label(RichText::new("Dash / Gap pairs (up to 3):").weak().small());
            let pair_count = (new_stroke.dash_array.len() / 2).max(1).min(3);
            for i in 0..pair_count {
                let dash_idx = i * 2;
                let gap_idx = i * 2 + 1;
                ui.horizontal(|ui| {
                    ui.label(format!("Pair {}:", i + 1));
                    let mut dash_val =
                        new_stroke.dash_array.get(dash_idx).copied().unwrap_or(8.0) as f32;
                    let mut gap_val =
                        new_stroke.dash_array.get(gap_idx).copied().unwrap_or(4.0) as f32;
                    if ui
                        .add(
                            egui::DragValue::new(&mut dash_val)
                                .range(0.5..=500.0)
                                .speed(0.5)
                                .prefix("—"),
                        )
                        .changed()
                    {
                        if new_stroke.dash_array.len() <= dash_idx {
                            new_stroke.dash_array.resize(dash_idx + 1, 0.0);
                        }
                        new_stroke.dash_array[dash_idx] = dash_val as f64;
                        changed = true;
                    }
                    ui.label("·");
                    if ui
                        .add(
                            egui::DragValue::new(&mut gap_val)
                                .range(0.0..=500.0)
                                .speed(0.5),
                        )
                        .changed()
                    {
                        if new_stroke.dash_array.len() <= gap_idx {
                            new_stroke.dash_array.resize(gap_idx + 1, 0.0);
                        }
                        new_stroke.dash_array[gap_idx] = gap_val as f64;
                        changed = true;
                    }
                });
            }
            // Add/remove pair buttons
            ui.horizontal(|ui| {
                if pair_count < 3
                    && ui
                        .small_button("+ Pair")
                        .on_hover_text("Add a dash/gap pair")
                        .clicked()
                {
                    new_stroke.dash_array.extend_from_slice(&[8.0, 4.0]);
                    changed = true;
                }
                if pair_count > 1
                    && ui
                        .small_button("− Pair")
                        .on_hover_text("Remove the last dash/gap pair")
                        .clicked()
                {
                    new_stroke
                        .dash_array
                        .truncate(new_stroke.dash_array.len().saturating_sub(2));
                    changed = true;
                }
            });
            // Dash offset
            ui.horizontal(|ui| {
                ui.label("Offset:");
                let mut offset = new_stroke.dash_offset as f32;
                if ui
                    .add(egui::DragValue::new(&mut offset).speed(0.5))
                    .changed()
                {
                    new_stroke.dash_offset = offset as f64;
                    changed = true;
                }
            });
            // Align dashes to corners
            let mut align_corners = new_stroke.dash_corner_alignment;
            if ui.checkbox(&mut align_corners, "Align to corners")
                .on_hover_text("Adjust dash spacing so dashes start and end cleanly at path corners and endpoints")
                .changed()
            {
                new_stroke.dash_corner_alignment = align_corners;
                changed = true;
            }
        }
    }

    // ── Arrowheads ──────────────────────────────────────────────────────────
    {
        use photonic_core::style::ArrowheadStyle;
        let arrow_label = |s: ArrowheadStyle| match s {
            ArrowheadStyle::None => "None",
            ArrowheadStyle::FilledArrow => "Filled",
            ArrowheadStyle::OpenArrow => "Open",
        };
        ui.horizontal(|ui| {
            ui.label("Arrow start");
            egui::ComboBox::new("arrow_start", "")
                .selected_text(arrow_label(new_stroke.arrowhead_start))
                .show_ui(ui, |ui| {
                    for style in [
                        ArrowheadStyle::None,
                        ArrowheadStyle::FilledArrow,
                        ArrowheadStyle::OpenArrow,
                    ] {
                        if ui
                            .selectable_label(
                                new_stroke.arrowhead_start == style,
                                arrow_label(style),
                            )
                            .clicked()
                        {
                            new_stroke.arrowhead_start = style;
                            changed = true;
                        }
                    }
                });
        });
        ui.horizontal(|ui| {
            ui.label("Arrow end");
            egui::ComboBox::new("arrow_end", "")
                .selected_text(arrow_label(new_stroke.arrowhead_end))
                .show_ui(ui, |ui| {
                    for style in [
                        ArrowheadStyle::None,
                        ArrowheadStyle::FilledArrow,
                        ArrowheadStyle::OpenArrow,
                    ] {
                        if ui
                            .selectable_label(new_stroke.arrowhead_end == style, arrow_label(style))
                            .clicked()
                        {
                            new_stroke.arrowhead_end = style;
                            changed = true;
                        }
                    }
                });
        });
    }

    if changed {
        Some(new_stroke)
    } else {
        None
    }
}

/// Renders a compact editor for a `GlowEffect`. Returns `Some(updated)` on any change.
/// Sets `*dropper` to `true` when the eyedropper button is clicked.
pub(crate) fn draw_glow_editor(
    ui: &mut Ui,
    glow: &GlowEffect,
    dropper: &mut bool,
) -> Option<GlowEffect> {
    let mut new_glow = glow.clone();
    let mut changed = false;

    ui.horizontal(|ui| {
        if ui.checkbox(&mut new_glow.enabled, "Enabled").changed() {
            changed = true;
        }
    });

    if new_glow.enabled {
        ui.horizontal(|ui| {
            ui.label("Color");
            // Gamma-sRGB `Color` → shared sRGBA picker (issue #185).
            if ColorPopup::swatch_color(ui, &mut new_glow.color).changed() {
                changed = true;
            }
            if eyedropper_btn(ui) {
                *dropper = true;
            }
        });
        ui.horizontal(|ui| {
            ui.label("Opacity");
            if ui
                .add(egui::Slider::new(&mut new_glow.opacity, 0.0..=1.0))
                .changed()
            {
                changed = true;
            }
        });
        ui.horizontal(|ui| {
            ui.label("Size");
            if ui
                .add(egui::Slider::new(&mut new_glow.size, 1.0..=100.0).suffix("px"))
                .changed()
            {
                changed = true;
            }
        });
        ui.horizontal(|ui| {
            ui.label("Corners");
            let options = [
                (LineJoin::Miter, "Miter"),
                (LineJoin::Round, "Round"),
                (LineJoin::Bevel, "Bevel"),
            ];
            for (variant, label) in options {
                if ui
                    .selectable_label(new_glow.join == variant, label)
                    .clicked()
                {
                    new_glow.join = variant;
                    changed = true;
                }
            }
        });
    }

    if changed {
        Some(new_glow)
    } else {
        None
    }
}

/// Renders a compact editor for a `GaussianGlow`. Returns `Some(updated)` on any change.
/// Sets `*dropper` to `true` when the eyedropper button is clicked.
pub(crate) fn draw_gaussian_glow_editor(
    ui: &mut Ui,
    glow: &GaussianGlow,
    dropper: &mut bool,
) -> Option<GaussianGlow> {
    let mut new_glow = glow.clone();
    let mut changed = false;

    ui.horizontal(|ui| {
        if ui.checkbox(&mut new_glow.enabled, "Enabled").changed() {
            changed = true;
        }
    });

    if new_glow.enabled {
        ui.horizontal(|ui| {
            ui.label("Color");
            // Gamma-sRGB `Color` → shared sRGBA picker (issue #185).
            if ColorPopup::swatch_color(ui, &mut new_glow.color).changed() {
                changed = true;
            }
            if eyedropper_btn(ui) {
                *dropper = true;
            }
        });
        ui.horizontal(|ui| {
            ui.label("Opacity");
            if ui
                .add(egui::Slider::new(&mut new_glow.opacity, 0.0..=1.0))
                .changed()
            {
                changed = true;
            }
        });
        ui.horizontal(|ui| {
            ui.label("Radius");
            if ui
                .add(egui::Slider::new(&mut new_glow.radius, 1.0..=200.0).suffix("px"))
                .changed()
            {
                changed = true;
            }
        });
    }

    if changed {
        Some(new_glow)
    } else {
        None
    }
}
