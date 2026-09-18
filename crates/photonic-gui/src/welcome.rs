//! Welcome / opening flow — a guided, cinematic landing screen.
//!
//! Rather than a static two-column form, the opening is a small state machine:
//!
//!   Hub  ──▶  New Canvas   (blank-document setup, aspect-ratio tiles + preview)
//!    │
//!    └────▶  Open          (recent documents as live preview cards + browse)
//!
//! Entrance and panel transitions are animated off egui's frame clock
//! (`input.time` + `animate_bool_with_time`) — a subtle, premium feel: short
//! fades, small drifts, gentle easing, no overshoot. Recent documents render a
//! true preview thumbnail, generated off-thread by the pure-CPU compositor and
//! uploaded as an egui texture once ready (a stylized placeholder shows first).

use egui::{
    Align2, Color32, FontId, Margin, Mesh, Pos2, Rect, RichText, Rounding, Sense, Stroke,
    TextureHandle, TextureOptions, Vec2,
};
use egui_phosphor::regular as ph;
use photonic_core::{Document, PHOTON_FILE_EXTENSION};
use photonic_render::{canvas::CanvasView, compositor::composite_document};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};

const MAX_RECENT: usize = 8;

// ─── Palette (shared with the dark editor theme) ───────────────────────────────

const ACCENT: Color32 = Color32::from_rgb(110, 86, 207);
const ACCENT_BRIGHT: Color32 = Color32::from_rgb(150, 128, 240);
const ACCENT_DIM: Color32 = Color32::from_rgb(61, 48, 128);
const BG_BASE: Color32 = Color32::from_rgb(7, 7, 11);
const BG_PANEL: Color32 = Color32::from_rgb(12, 12, 21);
const BG_ELEVATED: Color32 = Color32::from_rgb(19, 19, 31);
const BG_WIDGET: Color32 = Color32::from_rgb(26, 26, 40);
const BORDER: Color32 = Color32::from_rgb(30, 30, 50);
const TEXT_PRIMARY: Color32 = Color32::from_rgb(232, 232, 242);
const TEXT_MUTED: Color32 = Color32::from_rgb(138, 138, 168); // #8A8AA8 secondary (WCAG AA)

// Canvas size presets, grouped by use-case: (group, &[(label, width, height)]).
// Dimensions are in pixels (paper sizes at 96 DPI, photo/poster noted per group).
// Each tile draws its true aspect ratio, so wide/tall/square presets are
// visually distinguishable. Filterable via the size search bar.
const PRESET_GROUPS: &[(&str, &[(&str, f64, f64)])] = &[
    (
        "Paper · A series (96dpi)",
        &[
            ("A6", 397.0, 559.0),
            ("A5", 559.0, 794.0),
            ("A4", 794.0, 1123.0),
            ("A4 Landscape", 1123.0, 794.0),
            ("A3", 1123.0, 1587.0),
            ("A2", 1587.0, 2245.0),
            ("A1", 2245.0, 3179.0),
            ("A0", 3179.0, 4494.0),
        ],
    ),
    (
        "Paper · US (96dpi)",
        &[
            ("Letter", 816.0, 1056.0),
            ("Letter Landscape", 1056.0, 816.0),
            ("Legal", 816.0, 1344.0),
            ("Tabloid", 1056.0, 1632.0),
            ("Ledger", 1632.0, 1056.0),
            ("Executive", 696.0, 1008.0),
            ("Statement", 528.0, 816.0),
            ("B5", 665.0, 944.0),
            ("B4", 944.0, 1334.0),
        ],
    ),
    (
        "Stationery",
        &[
            ("Business Card", 1050.0, 600.0),
            ("Postcard 4×6", 1200.0, 1800.0),
            ("Postcard 5×7", 1500.0, 2100.0),
            ("DL Flyer", 1039.0, 2079.0),
            ("Envelope #10", 2850.0, 1237.0),
            ("Letterhead", 2550.0, 3300.0),
            ("Greeting Card", 1500.0, 2100.0),
            ("Door Hanger", 1050.0, 2700.0),
            ("Ticket", 1650.0, 600.0),
            ("Bookmark", 600.0, 1800.0),
        ],
    ),
    (
        "Photo print (300dpi)",
        &[
            ("2×3", 600.0, 900.0),
            ("3.5×5", 1050.0, 1500.0),
            ("4×6", 1200.0, 1800.0),
            ("5×7", 1500.0, 2100.0),
            ("6×8", 1800.0, 2400.0),
            ("8×10", 2400.0, 3000.0),
            ("8.5×11", 2550.0, 3300.0),
            ("11×14", 3300.0, 4200.0),
            ("12×12", 3600.0, 3600.0),
            ("16×20", 4800.0, 6000.0),
            ("20×30", 6000.0, 9000.0),
            ("24×36", 7200.0, 10800.0),
        ],
    ),
    (
        "Poster & large (150dpi)",
        &[
            ("11×17", 1650.0, 2550.0),
            ("Tabloid Poster", 1800.0, 2700.0),
            ("18×24", 2700.0, 3600.0),
            ("24×36", 3600.0, 5400.0),
            ("Movie 27×40", 4050.0, 6000.0),
            ("A2 Poster", 2480.0, 3508.0),
            ("A1 Poster", 3508.0, 4961.0),
            ("Banner 2×6ft", 3600.0, 10800.0),
            ("Roll-up", 3838.0, 9921.0),
        ],
    ),
    (
        "Screen & video",
        &[
            ("480p", 854.0, 480.0),
            ("720p", 1280.0, 720.0),
            ("1080p", 1920.0, 1080.0),
            ("1440p QHD", 2560.0, 1440.0),
            ("4K UHD", 3840.0, 2160.0),
            ("5K", 5120.0, 2880.0),
            ("8K", 7680.0, 4320.0),
            ("DCI 4K", 4096.0, 2160.0),
            ("Vertical 1080", 1080.0, 1920.0),
            ("Ultrawide", 3440.0, 1440.0),
            ("Super Ultrawide", 5120.0, 1440.0),
            ("WXGA", 1366.0, 768.0),
            ("WUXGA", 1920.0, 1200.0),
            ("Cinemascope", 1920.0, 816.0),
        ],
    ),
    (
        "Web",
        &[
            ("Desktop 1280", 1280.0, 800.0),
            ("Desktop 1440", 1440.0, 900.0),
            ("Desktop 1920", 1920.0, 1080.0),
            ("Hero", 1600.0, 900.0),
            ("OG Image", 1200.0, 630.0),
            ("Blog Header", 1200.0, 630.0),
            ("Email", 600.0, 1200.0),
            ("Email Header", 600.0, 200.0),
            ("Card", 400.0, 500.0),
            ("Logo Space", 800.0, 400.0),
        ],
    ),
    (
        "Instagram",
        &[
            ("Post", 1080.0, 1080.0),
            ("Portrait", 1080.0, 1350.0),
            ("Landscape", 1080.0, 566.0),
            ("Story / Reel", 1080.0, 1920.0),
            ("Profile", 320.0, 320.0),
            ("Carousel", 1080.0, 1080.0),
        ],
    ),
    (
        "Facebook",
        &[
            ("Post", 1200.0, 630.0),
            ("Square Post", 1200.0, 1200.0),
            ("Cover", 820.0, 312.0),
            ("Story", 1080.0, 1920.0),
            ("Event", 1920.0, 1005.0),
            ("Profile", 320.0, 320.0),
            ("Ad", 1200.0, 628.0),
        ],
    ),
    (
        "X / Twitter",
        &[
            ("Post", 1600.0, 900.0),
            ("Square Post", 1080.0, 1080.0),
            ("Header", 1500.0, 500.0),
            ("Profile", 400.0, 400.0),
        ],
    ),
    (
        "LinkedIn",
        &[
            ("Post", 1200.0, 1200.0),
            ("Link Post", 1200.0, 627.0),
            ("Personal Banner", 1584.0, 396.0),
            ("Company Banner", 1128.0, 191.0),
            ("Profile", 400.0, 400.0),
        ],
    ),
    (
        "YouTube",
        &[
            ("Thumbnail", 1280.0, 720.0),
            ("Banner", 2560.0, 1440.0),
            ("Short", 1080.0, 1920.0),
            ("Profile", 800.0, 800.0),
        ],
    ),
    (
        "TikTok & more",
        &[
            ("TikTok Video", 1080.0, 1920.0),
            ("TikTok Profile", 200.0, 200.0),
            ("Snapchat", 1080.0, 1920.0),
            ("Pinterest Pin", 1000.0, 1500.0),
            ("Pinterest Square", 1000.0, 1000.0),
            ("Pinterest Long", 1000.0, 2100.0),
            ("Tumblr", 1280.0, 1920.0),
            ("Reddit Banner", 1920.0, 384.0),
            ("WhatsApp", 1080.0, 1920.0),
            ("Discord Banner", 960.0, 540.0),
        ],
    ),
    (
        "Twitch",
        &[
            ("Banner", 1200.0, 480.0),
            ("Offline", 1920.0, 1080.0),
            ("Panel", 320.0, 100.0),
            ("Overlay", 1920.0, 1080.0),
            ("Profile", 800.0, 800.0),
        ],
    ),
    (
        "Display ads",
        &[
            ("Leaderboard", 728.0, 90.0),
            ("Banner", 468.0, 60.0),
            ("Skyscraper", 120.0, 600.0),
            ("Wide Skyscraper", 160.0, 600.0),
            ("Med Rectangle", 300.0, 250.0),
            ("Large Rectangle", 336.0, 280.0),
            ("Half Page", 300.0, 600.0),
            ("Billboard", 970.0, 250.0),
            ("Mobile Banner", 320.0, 50.0),
            ("Large Mobile", 320.0, 100.0),
        ],
    ),
    (
        "Device",
        &[
            ("iPhone 15 Pro", 1179.0, 2556.0),
            ("iPhone SE", 750.0, 1334.0),
            ("iPad Pro 12.9", 2048.0, 2732.0),
            ("iPad", 1640.0, 2360.0),
            ("Android", 1080.0, 2340.0),
            ("Pixel", 1080.0, 2400.0),
            ("Galaxy", 1440.0, 3088.0),
            ("Apple Watch", 368.0, 448.0),
        ],
    ),
    (
        "App icons",
        &[
            ("iOS Icon", 1024.0, 1024.0),
            ("Android Icon", 512.0, 512.0),
            ("Touch Icon", 180.0, 180.0),
            ("Adaptive", 432.0, 432.0),
            ("Favicon 64", 64.0, 64.0),
            ("Favicon 32", 32.0, 32.0),
            ("Favicon 16", 16.0, 16.0),
        ],
    ),
    (
        "Square",
        &[
            ("64", 64.0, 64.0),
            ("128", 128.0, 128.0),
            ("256", 256.0, 256.0),
            ("512", 512.0, 512.0),
            ("1024", 1024.0, 1024.0),
            ("2048", 2048.0, 2048.0),
            ("4096", 4096.0, 4096.0),
        ],
    ),
];

// ─── Types ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentEntry {
    pub path: PathBuf,
    pub name: String,
}

/// The fully-resolved parameters chosen in the new-document flow, ready to be
/// turned into a [`photonic_core::Document`]. Produced by [`NewDocumentForm`] and
/// consumed identically by the welcome screen and the in-editor File ▸ New modal.
#[derive(Debug, Clone)]
pub struct NewDocumentSpec {
    pub name: String,
    /// Final canvas dimensions in pixels.
    pub width: f64,
    pub height: f64,
    /// Print bleed in millimetres (0 = none).
    pub bleed_mm: f64,
    /// Print slug area in millimetres (0 = none).
    pub slug_mm: f64,
    /// Uniform safe-area margin inset on all four sides, in pixels (0 = none).
    pub margin: f64,
    /// Number of same-size artboards to create, laid out in a grid (>= 1).
    pub artboards: usize,
    /// Per-document edit-history size cap in MB (#195).
    pub history_max_mb: f64,
}

/// Minimal parameters for a new video timeline project (video-editor-module
/// `04-ui-mode-timeline.md` §1.2 Welcome-action entry point) — the video-mode
/// counterpart of [`NewDocumentSpec`].
#[derive(Debug, Clone)]
pub struct VideoProjectSpec {
    pub name: String,
    pub width: f64,
    pub height: f64,
    pub frame_rate: photonic_core::timeline::FrameRate,
}

pub enum WelcomeAction {
    CreateNew(NewDocumentSpec),
    /// Video-mode counterpart of `CreateNew` (04 §1.2). No UI produces this
    /// yet — the video-mode new-project flow is P2/mode-switch-story work.
    CreateNewVideo(VideoProjectSpec),
    OpenFile(PathBuf),
    OpenBrowse,
    /// Prompt for a folder to add as a disk-search root.
    AddDiskRoot,
}

/// Which tab is showing inside the Open panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenTab {
    Recent,
    Disk,
}

/// Unit the dimension fields are expressed in. Pixels are canonical; mm/in are
/// converted through the chosen DPI when computing the final canvas size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SizeUnit {
    Px,
    Mm,
    In,
}

impl SizeUnit {
    fn label(self) -> &'static str {
        match self {
            SizeUnit::Px => "px",
            SizeUnit::Mm => "mm",
            SizeUnit::In => "in",
        }
    }
    /// Pixels per one unit at the given DPI (DPI = pixels per inch).
    fn px_per_unit(self, dpi: f64) -> f64 {
        match self {
            SizeUnit::Px => 1.0,
            SizeUnit::In => dpi,
            SizeUnit::Mm => dpi / 25.4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WelcomeView {
    Hub,
    NewCanvas,
    Open,
}

// ─── New-document form (shared component) ───────────────────────────────────────

/// The reusable new-document flow: a searchable size catalog, a custom-size box,
/// print/output options, a name field, and a Create button. Rendered both by the
/// welcome screen's New Canvas panel and by the in-editor File ▸ New modal, so the
/// two stay identical by construction rather than by copy-paste.
pub struct NewDocumentForm {
    pub doc_name: String,
    pub width: f64,
    pub height: f64,
    unit: SizeUnit,
    dpi: f64,
    bleed_mm: f64,
    slug_mm: f64,
    margin: f64,
    num_artboards: usize,
    /// Per-document history size cap (MB) for the new file (#195).
    history_mb: f64,
    size_search: String,
}

impl Default for NewDocumentForm {
    fn default() -> Self {
        Self::new()
    }
}

impl NewDocumentForm {
    pub fn new() -> Self {
        Self {
            doc_name: "Untitled".to_string(),
            width: 1123.0,
            height: 794.0,
            unit: SizeUnit::Px,
            dpi: 300.0,
            bleed_mm: 0.0,
            slug_mm: 0.0,
            margin: 0.0,
            num_artboards: 1,
            history_mb: 50.0,
            size_search: String::new(),
        }
    }

    /// Assemble the current field values into a [`NewDocumentSpec`], normalising an
    /// empty/whitespace name to "Untitled".
    fn build_spec(&self) -> NewDocumentSpec {
        let name = self.doc_name.trim();
        let name = if name.is_empty() { "Untitled" } else { name };
        NewDocumentSpec {
            name: name.to_string(),
            width: self.width,
            height: self.height,
            bleed_mm: self.bleed_mm,
            slug_mm: self.slug_mm,
            margin: self.margin,
            artboards: self.num_artboards,
            history_max_mb: self.history_mb,
        }
    }

    /// Draw the form body — name field, the two option containers, and the Create
    /// button — inside the caller-provided width. `opacity` drives an optional
    /// entrance fade (pass 1.0 for no fade). Returns a spec when the user commits
    /// (Create clicked or Enter pressed).
    pub fn draw(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        panel_w: f32,
        opacity: f32,
    ) -> Option<NewDocumentSpec> {
        let mut spec = None;
        ui.add_space(6.0);
        ui.vertical_centered(|ui| {
            ui.set_max_width(panel_w);
            ui.scope(|ui| {
                ui.set_opacity(opacity);
                ui.add_space(lift(opacity));

                // ── Name (centered, on top, in its own distinct container) ───
                let name_w = (panel_w * 0.46).min(560.0);
                ui.allocate_ui_with_layout(
                    Vec2::new(name_w, 0.0),
                    egui::Layout::top_down(egui::Align::Center),
                    |ui| {
                        egui::Frame::none()
                            .fill(BG_PANEL)
                            .rounding(Rounding::same(10.0))
                            .stroke(Stroke::new(1.0, BORDER))
                            .shadow(box_shadow())
                            .inner_margin(Margin::symmetric(18.0, 12.0))
                            .show(ui, |ui| {
                                ui.set_width(name_w - 36.0);
                                ui.vertical_centered(|ui| {
                                    ui.label(
                                        RichText::new("DOCUMENT NAME")
                                            .color(ACCENT_BRIGHT)
                                            .size(10.5)
                                            .strong(),
                                    );
                                    ui.add_space(6.0);
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.doc_name)
                                            .desired_width(f32::INFINITY)
                                            .horizontal_align(egui::Align::Center)
                                            .font(FontId::proportional(15.0)),
                                    );
                                });
                            });
                    },
                );
                ui.add_space(16.0);

                // ── Two separate containers side by side ─────────────────────
                let gap = 18.0;
                let usable = panel_w - 4.0;
                let left_w = (usable - gap) * 0.62;
                let right_w = (usable - gap) * 0.38;
                let container_h = (ui.available_height() - 84.0).clamp(300.0, 640.0);
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    ui.allocate_ui_with_layout(
                        Vec2::new(left_w, container_h),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            container_frame().show(ui, |ui| {
                                ui.set_width(left_w - 36.0);
                                ui.set_min_height(container_h - 36.0);
                                self.draw_size_column(ui, ctx);
                            });
                        },
                    );
                    ui.allocate_ui_with_layout(
                        Vec2::new(right_w, container_h),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            container_frame().show(ui, |ui| {
                                ui.set_width(right_w - 36.0);
                                ui.set_min_height(container_h - 36.0);
                                self.draw_options_column(ui);
                            });
                        },
                    );
                });

                // ── Create button (centered, at bottom) ──────────────────────
                ui.add_space(16.0);
                let mut sub = format!("{} × {} px", self.width as i64, self.height as i64);
                if self.num_artboards > 1 {
                    sub.push_str(&format!("  ·  {} artboards", self.num_artboards));
                }
                if self.bleed_mm > 0.0 {
                    sub.push_str(&format!("  ·  bleed {}mm", trim_num(self.bleed_mm)));
                }
                let label = format!("{}  Create  ·  {}", ph::SPARKLE, sub);
                let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                let mut create = false;
                ui.allocate_ui_with_layout(
                    Vec2::new((panel_w * 0.5).min(520.0), 46.0),
                    egui::Layout::top_down(egui::Align::Center),
                    |ui| {
                        let btn = egui::Button::new(
                            RichText::new(label)
                                .size(13.5)
                                .color(Color32::WHITE)
                                .strong(),
                        )
                        .fill(ACCENT)
                        .stroke(Stroke::new(1.0, ACCENT_BRIGHT))
                        .rounding(Rounding::same(6.0))
                        .min_size(Vec2::new(ui.available_width(), 44.0));
                        if ui.add(btn).clicked() {
                            create = true;
                        }
                    },
                );
                if create || enter {
                    spec = Some(self.build_spec());
                }
            });
        });
        spec
    }

    /// Left container: a searchable size catalog (scrolling) above a visually
    /// distinct custom-size section.
    fn draw_size_column(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        // ── Search bar ───────────────────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.size_search)
                    .hint_text("Search sizes — name, platform, or dimensions…")
                    .desired_width(f32::INFINITY),
            );
            if !self.size_search.is_empty()
                && ui.small_button(ph::X).on_hover_text("Clear").clicked()
            {
                self.size_search.clear();
            }
        });
        ui.add_space(8.0);

        let q = self.size_search.trim().to_lowercase();
        let mut pick: Option<(f64, f64)> = None;
        // Reserve room for the custom-size box so the scroll fills the space
        // above it and the box stays pinned to the container bottom.
        const CUSTOM_H: f32 = 74.0;
        let scroll_h = (ui.available_height() - CUSTOM_H - 12.0).max(150.0);
        egui::ScrollArea::vertical()
            .id_source("canvas_size_scroll")
            .max_height(scroll_h)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut any = false;
                for group_entry in PRESET_GROUPS {
                    let group = group_entry.0;
                    let items = group_entry.1;
                    let filtered: Vec<(&str, f64, f64)> = items
                        .iter()
                        .copied()
                        .filter(|(label, pw, ph_)| {
                            if q.is_empty() {
                                return true;
                            }
                            let hay = format!(
                                "{} {} {} {} {}x{}",
                                label, group, *pw as i64, *ph_ as i64, *pw as i64, *ph_ as i64
                            )
                            .to_lowercase();
                            hay.contains(&q)
                        })
                        .collect();
                    if filtered.is_empty() {
                        continue;
                    }
                    if any {
                        ui.add_space(10.0);
                    }
                    any = true;
                    ui.label(RichText::new(group).color(TEXT_MUTED).size(10.0).strong());
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(9.0, 9.0);
                        for (label, pw, ph_) in filtered {
                            let selected =
                                (self.width - pw).abs() < 0.5 && (self.height - ph_).abs() < 0.5;
                            if aspect_tile(ui, ctx, label, pw, ph_, selected) {
                                pick = Some((pw, ph_));
                            }
                        }
                    });
                }
                if !any {
                    ui.add_space(20.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new("No sizes match your search.")
                                .color(TEXT_MUTED)
                                .italics()
                                .size(12.0),
                        );
                    });
                }
            });
        if let Some((w, h)) = pick {
            self.width = w;
            self.height = h;
        }
        // Push the custom-size box to the bottom of the container.
        let gap = (ui.available_height() - CUSTOM_H).max(8.0);
        ui.add_space(gap);

        // ── Custom size — a visually distinct section (its own framed box) ────
        let ppu = self.unit.px_per_unit(self.dpi);
        let decimals = if self.unit == SizeUnit::Px { 0 } else { 2 };
        let speed = if self.unit == SizeUnit::Px { 1.0 } else { 0.1 };
        egui::Frame::none()
            .fill(BG_ELEVATED)
            .rounding(Rounding::same(7.0))
            .stroke(Stroke::new(1.0, BORDER))
            .shadow(box_shadow())
            .inner_margin(Margin::symmetric(12.0, 10.0))
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new(format!("{}  CUSTOM SIZE", ph::FRAME_CORNERS))
                            .color(ACCENT_BRIGHT)
                            .size(10.5)
                            .strong(),
                    );
                });
                ui.add_space(8.0);
                centered_row(ui, "custom_dims", |ui| {
                    ui.label(RichText::new("W").color(TEXT_MUTED).size(12.0));
                    let mut w = self.width / ppu;
                    if ui
                        .add(
                            egui::DragValue::new(&mut w)
                                .speed(speed)
                                .range(0.01..=100_000.0)
                                .max_decimals(decimals),
                        )
                        .changed()
                    {
                        self.width = (w * ppu).clamp(1.0, 16384.0);
                    }
                    ui.add_space(10.0);
                    ui.label(RichText::new("H").color(TEXT_MUTED).size(12.0));
                    let mut h = self.height / ppu;
                    if ui
                        .add(
                            egui::DragValue::new(&mut h)
                                .speed(speed)
                                .range(0.01..=100_000.0)
                                .max_decimals(decimals),
                        )
                        .changed()
                    {
                        self.height = (h * ppu).clamp(1.0, 16384.0);
                    }
                    ui.add_space(12.0);
                    for u in [SizeUnit::Px, SizeUnit::Mm, SizeUnit::In] {
                        if mini_toggle(ui, u.label(), self.unit == u) {
                            self.unit = u;
                        }
                    }
                });
            });
    }

    /// Right container: print & output options (DPI, bleed, slug, margin, artboards),
    /// all centered.
    fn draw_options_column(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new("PRINT & OUTPUT")
                    .color(ACCENT_BRIGHT)
                    .size(10.5)
                    .strong(),
            );
            ui.add_space(12.0);

            // Resolution (DPI/PPI) — drives mm/in ⇄ px conversion.
            field_label_centered(ui, "Resolution", "Pixels per inch — drives mm/in ⇄ px.");
            centered_row(ui, "resolution", |ui| {
                ui.add(
                    egui::DragValue::new(&mut self.dpi)
                        .speed(1.0)
                        .range(1.0..=2400.0)
                        .max_decimals(0)
                        .suffix(" DPI"),
                );
                for d in [72.0, 96.0, 150.0, 300.0, 600.0] {
                    if mini_toggle(ui, &format!("{}", d as i64), (self.dpi - d).abs() < 0.5) {
                        self.dpi = d;
                    }
                }
            });
            ui.add_space(14.0);

            // Bleed (print) — stored as Document::bleed_mm.
            field_label_centered(ui, "Bleed", "Extra print area on all sides.");
            centered_row(ui, "bleed", |ui| {
                ui.add(
                    egui::DragValue::new(&mut self.bleed_mm)
                        .speed(0.1)
                        .range(0.0..=50.0)
                        .max_decimals(2)
                        .suffix(" mm"),
                );
                if mini_toggle(ui, "3mm EU", (self.bleed_mm - 3.0).abs() < 0.05) {
                    self.bleed_mm = 3.0;
                }
                if mini_toggle(ui, "1/8\" US", (self.bleed_mm - 3.175).abs() < 0.05) {
                    self.bleed_mm = 3.175;
                }
            });
            ui.add_space(14.0);

            // Slug (print) — stored as Document::slug_mm.
            field_label_centered(ui, "Slug", "Margin outside the bleed for marks.");
            ui.add(
                egui::DragValue::new(&mut self.slug_mm)
                    .speed(0.1)
                    .range(0.0..=50.0)
                    .max_decimals(2)
                    .suffix(" mm"),
            );
            ui.add_space(14.0);

            // Safe-area margin — stored on all four Document margins.
            field_label_centered(ui, "Safe margin", "Inset guide on all four sides.");
            ui.add(
                egui::DragValue::new(&mut self.margin)
                    .speed(1.0)
                    .range(0.0..=4096.0)
                    .max_decimals(0)
                    .suffix(" px"),
            );
            ui.add_space(14.0);

            // Artboards — N same-size boards laid out in a grid.
            field_label_centered(ui, "Artboards", "Several same-size boards in a grid.");
            centered_row(ui, "artboards", |ui| {
                let mut n = self.num_artboards as i64;
                if ui
                    .add(
                        egui::DragValue::new(&mut n)
                            .speed(0.1)
                            .range(1..=64)
                            .max_decimals(0),
                    )
                    .changed()
                {
                    self.num_artboards = n.clamp(1, 64) as usize;
                }
                for c in [1_usize, 2, 4, 6] {
                    if mini_toggle(ui, &format!("{c}"), self.num_artboards == c) {
                        self.num_artboards = c;
                    }
                }
            });
            ui.add_space(14.0);

            // Per-document edit-history size cap (#195).
            field_label_centered(
                ui,
                "History budget",
                "Edit-history size cap for this file (MB).",
            );
            centered_row(ui, "history_mb", |ui| {
                ui.add(
                    egui::DragValue::new(&mut self.history_mb)
                        .speed(1.0)
                        .range(1.0..=4000.0)
                        .max_decimals(0)
                        .suffix(" MB"),
                );
                for m in [25.0_f64, 50.0, 100.0, 250.0] {
                    if mini_toggle(
                        ui,
                        &format!("{}", m as i64),
                        (self.history_mb - m).abs() < 0.5,
                    ) {
                        self.history_mb = m;
                    }
                }
            });
        });
    }
}

// ─── State ────────────────────────────────────────────────────────────────────

pub struct WelcomeState {
    /// The shared new-document flow (size catalog, options, name) — identical to
    /// the one used by the in-editor File ▸ New modal.
    form: NewDocumentForm,
    pub recent: Vec<RecentEntry>,
    view: WelcomeView,
    /// Frame-clock time (`input.time`) at which the screen first drew — anchors
    /// the one-shot entrance reveal.
    appeared_at: Option<f64>,
    thumbs: Thumbnailer,
    /// Lazily-uploaded transparent logo mark shown above the hub wordmark.
    logo_tex: Option<TextureHandle>,
    // ── Open panel ──
    open_tab: OpenTab,
    disk_roots: Vec<PathBuf>,
    disk_filter: String,
    disk: crate::disk_search::DiskScanner,
    /// Whether the disk tab has kicked off its first scan this session.
    disk_scanned: bool,
}

impl Default for WelcomeState {
    fn default() -> Self {
        Self::new()
    }
}

impl WelcomeState {
    pub fn new() -> Self {
        Self {
            form: NewDocumentForm::new(),
            recent: load_recent(),
            view: WelcomeView::Hub,
            appeared_at: None,
            thumbs: Thumbnailer::new(),
            logo_tex: None,
            open_tab: OpenTab::Recent,
            disk_roots: load_disk_roots(),
            disk_filter: String::new(),
            disk: crate::disk_search::DiskScanner::new(),
            disk_scanned: false,
        }
    }

    /// Add a folder as a disk-search root (deduped), persist, and rescan.
    pub fn add_disk_root(&mut self, path: PathBuf) {
        if !self.disk_roots.contains(&path) {
            self.disk_roots.push(path);
            save_disk_roots(&self.disk_roots);
            self.disk.rescan(self.disk_roots.clone(), false);
            self.disk_scanned = true;
        }
    }

    /// Record a file in the recent list (modifies in-memory + saves to disk).
    pub fn add_recent(&mut self, path: PathBuf, name: String) {
        self.recent.retain(|e| e.path != path);
        self.recent.insert(0, RecentEntry { path, name });
        self.recent.truncate(MAX_RECENT);
        save_recent(&self.recent);
    }

    /// Draw the welcome screen, returning an action if the user made a choice.
    pub fn draw(&mut self, ctx: &egui::Context) -> Option<WelcomeAction> {
        // The welcome screen is always dark, regardless of the app's light/dark
        // preference — otherwise egui widgets (fields, buttons) would render light
        // over the dark background.
        ctx.set_visuals(crate::theme::build_dark_theme());
        let t = ctx.input(|i| i.time);
        let appeared = *self.appeared_at.get_or_insert(t);
        let elapsed = (t - appeared) as f32;
        // Repaint every frame so the animated Lightfall background advances.
        ctx.request_repaint();
        // Upload any thumbnails that finished rendering since last frame.
        self.thumbs.pump(ctx);
        // Drain any disk-search results found since last frame.
        self.disk.pump();

        let mut action: Option<WelcomeAction> = None;
        let mut next_view: Option<WelcomeView> = None;

        // ── Global keyboard shortcuts ────────────────────────────────────────
        ctx.input(|i| {
            use egui::Key;
            match self.view {
                WelcomeView::Hub => {
                    if i.key_pressed(Key::N) {
                        next_view = Some(WelcomeView::NewCanvas);
                    }
                    if i.key_pressed(Key::O) {
                        next_view = Some(WelcomeView::Open);
                    }
                    if i.key_pressed(Key::V) {
                        action = Some(WelcomeAction::CreateNewVideo(VideoProjectSpec {
                            name: "Untitled".to_string(),
                            width: 1920.0,
                            height: 1080.0,
                            frame_rate: photonic_core::timeline::FrameRate::FPS_30,
                        }));
                    }
                }
                _ => {
                    if i.key_pressed(Key::Escape) {
                        next_view = Some(WelcomeView::Hub);
                    }
                }
            }
        });

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(BG_BASE))
            .show(ctx, |ui| {
                let full = ui.max_rect();
                // Animated Lightfall shader background (real fullscreen GPU pass
                // behind the UI). Falls back to BG_BASE if not installed.
                crate::lightfall::paint(ui, full, t as f32);
                // Dark gradient over the whole background, plus an extra radial
                // scrim in the centre so text + boxes read clearly.
                paint_bg_gradient(ui.painter(), full);
                paint_center_scrim(ui.painter(), full);

                match self.view {
                    WelcomeView::Hub => {
                        self.draw_hub(ui, ctx, elapsed, &mut action, &mut next_view);
                    }
                    WelcomeView::NewCanvas => {
                        self.draw_new(ui, ctx, &mut action, &mut next_view);
                    }
                    WelcomeView::Open => {
                        self.draw_open(ui, ctx, &mut action, &mut next_view);
                    }
                }
            });

        if let Some(v) = next_view {
            self.view = v;
            ctx.request_repaint();
        }
        action
    }

    // ── Hub: the landing fork ────────────────────────────────────────────────

    fn draw_hub(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        elapsed: f32,
        action: &mut Option<WelcomeAction>,
        next_view: &mut Option<WelcomeView>,
    ) {
        // Staggered reveal: each element fades + drifts up off one shared clock.
        let r_word = reveal(elapsed, 0.00, 0.55);
        let r_sub = reveal(elapsed, 0.10, 0.55);
        let r_card1 = reveal(elapsed, 0.20, 0.55);
        let r_card2 = reveal(elapsed, 0.28, 0.55);
        let r_card3 = reveal(elapsed, 0.36, 0.55);

        let avail = ui.available_height();
        ui.add_space(avail * 0.11);

        // ── Logo mark (the transparent Photonic sparkle) ──
        // Lazily upload the embedded mark once, then draw it centered above the
        // wordmark, fading in on the same reveal clock.
        if self.logo_tex.is_none() {
            if let Ok(img) = photonic_core::raster::image::RasterImage::from_encoded(
                include_bytes!("../assets/logo_mark.png"),
            ) {
                let color = egui::ColorImage::from_rgba_unmultiplied(
                    [img.width as usize, img.height as usize],
                    &img.pixels,
                );
                self.logo_tex =
                    Some(ctx.load_texture("photonic_logo_mark", color, TextureOptions::LINEAR));
            }
        }
        if let Some(tex) = &self.logo_tex {
            ui.vertical_centered(|ui| {
                let sz = 96.0;
                ui.add(
                    egui::Image::new(egui::load::SizedTexture::new(tex.id(), Vec2::splat(sz)))
                        .tint(Color32::from_white_alpha((255.0 * r_word) as u8)),
                );
            });
            ui.add_space(10.0);
        }

        // Wordmark + subtitle.
        ui.vertical_centered(|ui| {
            let p = ui.painter();
            let cx = ui.max_rect().center().x;
            let y = ui.cursor().top();
            text_shadow(
                p,
                Pos2::new(cx, y + lift(r_word)),
                Align2::CENTER_TOP,
                "PHOTONIC",
                FontId::proportional(40.0),
                fade(ACCENT, r_word),
            );
        });
        ui.add_space(46.0);
        ui.vertical_centered(|ui| {
            let p = ui.painter();
            let cx = ui.max_rect().center().x;
            let y = ui.cursor().top();
            text_shadow(
                p,
                Pos2::new(cx, y + lift(r_sub)),
                Align2::CENTER_TOP,
                "a modern graphics studio",
                FontId::proportional(13.5),
                fade(TEXT_MUTED, r_sub),
            );
        });
        ui.add_space(54.0);

        // Two hero cards, centered.
        let card_w = 218.0;
        let card_h = 158.0;
        let gap = 22.0;
        let recent_hint = if self.recent.is_empty() {
            "Pick up where you left off".to_string()
        } else if self.recent.len() == 1 {
            "1 recent document".to_string()
        } else {
            format!("{} recent documents", self.recent.len())
        };

        ui.vertical_centered(|ui| {
            ui.set_max_width(card_w * 3.0 + gap * 2.0);
            ui.horizontal(|ui| {
                if hero_card(
                    ui,
                    ctx,
                    "hub_new",
                    Vec2::new(card_w, card_h),
                    ph::SPARKLE,
                    "New Canvas",
                    "Start something blank",
                    r_card1,
                ) {
                    *next_view = Some(WelcomeView::NewCanvas);
                }
                ui.add_space(gap);
                if hero_card(
                    ui,
                    ctx,
                    "hub_open",
                    Vec2::new(card_w, card_h),
                    ph::CLOCK_COUNTER_CLOCKWISE,
                    "Open",
                    &recent_hint,
                    r_card2,
                ) {
                    *next_view = Some(WelcomeView::Open);
                }
                ui.add_space(gap);
                // Video-mode entry point (video-editor-module
                // 04-ui-mode-timeline.md §1.2 Welcome-action entry). Fires
                // `WelcomeAction::CreateNewVideo` directly with a sensible
                // default (1920x1080/30fps) rather than routing through a
                // sizing sub-view — a full new-video-project form is
                // 05-import-export.md's territory, not this mode-switch story.
                if hero_card(
                    ui,
                    ctx,
                    "hub_new_video",
                    Vec2::new(card_w, card_h),
                    ph::FILM_STRIP,
                    "New Video Project",
                    "Start a video timeline",
                    r_card3,
                ) {
                    *action = Some(WelcomeAction::CreateNewVideo(VideoProjectSpec {
                        name: "Untitled".to_string(),
                        width: 1920.0,
                        height: 1080.0,
                        frame_rate: photonic_core::timeline::FrameRate::FPS_30,
                    }));
                }
            });
        });

        // Quiet hint strip at the very bottom.
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height().max(20.0) - 38.0);
            ui.scope(|ui| {
                ui.set_opacity(r_card2 * 0.8);
                ui.label(
                    RichText::new("N  new canvas      O  open      V  new video")
                        .size(11.0)
                        .color(TEXT_MUTED),
                );
            });
        });
    }

    // ── New Canvas panel ─────────────────────────────────────────────────────

    fn draw_new(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        action: &mut Option<WelcomeAction>,
        next_view: &mut Option<WelcomeView>,
    ) {
        let anim = ctx.animate_bool_with_time(egui::Id::new("welcome_anim_new"), true, 0.18);
        panel_chrome(ui, ctx, "New canvas", next_view);

        // The size catalog, options, and Create button are the shared new-document
        // form -- identical to the File > New modal shown inside the editor.
        let panel_w = (ui.available_width() - 56.0).clamp(620.0, 1280.0);
        if let Some(spec) = self.form.draw(ui, ctx, panel_w, anim) {
            *action = Some(WelcomeAction::CreateNew(spec));
        }
    }

    // ── Open panel ───────────────────────────────────────────────────────────

    fn draw_open(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        action: &mut Option<WelcomeAction>,
        next_view: &mut Option<WelcomeView>,
    ) {
        let anim = ctx.animate_bool_with_time(egui::Id::new("welcome_anim_open"), true, 0.18);
        panel_chrome(ui, ctx, "Open", next_view);

        let panel_w = 720.0_f32.min(ui.available_width() - 48.0);
        ui.add_space(8.0);
        ui.vertical_centered(|ui| {
            ui.set_max_width(panel_w);
            ui.scope(|ui| {
                ui.set_opacity(anim);
                ui.add_space(lift(anim));

                // ── Tab strip: Recent · On disk ──────────────────────────────
                centered_row(ui, "open_tabs", |ui| {
                    if open_tab_btn(
                        ui,
                        ph::CLOCK_COUNTER_CLOCKWISE,
                        "Recent",
                        self.open_tab == OpenTab::Recent,
                    ) {
                        self.open_tab = OpenTab::Recent;
                    }
                    if open_tab_btn(
                        ui,
                        ph::FOLDER_OPEN,
                        "On disk",
                        self.open_tab == OpenTab::Disk,
                    ) {
                        self.open_tab = OpenTab::Disk;
                    }
                });
                ui.add_space(12.0);

                match self.open_tab {
                    OpenTab::Recent => self.draw_recent_grid(ui, ctx, action, panel_w),
                    OpenTab::Disk => self.draw_disk_view(ui, ctx, action, panel_w),
                }
            });
        });
    }

    /// Recent tab: the browse tile + recent-document preview cards.
    fn draw_recent_grid(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        action: &mut Option<WelcomeAction>,
        panel_w: f32,
    ) {
        let (card_w, card_h, gap) = (178.0, 168.0, 16.0);
        let per_row = ((panel_w + gap) / (card_w + gap)).floor().max(1.0) as usize;
        egui::ScrollArea::vertical()
            .id_source("recent_scroll")
            .max_height(ui.available_height() - 12.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = Vec2::new(gap, gap);
                ui.add_space(6.0);
                let count = self.recent.len() + 1;
                let mut idx = 0;
                while idx < count {
                    ui.horizontal(|ui| {
                        for _ in 0..per_row {
                            if idx >= count {
                                break;
                            }
                            if idx == 0 {
                                if browse_card(ui, ctx, Vec2::new(card_w, card_h)) {
                                    *action = Some(WelcomeAction::OpenBrowse);
                                }
                            } else {
                                let path = self.recent[idx - 1].path.clone();
                                let thumb = self.thumbs.get(ctx, &path);
                                let entry = &self.recent[idx - 1];
                                if recent_card(
                                    ui,
                                    ctx,
                                    idx,
                                    Vec2::new(card_w, card_h),
                                    entry,
                                    Some(&thumb),
                                ) {
                                    *action = Some(WelcomeAction::OpenFile(path.clone()));
                                }
                            }
                            idx += 1;
                        }
                    });
                }
                if self.recent.is_empty() {
                    ui.add_space(14.0);
                    ui.label(
                        RichText::new("No recent documents yet — open one to see it here.")
                            .color(TEXT_MUTED)
                            .size(12.0)
                            .italics(),
                    );
                }
            });
    }

    /// On-disk tab: managed search roots + a live grid of `.photon` files found.
    fn draw_disk_view(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        action: &mut Option<WelcomeAction>,
        panel_w: f32,
    ) {
        // Kick off the first scan when the tab is first opened.
        if !self.disk_scanned {
            self.disk_scanned = true;
            if !self.disk_roots.is_empty() {
                self.disk.rescan(self.disk_roots.clone(), false);
            }
        }

        // ── Search roots: chips + add folder ─────────────────────────────────
        let mut remove: Option<usize> = None;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 6.0);
            for (i, root) in self.disk_roots.iter().enumerate() {
                let name = root
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| root.to_string_lossy().into_owned());
                egui::Frame::none()
                    .fill(BG_ELEVATED)
                    .rounding(Rounding::same(5.0))
                    .stroke(Stroke::new(1.0, BORDER))
                    .inner_margin(Margin::symmetric(8.0, 4.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("{}  {}", ph::FOLDER_OPEN, name))
                                    .size(11.5)
                                    .color(TEXT_PRIMARY),
                            )
                            .on_hover_text(root.to_string_lossy().into_owned());
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new(ph::X).size(11.0).color(TEXT_MUTED),
                                    )
                                    .frame(false),
                                )
                                .clicked()
                            {
                                remove = Some(i);
                            }
                        });
                    });
            }
            if ui
                .add(
                    egui::Button::new(
                        RichText::new(format!("{}  Add folder", ph::PLUS))
                            .size(11.5)
                            .color(ACCENT_BRIGHT),
                    )
                    .fill(BG_ELEVATED)
                    .stroke(Stroke::new(1.0, BORDER))
                    .rounding(Rounding::same(5.0))
                    .min_size(Vec2::new(0.0, 26.0)),
                )
                .clicked()
            {
                *action = Some(WelcomeAction::AddDiskRoot);
            }
        });
        if let Some(i) = remove {
            self.disk_roots.remove(i);
            save_disk_roots(&self.disk_roots);
            self.disk.rescan(self.disk_roots.clone(), false);
        }
        ui.add_space(10.0);

        if self.disk_roots.is_empty() {
            ui.add_space(28.0);
            ui.label(
                RichText::new(format!(
                    "Add a folder or drive to search it for .{PHOTON_FILE_EXTENSION} files."
                ))
                .color(TEXT_MUTED)
                .size(13.0),
            );
            return;
        }

        // ── Filter + status + rescan ─────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.disk_filter)
                    .hint_text("Filter by name…")
                    .desired_width(220.0),
            );
            ui.add_space(10.0);
            let status = if self.disk.scanning {
                format!("Scanning…  {} found", self.disk.files.len())
            } else {
                format!("{} .{PHOTON_FILE_EXTENSION} files", self.disk.files.len())
            };
            ui.label(RichText::new(status).size(11.5).color(TEXT_MUTED));
            ui.add_space(10.0);
            if ui
                .add(
                    egui::Button::new(RichText::new("Rescan").size(11.5).color(TEXT_PRIMARY))
                        .fill(BG_ELEVATED)
                        .stroke(Stroke::new(1.0, BORDER))
                        .rounding(Rounding::same(5.0)),
                )
                .on_hover_text("Deep rescan (fresh walk, ignores the OS index)")
                .clicked()
            {
                self.disk.rescan(self.disk_roots.clone(), true);
            }
        });
        ui.add_space(10.0);

        // ── Results grid (clone to avoid borrowing the scanner while drawing) ─
        let q = self.disk_filter.trim().to_lowercase();
        let files: Vec<(PathBuf, String)> = self
            .disk
            .files
            .iter()
            .filter(|f| q.is_empty() || f.name.to_lowercase().contains(&q))
            .map(|f| (f.path.clone(), f.name.clone()))
            .collect();
        let scanning = self.disk.scanning;

        let (card_w, card_h, gap) = (178.0, 168.0, 16.0);
        let per_row = ((panel_w + gap) / (card_w + gap)).floor().max(1.0) as usize;
        egui::ScrollArea::vertical()
            .id_source("disk_scroll")
            .max_height(ui.available_height() - 12.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = Vec2::new(gap, gap);
                ui.add_space(6.0);
                if files.is_empty() {
                    ui.add_space(14.0);
                    let msg = if scanning {
                        "Scanning…".to_string()
                    } else {
                        format!("No .{PHOTON_FILE_EXTENSION} files found in these folders.")
                    };
                    ui.label(RichText::new(msg).color(TEXT_MUTED).size(12.0).italics());
                    return;
                }
                let mut i = 0;
                while i < files.len() {
                    ui.horizontal(|ui| {
                        for _ in 0..per_row {
                            if i >= files.len() {
                                break;
                            }
                            let path = files[i].0.clone();
                            let name = files[i].1.clone();
                            let thumb = self.thumbs.get(ctx, &path);
                            let entry = RecentEntry {
                                path: path.clone(),
                                name,
                            };
                            if recent_card(
                                ui,
                                ctx,
                                100_000 + i,
                                Vec2::new(card_w, card_h),
                                &entry,
                                Some(&thumb),
                            ) {
                                *action = Some(WelcomeAction::OpenFile(path.clone()));
                            }
                            i += 1;
                        }
                    });
                }
            });
    }
}

// ─── Shared chrome: shrunk wordmark + back affordance ──────────────────────────

fn panel_chrome(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    title: &str,
    next_view: &mut Option<WelcomeView>,
) {
    ui.add_space(22.0);
    ui.horizontal(|ui| {
        ui.add_space(28.0);
        // Back chevron.
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(34.0, 28.0), Sense::click());
        let hot = resp.hovered();
        if hot {
            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            ph::ARROW_LEFT,
            FontId::proportional(18.0),
            if hot { TEXT_PRIMARY } else { TEXT_MUTED },
        );
        if resp.clicked() {
            *next_view = Some(WelcomeView::Hub);
        }
        ui.add_space(2.0);
        ui.label(RichText::new("PHOTONIC").size(13.0).color(ACCENT).strong());
        ui.add_space(10.0);
        ui.label(RichText::new("/").size(13.0).color(BORDER));
        ui.add_space(10.0);
        ui.label(RichText::new(title).size(13.0).color(TEXT_PRIMARY));
    });
    ui.add_space(10.0);
    let sep_y = ui.cursor().top();
    ui.painter().hline(
        ui.max_rect().x_range(),
        sep_y,
        Stroke::new(1.0, Color32::from_rgb(25, 25, 42)),
    );
    ui.add_space(14.0);
}

// ─── Small advanced-panel widgets ──────────────────────────────────────────────

/// A compact pill toggle used for unit / DPI / bleed quick-picks.
fn mini_toggle(ui: &mut egui::Ui, label: &str, selected: bool) -> bool {
    let btn = egui::Button::new(RichText::new(label).size(11.0).color(if selected {
        Color32::WHITE
    } else {
        TEXT_MUTED
    }))
    .fill(if selected { ACCENT } else { BG_ELEVATED })
    .stroke(Stroke::new(
        1.0,
        if selected { ACCENT_BRIGHT } else { BORDER },
    ))
    .rounding(Rounding::same(4.0))
    .min_size(Vec2::new(0.0, 22.0));
    ui.add(btn).clicked()
}

/// Lay a row of widgets out horizontally, truly centered. egui does not honor
/// `main_align: Center` for incrementally-placed rows, so we measure the content
/// width (via `min_rect`, cached in egui memory — stable because the welcome
/// repaints every frame) and center it with a leading spacer.
fn centered_row(ui: &mut egui::Ui, salt: &str, content: impl FnOnce(&mut egui::Ui)) {
    let id = ui.id().with(("centered_row", salt));
    let measured: f32 = ui
        .ctx()
        .memory(|m| m.data.get_temp::<f32>(id))
        .unwrap_or(0.0);
    let lead = ((ui.available_width() - measured) * 0.5).max(0.0);
    let mut content_w = measured;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
        if lead > 0.5 {
            ui.add_space(lead);
        }
        let before = ui.min_rect().right();
        content(ui);
        content_w = (ui.min_rect().right() - before).max(0.0);
    });
    ui.ctx().memory_mut(|m| m.data.insert_temp(id, content_w));
}

/// A tab button for the Open panel's tab strip.
fn open_tab_btn(ui: &mut egui::Ui, icon: &str, label: &str, selected: bool) -> bool {
    let btn = egui::Button::new(
        RichText::new(format!("{}  {}", icon, label))
            .size(13.0)
            .color(if selected { Color32::WHITE } else { TEXT_MUTED }),
    )
    .fill(if selected { ACCENT } else { BG_ELEVATED })
    .stroke(Stroke::new(
        1.0,
        if selected { ACCENT_BRIGHT } else { BORDER },
    ))
    .rounding(Rounding::same(6.0))
    .min_size(Vec2::new(132.0, 32.0));
    ui.add(btn).clicked()
}

/// A bordered panel container used for the New Canvas side-by-side columns.
fn container_frame() -> egui::Frame {
    egui::Frame::none()
        .fill(BG_PANEL)
        .rounding(Rounding::same(10.0))
        .stroke(Stroke::new(1.0, BORDER))
        .shadow(box_shadow())
        .inner_margin(Margin::same(18.0))
}

/// A soft drop shadow for cards/boxes, lifting them off the animated background.
fn box_shadow() -> egui::epaint::Shadow {
    egui::epaint::Shadow {
        offset: Vec2::new(0.0, 6.0),
        blur: 22.0,
        spread: 0.0,
        color: Color32::from_black_alpha(130),
    }
}

/// Paint a soft drop shadow behind a manually-drawn card `rect`.
fn shadow_behind(p: &egui::Painter, rect: Rect, radius: f32) {
    p.add(box_shadow().as_shape(rect, Rounding::same(radius)));
}

/// Draw text with a soft dark shadow for legibility over the background.
fn text_shadow(
    p: &egui::Painter,
    pos: Pos2,
    anchor: Align2,
    text: impl Into<String>,
    font: FontId,
    color: Color32,
) {
    let s = text.into();
    p.text(
        pos + Vec2::new(0.0, 2.0),
        anchor,
        &s,
        font.clone(),
        Color32::from_black_alpha(170),
    );
    p.text(pos, anchor, s, font, color);
}

/// A full-screen vertical dark gradient over the whole background, subduing the
/// animated shader (lighter at the top, darker toward the bottom).
fn paint_bg_gradient(p: &egui::Painter, rect: Rect) {
    let top = Color32::from_black_alpha(95);
    let bot = Color32::from_black_alpha(155);
    let mut mesh = Mesh::default();
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.right_bottom(), bot);
    mesh.colored_vertex(rect.left_bottom(), bot);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    p.add(mesh);
}

/// Darken the centre of the welcome screen so content reads clearly over the
/// animated Lightfall background — a broad radial scrim, opaque-ish in the
/// middle and fading to transparent toward the edges.
fn paint_center_scrim(p: &egui::Painter, rect: Rect) {
    let center = rect.center();
    let radius = rect.size().max_elem() * 0.62;
    let mut mesh = Mesh::default();
    mesh.colored_vertex(center, Color32::from_black_alpha(160));
    let edge = Color32::from_black_alpha(0);
    let n = 48;
    for i in 0..=n {
        let ang = (i as f32 / n as f32) * std::f32::consts::TAU;
        mesh.colored_vertex(center + Vec2::new(ang.cos(), ang.sin()) * radius, edge);
        if i > 0 {
            mesh.add_triangle(0, i, i + 1);
        }
    }
    p.add(mesh);
}

/// A stacked field label (title on its own line, with a hover hint) sitting
/// above its control. In a centered layout the label centers itself.
fn field_label_centered(ui: &mut egui::Ui, title: &str, hint: &str) {
    ui.add(egui::Label::new(
        RichText::new(title).size(11.5).color(TEXT_PRIMARY),
    ))
    .on_hover_text(hint);
    ui.add_space(4.0);
}

/// Format a number without trailing zeros (e.g. 3.0 → "3", 3.175 → "3.18").
fn trim_num(x: f64) -> String {
    let r = (x * 100.0).round() / 100.0;
    if r.fract().abs() < 1e-9 {
        format!("{}", r as i64)
    } else {
        format!("{r:.2}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}

// ─── Hero card (Hub) ───────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn hero_card(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    id: &str,
    size: Vec2,
    icon: &str,
    title: &str,
    subtitle: &str,
    reveal: f32,
) -> bool {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let hov = ctx.animate_bool_with_time(egui::Id::new(id), resp.hovered(), 0.12);
    if resp.hovered() {
        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    // Reveal: fade + a small upward drift that settles to zero.
    let draw = rect.translate(Vec2::new(0.0, lift(reveal)));
    // Hover micro-lift: a hair of vertical rise.
    let draw = draw.translate(Vec2::new(0.0, -3.0 * hov));
    let p = ui.painter();

    let fill = lerp_color(BG_PANEL, BG_ELEVATED, hov);
    let stroke = lerp_color(BORDER, ACCENT, hov);
    if reveal > 0.5 {
        shadow_behind(p, draw, 12.0);
    }
    p.rect(
        draw,
        Rounding::same(12.0),
        fade(fill, reveal),
        Stroke::new(1.0 + hov, fade(stroke, reveal)),
    );

    // Icon in an accent chip near the top.
    let icon_c = lerp_color(ACCENT, ACCENT_BRIGHT, hov);
    p.text(
        Pos2::new(draw.left() + 22.0, draw.top() + 26.0),
        Align2::LEFT_TOP,
        icon,
        FontId::proportional(30.0),
        fade(icon_c, reveal),
    );
    // Title + subtitle anchored to the lower-left.
    p.text(
        Pos2::new(draw.left() + 22.0, draw.bottom() - 50.0),
        Align2::LEFT_TOP,
        title,
        FontId::proportional(18.0),
        fade(TEXT_PRIMARY, reveal),
    );
    p.text(
        Pos2::new(draw.left() + 22.0, draw.bottom() - 26.0),
        Align2::LEFT_TOP,
        subtitle,
        FontId::proportional(12.0),
        fade(TEXT_MUTED, reveal),
    );

    resp.clicked()
}

// ─── Aspect-ratio preset tile (New Canvas) ─────────────────────────────────────

fn aspect_tile(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    label: &str,
    pw: f64,
    ph_: f64,
    selected: bool,
) -> bool {
    let size = Vec2::new(94.0, 98.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let hov = ctx.animate_bool_with_time(
        egui::Id::new(("aspect", label, pw as i64, ph_ as i64)),
        resp.hovered(),
        0.1,
    );
    if resp.hovered() {
        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let p = ui.painter();

    let fill = if selected {
        ACCENT_DIM
    } else {
        lerp_color(BG_ELEVATED, BG_WIDGET, hov)
    };
    let stroke = if selected {
        ACCENT
    } else {
        lerp_color(BORDER, ACCENT, hov * 0.6)
    };
    p.rect(rect, Rounding::same(6.0), fill, Stroke::new(1.0, stroke));

    // Proportional rectangle representing the aspect ratio, near the top so the
    // (possibly wrapped) label + dimensions have clear room below it.
    let max_box = Vec2::new(40.0, 24.0);
    let ar = (pw / ph_) as f32;
    let (rw, rh) = if ar >= max_box.x / max_box.y {
        (max_box.x, max_box.x / ar)
    } else {
        (max_box.y * ar, max_box.y)
    };
    let center = Pos2::new(rect.center().x, rect.top() + 20.0);
    let prect = Rect::from_center_size(center, Vec2::new(rw, rh));
    let pr_fill = if selected { ACCENT_BRIGHT } else { TEXT_MUTED };
    p.rect(prect, Rounding::same(2.0), pr_fill, Stroke::NONE);

    // Label — a left-aligned wrapped galley, drawn centered as a block, so long
    // names (e.g. "Letter Landscape") break onto a second line and stay inside
    // the tile instead of poking out the sides.
    let label_y = rect.top() + 37.0;
    let galley = p.layout(
        label.to_string(),
        FontId::proportional(10.0),
        TEXT_PRIMARY,
        size.x - 12.0,
    );
    p.galley(
        Pos2::new(rect.center().x - galley.size().x / 2.0, label_y),
        galley.clone(),
        TEXT_PRIMARY,
    );
    p.text(
        Pos2::new(rect.center().x, label_y + galley.size().y + 3.0),
        Align2::CENTER_TOP,
        format!("{}×{}", pw as i64, ph_ as i64),
        FontId::proportional(8.5),
        TEXT_MUTED,
    );

    resp.clicked()
}

// ─── Recent + browse cards (Open) ──────────────────────────────────────────────

fn browse_card(ui: &mut egui::Ui, ctx: &egui::Context, size: Vec2) -> bool {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let hov = ctx.animate_bool_with_time(egui::Id::new("open_browse"), resp.hovered(), 0.12);
    if resp.hovered() {
        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let p = ui.painter();
    let draw = rect.translate(Vec2::new(0.0, -3.0 * hov));
    let stroke = lerp_color(BORDER, ACCENT, hov);
    // Ghost card — subtle fill, dashed-feel via lighter stroke.
    shadow_behind(p, draw, 10.0);
    p.rect(
        draw,
        Rounding::same(10.0),
        lerp_color(BG_BASE, BG_PANEL, 0.5 + hov * 0.5),
        Stroke::new(1.0, stroke),
    );
    p.text(
        draw.center() - Vec2::new(0.0, 12.0),
        Align2::CENTER_CENTER,
        ph::FOLDER_OPEN,
        FontId::proportional(34.0),
        lerp_color(TEXT_MUTED, ACCENT_BRIGHT, hov),
    );
    p.text(
        Pos2::new(draw.center().x, draw.bottom() - 26.0),
        Align2::CENTER_TOP,
        "Open file…",
        FontId::proportional(13.0),
        TEXT_PRIMARY,
    );
    resp.clicked()
}

fn recent_card(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    idx: usize,
    size: Vec2,
    entry: &RecentEntry,
    thumb: Option<&Thumb>,
) -> bool {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let hov = ctx.animate_bool_with_time(egui::Id::new(("recent", idx)), resp.hovered(), 0.12);
    if resp.hovered() {
        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let draw = rect.translate(Vec2::new(0.0, -3.0 * hov));
    let p = ui.painter();

    // Card body.
    shadow_behind(p, draw, 10.0);
    p.rect(
        draw,
        Rounding::same(10.0),
        lerp_color(BG_PANEL, BG_ELEVATED, hov),
        Stroke::new(1.0, lerp_color(BORDER, ACCENT, hov)),
    );

    // Thumbnail well (top portion).
    let pad = 8.0;
    let well = Rect::from_min_max(
        draw.left_top() + Vec2::new(pad, pad),
        Pos2::new(draw.right() - pad, draw.top() + size.y - 42.0),
    );
    p.rect(
        well,
        Rounding::same(6.0),
        Color32::from_rgb(15, 15, 24),
        Stroke::NONE,
    );

    let file_name = entry
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| entry.name.clone());
    let dir = entry
        .path
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();

    match thumb {
        Some(Thumb::Ready(tex)) => {
            // Fit the preview inside the well, preserving aspect.
            let isz = tex.size_vec2();
            let ar = isz.x / isz.y.max(1.0);
            let (mut w, mut h) = (well.width() - 8.0, (well.width() - 8.0) / ar);
            if h > well.height() - 8.0 {
                h = well.height() - 8.0;
                w = h * ar;
            }
            let irect = Rect::from_center_size(well.center(), Vec2::new(w, h));
            // Soft drop shadow behind the artboard.
            p.rect(
                irect.translate(Vec2::new(0.0, 2.0)),
                Rounding::same(2.0),
                Color32::from_black_alpha(120),
                Stroke::NONE,
            );
            p.image(
                tex.id(),
                irect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        _ => {
            // Stylized placeholder: accent wash + the file's initial.
            placeholder_thumb(p, well, &file_name, matches!(thumb, Some(Thumb::Failed)));
        }
    }

    // Filename + directory.
    p.text(
        Pos2::new(draw.left() + pad + 2.0, draw.bottom() - 32.0),
        Align2::LEFT_TOP,
        elide(&file_name, 22),
        FontId::proportional(12.5),
        TEXT_PRIMARY,
    );
    p.text(
        Pos2::new(draw.left() + pad + 2.0, draw.bottom() - 16.0),
        Align2::LEFT_TOP,
        elide(&dir, 26),
        FontId::proportional(9.5),
        TEXT_MUTED,
    );

    resp.clicked()
}

fn placeholder_thumb(p: &egui::Painter, well: Rect, name: &str, failed: bool) {
    // Faint accent gradient via a vertical two-tri mesh.
    let mut mesh = Mesh::default();
    let top = fade(ACCENT_DIM, 0.55);
    let bot = fade(BG_WIDGET, 0.9);
    mesh.colored_vertex(well.left_top(), top);
    mesh.colored_vertex(well.right_top(), top);
    mesh.colored_vertex(well.right_bottom(), bot);
    mesh.colored_vertex(well.left_bottom(), bot);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    // Clip the mesh to the rounded well by painting it then a frame.
    p.add(mesh);
    if failed {
        p.text(
            well.center(),
            Align2::CENTER_CENTER,
            ph::IMAGE,
            FontId::proportional(22.0),
            TEXT_MUTED,
        );
    } else {
        let initial = name
            .chars()
            .find(|c| c.is_alphanumeric())
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "·".to_string());
        p.text(
            well.center(),
            Align2::CENTER_CENTER,
            initial,
            FontId::proportional(30.0),
            fade(TEXT_PRIMARY, 0.85),
        );
    }
}

// ─── Animation + paint helpers ─────────────────────────────────────────────────

/// Cubic ease-out, 0 → 1.
fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// Reveal progress (0 → 1) for an element starting at `delay` over `dur`.
fn reveal(elapsed: f32, delay: f32, dur: f32) -> f32 {
    ease_out_cubic((elapsed - delay) / dur)
}

/// Vertical drift for a reveal: 10px → 0 as progress 0 → 1.
fn lift(reveal: f32) -> f32 {
    10.0 * (1.0 - reveal)
}

/// Multiply a colour's alpha by `a` (0 → 1).
fn fade(c: Color32, a: f32) -> Color32 {
    let a = a.clamp(0.0, 1.0);
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (c.a() as f32 * a) as u8)
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    Color32::from_rgba_unmultiplied(
        l(a.r(), b.r()),
        l(a.g(), b.g()),
        l(a.b(), b.b()),
        l(a.a(), b.a()),
    )
}

fn elide(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let kept: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{kept}…")
    }
}

// ─── Thumbnail generation (off-thread CPU compositor → egui texture) ────────────

#[derive(Clone)]
enum Thumb {
    Pending,
    Ready(TextureHandle),
    Failed,
}

type ThumbResult = (PathBuf, Option<(u32, u32, Vec<u8>)>);

struct Thumbnailer {
    req_tx: Sender<PathBuf>,
    res_rx: Receiver<ThumbResult>,
    state: HashMap<PathBuf, Thumb>,
}

impl Thumbnailer {
    fn new() -> Self {
        let (req_tx, req_rx) = channel::<PathBuf>();
        let (res_tx, res_rx) = channel::<ThumbResult>();
        std::thread::Builder::new()
            .name("photonic-thumbs".into())
            .spawn(move || {
                while let Ok(path) = req_rx.recv() {
                    let out = render_thumb(&path);
                    if res_tx.send((path, out)).is_err() {
                        break;
                    }
                }
            })
            .ok();
        Self {
            req_tx,
            res_rx,
            state: HashMap::new(),
        }
    }

    /// Upload any thumbnails that finished rendering since the last frame.
    fn pump(&mut self, ctx: &egui::Context) {
        while let Ok((path, out)) = self.res_rx.try_recv() {
            let thumb = match out {
                Some((w, h, rgba)) => {
                    let img =
                        egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                    let name = format!("thumb_{}", path.to_string_lossy());
                    Thumb::Ready(ctx.load_texture(name, img, TextureOptions::LINEAR))
                }
                None => Thumb::Failed,
            };
            self.state.insert(path, thumb);
        }
    }

    /// Get the thumbnail for `path`, requesting generation on first sight.
    fn get(&mut self, _ctx: &egui::Context, path: &Path) -> Thumb {
        if let Some(t) = self.state.get(path) {
            return t.clone();
        }
        let _ = self.req_tx.send(path.to_path_buf());
        self.state.insert(path.to_path_buf(), Thumb::Pending);
        Thumb::Pending
    }
}

/// Render a document to a fit-to-artboard RGBA8 thumbnail using the pure-CPU
/// compositor. Returns `(width, height, rgba)` or `None` on any load/parse error.
fn render_thumb(path: &Path) -> Option<(u32, u32, Vec<u8>)> {
    let doc = load_doc(path).ok()?;
    let dw = doc.width.max(1.0);
    let dh = doc.height.max(1.0);
    const MAX_EDGE: f64 = 360.0;
    let scale = (MAX_EDGE / dw.max(dh)).min(2.0);
    let tw = (dw * scale).round().clamp(1.0, 720.0) as u32;
    let th = (dh * scale).round().clamp(1.0, 720.0) as u32;

    let mut view = CanvasView::new(tw, th);
    view.zoom = scale;
    view.pan_x = 0.0;
    view.pan_y = 0.0;

    // Pre-fill with a near-white artboard so previews read as documents.
    let mut buf = vec![0u8; (tw as usize) * (th as usize) * 4];
    for px in buf.chunks_exact_mut(4) {
        px[0] = 250;
        px[1] = 250;
        px[2] = 252;
        px[3] = 255;
    }
    composite_document(&mut buf, tw, th, &doc, &view);
    Some((tw, th, buf))
}

fn load_doc(path: &Path) -> Result<Document, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let content = if ext == "svg" {
        crate::read_svg_file(path)?
    } else {
        std::fs::read_to_string(path).map_err(|e| e.to_string())?
    };
    if ext == "svg" {
        photonic_core::import_svg(&content).map_err(|e| e.to_string())
    } else {
        Document::from_json(&content).map_err(|e| e.to_string())
    }
}

// ─── Recent-docs persistence ──────────────────────────────────────────────────

/// Cross-platform Photonic config directory.
///
/// Single source of truth lives in `photonic_core::crash_dir` (shared with the
/// crash-report subsystem): `%APPDATA%\Photonic` on Windows, then
/// `$XDG_CONFIG_HOME/Photonic` / `~/.config/Photonic` on Linux and macOS.
pub(crate) fn config_dir() -> Option<PathBuf> {
    photonic_core::crash_dir()
}

fn recent_docs_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("recent_docs.json"))
}

fn disk_roots_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("disk_roots.json"))
}

fn load_disk_roots() -> Vec<PathBuf> {
    disk_roots_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_disk_roots(roots: &[PathBuf]) {
    let Some(path) = disk_roots_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(roots) {
        let _ = std::fs::write(path, json);
    }
}

fn load_recent() -> Vec<RecentEntry> {
    recent_docs_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_recent(docs: &[RecentEntry]) {
    let Some(path) = recent_docs_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(docs) {
        let _ = std::fs::write(path, json);
    }
}
