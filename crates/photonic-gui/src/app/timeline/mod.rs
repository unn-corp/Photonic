//! Bottom timeline panel (video-editor-module `04-ui-mode-timeline.md` §2).
//!
//! Module family mirroring the existing `panels/` split-by-concern pattern:
//! `layout` (zoom/scroll mapping), `ruler` (time ruler + playhead + markers),
//! `tracks` (header column), `clips` (culled, batched lane painting), `interact`
//! (pure hit-testing/snapping + drag state), and `ops_bridge` (the sole
//! intent→`ops`→`CommandHistory` sink). Every mutation flows through
//! `ops_bridge`; the panel itself never touches `doc.timeline` directly (the one
//! sanctioned exception, `height_px`, lives in `ops_bridge::set_track_height`).
//!
//! **Documented exception:** `link_selected_clips`/`unlink_selected_clips`
//! below (14-nle-parity gap #8's creation half) call
//! `photonic_core::timeline::ops::{set_clip_prop, unlink_clip}` directly
//! rather than through `ops_bridge.rs` — that file's own `link_group` module
//! note explicitly defers this ("link groups can only be established
//! directly against `TimelineProject`... outside this story's territory"
//! until a story lands to do it), and this story's territory is
//! `{clips.rs, mod.rs, tracks.rs}` only, which doesn't reach `ops_bridge.rs`
//! to add the wrapper. Mirrors the same bypass `tracks.rs::toggle_solo` /
//! `toggle_sync_lock` already establish for the identical constraint. A
//! follow-up pass should promote these into `ops_bridge.rs` to restore the
//! "sole sink" invariant.

pub mod clips;
pub mod interact;
pub mod layout;
pub mod ops_bridge;
pub mod ruler;
pub mod tracks;

pub use layout::TimelineView;

use super::PhotonicApp;
use interact::{DragKind, DragState, Marquee};
use layout::{EDGE_HIT_PX, RULER_HEIGHT_PX, SNAP_THRESHOLD_PX, ZOOM_STEP};
use photonic_core::document::Document;
use photonic_core::history::{Command, CommandHistory};
use photonic_core::timeline::clip::LinkGroupId;
use photonic_core::timeline::{
    ops, ClipId, ClipSource, ClipTiming, FrameRate, Sequence, SequenceId, Tick, TrackId, TrackKind,
};
// Timeline media caches (spec 15 — NLE parity gap 10): the engine-side thumbnail
// and waveform caches this panel feeds to `clips::paint_lane`.
use photonic_core::timeline::{AssetId, AssetSource, MediaPool};
use photonic_video::audio::{build_pyramid, WaveformPyramid};
use photonic_video::decode::scheduler::{PtsKind, SourceParams};
use photonic_video::decode::PixFmt;
use photonic_video::media::{
    cache_dir_for_project, locate, probe_details, DecodeThumbnailSource, FfmpegTools,
    KeyframeIndex, PtsIndex, ThumbnailCache, ThumbnailDecodeSpec, WaveformCache, WaveformSource,
};
use photonic_video::playback::FfmpegPcmSource;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const TOOLBAR_H: f32 = 24.0;
/// Sequence tab strip height above the mini-toolbar (17 G-17).
const SEQ_TABS_H: f32 = 24.0;
/// Navigator / horizontal-scrollbar strip height at the panel bottom (spec 17 G8).
const NAV_H: f32 = 12.0;
/// Height reserved at the bottom of the header column for the "+ Track" menu.
/// The button is bottom-anchored inside it (see [`tracks::draw_add_controls`]),
/// so a button taller than this grows *upwards* instead of being painted over
/// by the navigator strip below.
const TRACK_FOOTER_H: f32 = 30.0;

/// Height a one-row strip of buttons needs at the current UI scale. The old
/// hard-coded 24 px was smaller than a real `egui` button once the user bumped
/// `ui_scale`, so the timeline's Captions/Export buttons wrapped onto a second
/// line and got clipped by the strip.
fn button_strip_height(ui: &egui::Ui) -> f32 {
    let spacing = ui.spacing();
    (spacing.interact_size.y + 2.0 * spacing.button_padding.y + 6.0).max(TOOLBAR_H)
}
const DRAG_ID: &str = "timeline_drag_state";
const MARQUEE_ID: &str = "timeline_marquee";

/// Resolve the drop target for an in-flight track reorder: the nearest *same
/// kind* visible header row to `pointer_y`, by row-center distance. `geom`
/// entries are `(id, kind, index_in_kind, top_y, bot_y)` for the visible rows.
/// Returns `(index_in_kind, top_y)` — the top edge feeds the drop indicator.
///
/// `index_in_kind` is the row's *true* `video_tracks` / `audio_tracks` Vec
/// index, even for the reversed video lane (top row = last Vec index).
fn nearest_drop_row(
    geom: &[(TrackId, TrackKind, usize, f32, f32)],
    kind: TrackKind,
    pointer_y: f32,
) -> Option<(usize, f32)> {
    geom.iter()
        .filter(|g| g.1 == kind)
        .min_by(|a, b| {
            let ca = (a.3 + a.4) * 0.5;
            let cb = (b.3 + b.4) * 0.5;
            (ca - pointer_y).abs().total_cmp(&(cb - pointer_y).abs())
        })
        .map(|g| (g.2, g.3))
}

/// Convert a visual "insert before the target row" drop into the raw Vec index
/// expected by `ops::reorder_track`, which removes the dragged row first.
/// Video and text rows are painted in reverse Vec order; audio rows are not.
fn reorder_target_index(
    kind: TrackKind,
    drag_index: usize,
    drop_index: usize,
    count: usize,
) -> Option<usize> {
    if count == 0 || drag_index >= count || drop_index >= count {
        return None;
    }
    let reversed = matches!(kind, TrackKind::Video | TrackKind::Text);
    let visual_drag = if reversed {
        count - 1 - drag_index
    } else {
        drag_index
    };
    let visual_drop = if reversed {
        count - 1 - drop_index
    } else {
        drop_index
    };

    // The indicator is the target row's top edge, so the dragged row belongs
    // immediately before the target in visual order. The index is in the
    // post-removal visual list because reorder_track removes first.
    let visual_insert = if visual_drag < visual_drop {
        visual_drop - 1
    } else {
        visual_drop
    };
    let remaining = count - 1;
    Some(if reversed {
        remaining - visual_insert
    } else {
        visual_insert
    })
}

/// Place a fixed-position widget without advancing the timeline panel's normal
/// top-down layout cursor. The timeline is a canvas; using [`egui::Ui::put`]
/// directly would make the resizable dock persist the widget's bottom edge
/// (plus item spacing) as its next height.
pub(crate) fn put_fixed(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    widget: impl egui::Widget,
) -> egui::Response {
    let mut fixed_ui = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(
        egui::Layout::centered_and_justified(egui::Direction::TopDown),
    ));
    fixed_ui.add(widget)
}

/// [`put_fixed`] for a small square icon button in a hand-packed strip.
///
/// The track-header row packs seven controls into ~110 px using fixed 18 px
/// slots with 2 px gaps. egui's default button padding is wider than those
/// gaps, so a plain `put_fixed` button drew past its slot and every control
/// visibly overlapped its neighbour. Shrinking the padding *inside the child
/// ui* — never on the shared timeline `Ui`, whose style would then leak into
/// everything drawn afterwards — makes the widget honour the slot it was given.
pub(crate) fn put_icon(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    widget: impl egui::Widget,
) -> egui::Response {
    let mut fixed_ui = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(
        egui::Layout::centered_and_justified(egui::Direction::TopDown),
    ));
    fixed_ui.spacing_mut().button_padding = egui::vec2(1.0, 0.0);
    fixed_ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
    fixed_ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
    fixed_ui.add(widget)
}

/// Set the program-monitor transport intent used after a successful
/// media-pool → timeline drop. The engine bridge turns this into a seek and
/// normal forward playback when the monitor draws later in the frame.
fn start_dropped_media_preview(app: &mut PhotonicApp, at: Tick) {
    app.playhead = at;
    app.monitor_playing = true;
    app.monitor_play_reverse = false;
    app.monitor_play_speed = 1.0;
}

impl PhotonicApp {
    /// The bottom timeline panel's entry point (04 §1.1/§2). Registered by
    /// `app/mod.rs` as a `TopBottomPanel::bottom("timeline")` gated on
    /// `self.mode == AppMode::Video`.
    pub(crate) fn draw_timeline_panel(
        &mut self,
        ui: &mut egui::Ui,
        doc: &mut Document,
        history: &mut CommandHistory,
    ) {
        // Reserve the dock's current content rect once. Fixed-position controls
        // below use `put_fixed`, so they cannot extend this reservation.
        let panel_rect = ui.max_rect();
        ui.set_min_size(panel_rect.size());

        // Invariant: video mode ⇒ a project exists (04 §1.3). The first action on
        // a document with no project creates it lazily below.
        let frame_rate = active_frame_rate(doc);

        let has_sequence = doc
            .timeline
            .as_ref()
            .and_then(|p| p.active_sequence)
            .and_then(|id| doc.timeline.as_ref().and_then(|p| p.sequences.get(&id)))
            .is_some();
        if !has_sequence {
            self.draw_empty_affordance(ui, doc, history, frame_rate);
            return;
        }
        self.sync_sequence_view(doc);
        let seq_id = doc.timeline.as_ref().unwrap().active_sequence.unwrap();

        // Timeline-panel keyboard commands (spec 17 G1/G2/G3) — polled here, the
        // one video-mode surface that owns these editing keys, BEFORE session
        // state is pulled into locals so a playhead/selection change lands this
        // frame. Focus-gated inside the helper.
        let ctx = ui.ctx().clone();
        self.handle_timeline_shortcuts(&ctx, doc, history);

        // Pull session state into locals so helper fns don't fight the `&mut self`
        // borrow; written back at the end.
        let mut view = self.timeline_view;
        let mut playhead = self.playhead;
        let mut selection = std::mem::take(&mut self.timeline_selection);
        let mut snap = self.timeline_snap_enabled;
        let razor = self.timeline_razor_active;
        // Source-patch target tracks (spec 17 G6) — mutated by the track headers'
        // patch buttons this frame, written back at the end.
        let mut target_video = self.target_video_track;
        let mut target_audio = self.target_audio_track;

        // Timeline media caches (spec 15): construct once, then take them out of
        // `self` for the frame — mirroring the `selection` take above — so the
        // paint loop can hold `&caches` while `self` is still mutated for the
        // write-back at the end. Restored just before returning. Refreshed each
        // frame from the active document's media pool so the background resolver
        // closures see the current assets without borrowing the document.
        if self.timeline_media.is_none() {
            self.timeline_media = Some(TimelineMediaCaches::new(
                self.current_file.as_deref(),
                self.active_tab,
            ));
        }
        let mut media_caches = self.timeline_media.take();
        if let Some(caches) = media_caches.as_mut() {
            caches.refresh(
                self.current_file.as_deref(),
                self.active_tab,
                &doc.timeline.as_ref().unwrap().media,
            );
        }

        let full = ui.max_rect();
        // Sequence tabs (17 G-17) sit above the zoom/snap mini-toolbar.
        let tabs_rect = egui::Rect::from_min_size(full.min, egui::vec2(full.width(), SEQ_TABS_H));
        {
            let mut open_tabs = std::mem::take(&mut self.open_sequence_tabs);
            let mut breadcrumbs = std::mem::take(&mut self.nested_sequence_breadcrumbs);
            crate::panels::video::seq_tabs::draw_seq_tabs(
                ui,
                tabs_rect,
                doc,
                history,
                &mut open_tabs,
                &mut breadcrumbs,
            );
            self.open_sequence_tabs = open_tabs;
            self.nested_sequence_breadcrumbs = breadcrumbs;
        }
        if doc.timeline.as_ref().and_then(|p| p.active_sequence) != Some(seq_id) {
            self.timeline_view = view;
            self.playhead = playhead;
            self.timeline_selection = selection;
            self.sync_sequence_view(doc);
            view = self.timeline_view;
            playhead = self.playhead;
            selection = std::mem::take(&mut self.timeline_selection);
            target_video = None;
            target_audio = None;
        }
        // Active sequence may have changed via tab click — re-resolve.
        let seq_id = doc
            .timeline
            .as_ref()
            .and_then(|p| p.active_sequence)
            .unwrap_or(seq_id);

        let frame_rate = active_frame_rate(doc);
        let toolbar_rect = egui::Rect::from_min_size(
            egui::pos2(full.left(), tabs_rect.bottom()),
            egui::vec2(full.width(), button_strip_height(ui)),
        );
        self.draw_mini_toolbar(ui, toolbar_rect, doc, seq_id, &mut view, &mut snap);

        let below_full =
            egui::Rect::from_min_max(egui::pos2(full.left(), toolbar_rect.bottom()), full.max);
        // Reserve a strip at the bottom for the navigator / horizontal scrollbar
        // (spec 17 G8); everything else lays out above it.
        let nav_rect = egui::Rect::from_min_max(
            egui::pos2(
                below_full.left(),
                (below_full.bottom() - NAV_H).max(below_full.top()),
            ),
            below_full.max,
        );
        let below = egui::Rect::from_min_max(
            below_full.min,
            egui::pos2(below_full.right(), nav_rect.top()),
        );
        let header_w = view.header_width_px;
        let header_col = egui::Rect::from_min_max(
            below.min,
            egui::pos2(below.left() + header_w, below.bottom()),
        );
        let lane_col =
            egui::Rect::from_min_max(egui::pos2(header_col.right(), below.top()), below.max);
        let lane_left = lane_col.left();
        let ruler_rect =
            egui::Rect::from_min_size(lane_col.min, egui::vec2(lane_col.width(), RULER_HEIGHT_PX));
        let lanes_rect = egui::Rect::from_min_max(
            egui::pos2(lane_col.left(), ruler_rect.bottom()),
            lane_col.max,
        );
        // Track headers begin below the ruler, exactly where their matching
        // lane rows begin. Keeping the two origins identical makes every row
        // divider a continuous line across the timeline.
        let header_rows_rect = egui::Rect::from_min_max(
            egui::pos2(header_col.left(), lanes_rect.top()),
            header_col.max,
        );
        view.last_lane_width_px = lanes_rect.width();

        // ── Column splitter (header width) ──────────────────────────────────
        let splitter = egui::Rect::from_min_max(
            egui::pos2(header_col.right() - 2.0, below.top()),
            egui::pos2(header_col.right() + 2.0, below.bottom()),
        );
        let sresp = ui.interact(splitter, ui.id().with("tl_splitter"), egui::Sense::drag());
        if sresp.hovered() || sresp.dragged() {
            ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::ResizeHorizontal);
        }
        if sresp.dragged() {
            view.set_header_width(header_w + sresp.drag_delta().x);
        }

        // ── Scroll / zoom over the lane area ────────────────────────────────
        handle_scroll_zoom(ui, full, lane_left, &mut view);

        // K-A10: fixed-playhead mode — keep the playhead centred and scroll
        // the timeline underneath during transport (view state only).
        if view.fixed_playhead && self.monitor_playing {
            view.center_on_playhead(playhead, lanes_rect.width());
        }

        // ── Rows: draw headers + lanes, collect hit rects ───────────────────
        let seq = doc
            .timeline
            .as_ref()
            .unwrap()
            .sequences
            .get(&seq_id)
            .unwrap();
        let rows = tracks::track_rows(seq);
        let total_h: f32 = rows.iter().map(|r| r.height).sum();
        let max_scroll = (total_h - lanes_rect.height()).max(0.0);
        view.track_scroll_px = view.track_scroll_px.clamp(0.0, max_scroll);

        let colors = lane_colors(ui);
        let mut hits: Vec<interact::HitCandidate> = Vec::new();
        // Global per-frame egui-texture registration budget for clip
        // thumbnails (spec 15 §4), threaded through every `paint_lane` call
        // below so the cap is shared across all visible lanes, not per-lane.
        let mut thumb_budget = clips::ThumbnailBudget::new(clips::DEFAULT_THUMBNAIL_BUDGET);
        {
            // Clip the painter to the lanes rect so scrolled rows don't overflow.
            let lane_painter = ui.painter_at(lanes_rect);
            let mut y = lanes_rect.top() - view.track_scroll_px;
            for row in &rows {
                let row_top = y;
                let row_bot = y + row.height;
                y = row_bot;
                // Vertical cull.
                if row_bot < lanes_rect.top() || row_top > lanes_rect.bottom() {
                    continue;
                }
                let lane_rect = egui::Rect::from_min_max(
                    egui::pos2(lanes_rect.left(), row_top),
                    egui::pos2(lanes_rect.right(), row_bot),
                );
                let seq = doc
                    .timeline
                    .as_ref()
                    .unwrap()
                    .sequences
                    .get(&seq_id)
                    .unwrap();
                if let Some(track) = seq.track(row.id) {
                    // Lane separator.
                    lane_painter.line_segment(
                        [
                            egui::pos2(lanes_rect.left(), row_bot),
                            egui::pos2(lanes_rect.right(), row_bot),
                        ],
                        egui::Stroke::new(1.0, colors.clip_stroke),
                    );
                    let painted = clips::paint_lane(
                        &lane_painter,
                        &view,
                        track,
                        lane_rect,
                        &selection,
                        &colors,
                        // Live caches (spec 15): thumbnails sampled across video
                        // lanes, waveforms across audio lanes. A cache miss, no
                        // ffmpeg, or a non-file asset leaves the flat fill —
                        // `paint_lane` handles the per-cache `None` gracefully.
                        media_caches.as_ref().map(|c| &c.thumbnails),
                        media_caches.as_ref().map(|c| &c.waveforms),
                        &mut thumb_budget,
                    );
                    for pc in painted {
                        // Automation-lane indicator (04 §4.1): keyframe diamonds
                        // along an animated clip's body, painted by the keyframe
                        // editor module so the two surfaces stay in sync.
                        if let Some(clip) = track.clips.iter().find(|c| c.id == pc.clip) {
                            crate::panels::video::keyframe_editor::paint_clip_automation(
                                &lane_painter,
                                &view,
                                lanes_rect.left(),
                                clip,
                                pc.rect,
                                colors.selected_stroke,
                            );
                        }
                        hits.push(interact::HitCandidate {
                            track: row.id,
                            clip: pc.clip,
                            rect: pc.rect,
                            locked: row.locked,
                        });
                    }
                }
            }
        }

        // Cache workers deliberately never wake the GUI thread directly. While
        // a visible thumbnail or waveform is pending, ask egui for one modest
        // follow-up frame; this keeps the event loop idle otherwise and avoids
        // the old unconditional 60 Hz poll.
        if media_caches
            .as_ref()
            .is_some_and(TimelineMediaCaches::has_pending)
        {
            ui.ctx()
                // Thumb/waveform cache fill: 50ms was fine; 33ms ≈ 30 Hz keeps
                // strips snappy on Linux/Windows without full-speed repaint spam.
                .request_repaint_after(std::time::Duration::from_millis(33));
        }

        // ── Track headers (drawn after lanes so widgets sit on top) ─────────
        {
            let drag_key = egui::Id::new(("track_reorder_active", seq_id));
            let mut dragging: Option<TrackId> = ui
                .data(|d| d.get_temp::<Option<TrackId>>(drag_key))
                .flatten();
            // (id, kind, index_in_kind, top_y, bot_y) for visible header rows —
            // used to resolve a drop position after the loop.
            let mut geom: Vec<(TrackId, TrackKind, usize, f32, f32)> = Vec::new();
            let mut y = header_rows_rect.top() - view.track_scroll_px;
            for row in &rows {
                let row_top = y;
                let row_bot = y + row.height;
                y = row_bot;
                if row_bot < header_rows_rect.top()
                    || row_top > header_rows_rect.bottom() - TRACK_FOOTER_H
                {
                    continue;
                }
                let hrect = egui::Rect::from_min_max(
                    egui::pos2(header_col.left(), row_top),
                    egui::pos2(
                        header_col.right(),
                        row_bot.min(header_rows_rect.bottom() - TRACK_FOOTER_H),
                    ),
                );
                // Drag-to-reorder: interact on the header background BEFORE the
                // header's own buttons (drawn inside `draw_header`) so the buttons
                // sit on top and still take their clicks; a drag on empty header
                // space reorders the track within its lane.
                let resp = ui.interact(
                    hrect,
                    ui.id().with(("track_reorder", row.id)),
                    egui::Sense::drag(),
                );
                if resp.drag_started() {
                    dragging = Some(row.id);
                    ui.data_mut(|d| d.insert_temp(drag_key, Some(row.id)));
                }
                geom.push((
                    row.id,
                    row.kind,
                    row.index_in_kind,
                    hrect.top(),
                    hrect.bottom(),
                ));
                // Source-patch target for this row's kind (spec 17 G6): the header
                // draws a patch button + highlight and toggles it on click.
                let target = match row.kind {
                    TrackKind::Video | TrackKind::Text => &mut target_video,
                    TrackKind::Audio => &mut target_audio,
                };
                tracks::draw_header(ui, hrect, doc, history, seq_id, *row, target);
            }
            // Resolve a drag in progress: highlight the drop slot, and on release
            // reorder to the nearest same-kind row under the pointer.
            //
            // `index_in_kind` is the raw Vec index, while the drop indicator is a
            // visual insertion boundary. Convert between those coordinate systems
            // before the remove-then-insert operation.
            if let Some(drag_id) = dragging {
                // The primary button being held is the single source of truth for
                // "drag still live". It is a level, not the one-frame
                // `any_released()` edge: if the release lands on a frame this panel
                // did not lay out, an edge check would miss it and leave `drag_key`
                // set forever, pinning `request_repaint` every frame. A level check
                // can never get stuck that way.
                let still_held = ui.input(|i| i.pointer.primary_down());
                // `geom` holds only the *visible* header rows; if the dragged row
                // is absent it has been scrolled out of view.
                let row = geom.iter().find(|g| g.0 == drag_id).copied();
                if let Some((_, dkind, didx, _, _)) = row {
                    let pointer_y = ui.input(|i| {
                        i.pointer
                            .hover_pos()
                            .or(i.pointer.interact_pos())
                            .map(|p| p.y)
                    });
                    let nearest = pointer_y.and_then(|py| nearest_drop_row(&geom, dkind, py));
                    if let Some((drop_idx, top)) = nearest {
                        if still_held {
                            // Drop indicator — only while the drag is live.
                            ui.painter().line_segment(
                                [
                                    egui::pos2(header_col.left(), top),
                                    egui::pos2(header_col.right(), top),
                                ],
                                egui::Stroke::new(2.0, ui.visuals().selection.stroke.color),
                            );
                        } else if drop_idx != didx {
                            // Released over a different same-kind row → reorder.
                            if let Some(target) = reorder_target_index(
                                dkind,
                                didx,
                                drop_idx,
                                rows.iter()
                                    .find(|candidate| candidate.kind == dkind)
                                    .map(|candidate| candidate.count_in_kind)
                                    .unwrap_or(0),
                            ) {
                                ops_bridge::move_track(doc, history, seq_id, drag_id, target);
                            }
                        }
                    }
                }
                // Clear the temp key on EVERY end-of-drag path so it can never
                // stick and pin repaints: pointer released (button no longer down),
                // the dragged row scrolled out of view, or a press that never
                // became a real drag. Only a still-held, still-visible drag keeps
                // the state alive (and keeps requesting repaints for the indicator).
                if !still_held || row.is_none() {
                    ui.data_mut(|d| d.insert_temp::<Option<TrackId>>(drag_key, None));
                } else {
                    ui.ctx().request_repaint();
                }
            }
            // Add-track controls pinned to the header column's bottom.
            let footer = egui::Rect::from_min_max(
                egui::pos2(
                    header_rows_rect.left(),
                    header_rows_rect.bottom() - TRACK_FOOTER_H,
                ),
                header_rows_rect.max,
            );
            tracks::draw_add_controls(ui, footer, doc, history, seq_id);
        }

        // Transcript selection is a view overlay, independent of clip selection
        // and history. The drawer also underlines words from selected clips.
        if let Some((start, end)) = crate::panels::video::transcript::selected_range(ui.ctx(), doc)
        {
            let left = view
                .tick_to_x(start, lanes_rect.left())
                .max(lanes_rect.left());
            let right = view
                .tick_to_x(end, lanes_rect.left())
                .min(lanes_rect.right());
            if right > left {
                ui.painter_at(lanes_rect).rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(left, lanes_rect.top()),
                        egui::pos2(right, lanes_rect.bottom()),
                    ),
                    0.0,
                    colors.selected_stroke.linear_multiply(0.12),
                );
            }
        }

        // ── Ruler + playhead ────────────────────────────────────────────────
        ruler::draw_ruler(
            ui,
            doc,
            history,
            seq_id,
            &view,
            ruler_rect,
            lane_left,
            &mut playhead,
        );

        if let Some(sequence) = doc.timeline.as_ref().and_then(|p| p.sequences.get(&seq_id)) {
            let status = self
                .engine
                .as_ref()
                .map(|engine| engine.session().preview_status());
            ruler::draw_preview_strip(ui, &view, ruler_rect, lane_left, sequence, status.as_ref());
        }

        // ── Clip interaction (select / drag / marquee / context) ────────────
        let content_rect = egui::Rect::from_min_max(ruler_rect.min, lanes_rect.max);
        // Replace With Clip (G-5): resolve what an armed Match-Frame source or
        // a media-pool selection would swap onto a right-clicked clip, so the
        // context menu can offer/gate the entry this frame.
        let replace_candidate = replace_source_candidate(
            doc,
            self.pending_source.as_ref(),
            self.media_pool_ui.selected,
        );
        let mut open_edit_duration: Option<(TrackId, ClipId)> = None;
        self_interact(
            ui,
            doc,
            history,
            seq_id,
            &mut view,
            &mut playhead,
            &mut selection,
            snap,
            razor,
            lane_left,
            frame_rate,
            lanes_rect,
            &rows,
            &hits,
            replace_candidate,
            &mut open_edit_duration,
        );
        if let Some((track, clip)) = open_edit_duration {
            self.edit_duration_dialog =
                crate::panels::video::duration_dialog::EditDurationDialog::seed(
                    doc, seq_id, track, clip,
                );
        }
        // K-A7: context-menu "Grab item" seeds a session via egui temp data.
        if let Some((gseq, gtrack, gclip)) = ui.data(|d| {
            d.get_temp::<(SequenceId, TrackId, ClipId)>(egui::Id::new("k_a7_grab_request"))
        }) {
            ui.data_mut(|d| {
                d.remove::<(SequenceId, TrackId, ClipId)>(egui::Id::new("k_a7_grab_request"));
            });
            if let Some(seq) = doc.timeline.as_ref().and_then(|p| p.sequences.get(&gseq)) {
                if let Some(session) = interact::GrabSession::seed(seq, gseq, gtrack, gclip) {
                    selection = vec![gclip];
                    self.timeline_grab = Some(session);
                }
            }
        }

        // K-B14 freeze-frame request from the clip context menu.
        if let Some((fseq, ftrack, fclip)) = ui.data(|d| {
            d.get_temp::<(SequenceId, TrackId, ClipId)>(egui::Id::new("k_b14_freeze_request"))
        }) {
            ui.data_mut(|d| {
                d.remove::<(SequenceId, TrackId, ClipId)>(egui::Id::new("k_b14_freeze_request"));
            });
            let at_rel = doc
                .timeline
                .as_ref()
                .and_then(|p| p.sequences.get(&fseq))
                .and_then(|s| s.track(ftrack))
                .and_then(|t| t.clips.iter().find(|c| c.id == fclip))
                .map(|c| {
                    // Playhead → clip-relative; outside the clip freezes the
                    // first frame (clamp inside freeze_frame handles the rest).
                    Tick((playhead.0 - c.start.0).max(0))
                })
                .unwrap_or(Tick::ZERO);
            ops_bridge::freeze_frame(doc, history, fseq, ftrack, fclip, at_rel);
        }

        // ── Media-pool asset drop (05 §2) ───────────────────────────────────
        // A drag started in the media pool drawer carries an `AssetDrag`
        // payload; dropping it over a lane inserts a clip there via the
        // ops_bridge path (kind-checked, one undo step). While hovering, a
        // frame-snapped insertion caret previews the landing tick.
        let asset_payload =
            egui::DragAndDrop::payload::<crate::panels::media_pool::AssetDrag>(ui.ctx());
        if let (Some(payload), Some(pos)) = (asset_payload, ui.ctx().pointer_latest_pos()) {
            if lanes_rect.contains(pos) {
                let tpf = frame_rate.ticks_per_frame().0.max(1);
                let mut at = view.x_to_tick(pos.x, lane_left).0.max(0);
                if snap {
                    at = (at / tpf) * tpf;
                }
                let at = Tick(at);
                // y → row under the cursor (same walk as the paint loop).
                // Locked tracks reject drops too (14-nle-parity QW-2
                // watch-out): `target` stays `None` over a locked lane, so no
                // caret shows and the release below is a no-op there.
                let mut yy = lanes_rect.top() - view.track_scroll_px;
                let mut target: Option<TrackId> = None;
                for row in &rows {
                    let (top, bot) = (yy, yy + row.height);
                    yy = bot;
                    if pos.y >= top && pos.y < bot {
                        if !row.locked {
                            target = Some(row.id);
                        }
                        break;
                    }
                }
                // Hover caret — suppressed over a locked lane so the absent
                // caret itself signals "can't drop here".
                if target.is_some() {
                    let x = view.tick_to_x(at, lane_left);
                    ui.painter_at(lanes_rect).line_segment(
                        [
                            egui::pos2(x, lanes_rect.top()),
                            egui::pos2(x, lanes_rect.bottom()),
                        ],
                        egui::Stroke::new(2.0, colors.selected_stroke.gamma_multiply(0.8)),
                    );
                }
                if ui.input(|i| i.pointer.any_released()) {
                    egui::DragAndDrop::clear_payload(ui.ctx());
                    // Prefer the lane under the cursor; if there is none (empty
                    // timeline / locked lane) or it has no room at `at`, fall back
                    // to first-fit, which adds a track if needed — so a drop
                    // always lands rather than silently doing nothing.
                    let mut landed = match target {
                        Some(track) => ops_bridge::insert_asset_clip(
                            doc,
                            history,
                            seq_id,
                            track,
                            payload.asset,
                            at,
                        ),
                        None => false,
                    };
                    if !landed {
                        landed =
                            ops_bridge::insert_asset_at_first_fit(doc, history, payload.asset, at);
                    }
                    if landed {
                        // A media-pool drop is also a preview gesture: park the
                        // program monitor at the new clip's first frame and
                        // start normal forward transport immediately. The
                        // monitor later in this frame turns this intent into a
                        // Seek + Play for the engine.
                        start_dropped_media_preview(self, at);
                        // `playhead` is borrowed into a local for this render
                        // pass and written back below, so update both copies.
                        playhead = at;
                        if let Some(bridge) = self.engine.as_mut() {
                            // The regular per-frame sync ran before UI input;
                            // sync once more so this just-dropped clip is
                            // available to the Seek/Play emitted below.
                            bridge.sync_document(doc, history);
                        }
                        ui.ctx().request_repaint();
                    }
                }
            }
        }

        if let Some(trim) = self.precision_trim.as_ref() {
            if let Some(cut) = doc
                .timeline
                .as_ref()
                .and_then(|p| p.sequences.get(&trim.target.sequence))
                .and_then(|s| s.track(trim.target.track))
                .and_then(|t| t.clips.iter().find(|c| c.id == trim.target.incoming))
                .map(|c| c.start)
            {
                let x = view.tick_to_x(cut, lane_left);
                ui.painter_at(lanes_rect).vline(
                    x,
                    lanes_rect.y_range(),
                    egui::Stroke::new(3., ui.visuals().selection.stroke.color),
                );
            }
        }
        // Playhead line over everything (drawn last).
        ruler::draw_playhead_line(
            &ui.painter_at(content_rect),
            &view,
            playhead,
            content_rect,
            lane_left,
            colors.selected_stroke,
        );

        // ── Navigator / horizontal scrollbar (spec 17 G8) ───────────────────
        // A thumb over the whole sequence extent; drag to pan, drag an end to
        // zoom, click the trough to jump. Writes `view.scroll_ticks`/zoom (the
        // same fields `handle_scroll_zoom` drives) — mutated after painting, so
        // the change lands next frame like every other scroll gesture.
        if let Some(seq) = doc.timeline.as_ref().and_then(|p| p.sequences.get(&seq_id)) {
            draw_navigator(ui, nav_rect, lane_left, seq, &mut view);
        }

        // Keyframe / curve editor (04 §4.1, 01 §6): a floating editor that
        // auto-targets the selected clip. Invoked from here — the one video-mode
        // draw path that already holds `doc`, `history`, the playhead, and the
        // live selection — so it needs no `app/mod.rs` call-site wiring. Every
        // edit flows through a pure core keyframe op → `CommandHistory`.
        crate::panels::video::keyframe_editor::draw_window(
            ui.ctx(),
            doc,
            history,
            &mut self.keyframe_editor_target,
            &selection,
            playhead,
        );

        // K-A7: paint grab ghost + status strip while a keyboard grab is open.
        if let Some(session) = self.timeline_grab.as_ref() {
            if let Some(seq) = doc.timeline.as_ref().and_then(|p| p.sequences.get(&seq_id)) {
                paint_grab_ghost(ui, seq, session, &view, lane_left, lanes_rect, &rows);
            }
            // Lightweight status cue above the navigator.
            let status = format!(
                "Grab: ←→ frame{}  ↑↓ track  Enter commit  Esc cancel",
                if session.is_dirty() { " · dirty" } else { "" }
            );
            ui.painter().text(
                egui::pos2(lanes_rect.left() + 6.0, lanes_rect.top() + 4.0),
                egui::Align2::LEFT_TOP,
                status,
                egui::FontId::proportional(11.0),
                ui.visuals().strong_text_color(),
            );
        }

        // Write session state back.
        self.timeline_view = view;
        self.playhead = playhead;
        self.timeline_selection = selection;
        self.target_video_track = target_video;
        self.target_audio_track = target_audio;
        if snap != self.timeline_snap_enabled {
            self.timeline_snap_enabled = snap;
            self.prefs.timeline_snap_enabled = snap;
        }
        // Restore the media caches taken at the top of the frame.
        self.timeline_media = media_caches;
        if let Some((clip, precision, cut)) = ui.data_mut(|data| {
            data.remove_temp::<(ClipId, bool, Option<Tick>)>(egui::Id::new(
                "nested_precision_request",
            ))
        }) {
            self.timeline_selection = vec![clip];
            if let Some(cut) = cut {
                self.playhead = cut;
            }
            if precision {
                self.toggle_precision_trim(doc, history);
            } else {
                self.enter_nested_sequence(doc, history);
            }
        }
    }
}

/// K-A7 ghost outline at the grab session's preview placement.
fn paint_grab_ghost(
    ui: &egui::Ui,
    seq: &Sequence,
    session: &interact::GrabSession,
    view: &TimelineView,
    lane_left: f32,
    lanes_rect: egui::Rect,
    rows: &[tracks::TrackRow],
) {
    let Some(track_rect) = lane_rect_for_track(rows, lanes_rect, view, session.preview_track)
    else {
        return;
    };
    // Prefer live duration if the clip still exists (speed edits mid-grab are rare).
    let duration = seq
        .track(session.track)
        .and_then(|t| t.clips.iter().find(|c| c.id == session.clip))
        .map(|c| c.duration)
        .unwrap_or(session.duration);
    let accent = ui.visuals().selection.stroke.color;
    let painter = ui.painter_at(lanes_rect);
    let x0 = view.tick_to_x(session.preview_start, lane_left);
    let x1 = view.tick_to_x(session.preview_start + duration, lane_left);
    let ghost = egui::Rect::from_min_max(
        egui::pos2(x0.min(x1), track_rect.top() + 2.0),
        egui::pos2(x0.max(x1), track_rect.bottom() - 2.0),
    );
    painter.rect(
        ghost,
        egui::Rounding::same(3.0),
        accent.gamma_multiply(0.18),
        egui::Stroke::new(2.0, accent),
    );
    // Vertical guide at the preview in-point.
    painter.line_segment(
        [
            egui::pos2(x0, track_rect.top()),
            egui::pos2(x0, track_rect.bottom()),
        ],
        egui::Stroke::new(1.0, accent.gamma_multiply(0.7)),
    );
}

/// The `pub(crate)` timeline command entry points (04 §5). The mode-switch story
/// wires command dispatch (`video.*` `CommandId`s) to these — they are unused
/// until then, hence the `dead_code` allowance. Exact names are load-bearing:
/// `timeline_zoom_in`/`timeline_zoom_out`/`timeline_zoom_fit`/
/// `timeline_toggle_snap`/`timeline_toggle_fixed_playhead`/
/// `timeline_playhead_home`/`timeline_playhead_end`/
/// `timeline_prev_edit_point`/`timeline_next_edit_point`/
/// `timeline_prev_snap`/`timeline_next_snap`/
/// `timeline_split_at_playhead`.
#[allow(dead_code)]
impl PhotonicApp {
    /// Zoom the timeline in one step, anchored on the playhead (`+`, `video.zoom_in`).
    pub(crate) fn timeline_zoom_in(&mut self) {
        self.timeline_view
            .zoom_around(ZOOM_STEP, self.playhead, 0.0);
    }

    /// Zoom out one step (`-`, `video.zoom_out`).
    pub(crate) fn timeline_zoom_out(&mut self) {
        self.timeline_view
            .zoom_around(1.0 / ZOOM_STEP, self.playhead, 0.0);
    }

    /// Zoom to fit the active sequence's content (`Shift+Z`, `video.zoom_fit`).
    pub(crate) fn timeline_zoom_fit(&mut self, doc: &Document) {
        let extent = active_sequence(doc)
            .map(|s| s.content_end())
            .unwrap_or(Tick::ZERO);
        let w = self.timeline_view.last_lane_width_px.max(1.0);
        self.timeline_view.fit(extent, w);
    }

    /// Toggle snapping (`N`, `video.toggle_snap`).
    pub(crate) fn timeline_toggle_snap(&mut self) {
        self.timeline_snap_enabled = !self.timeline_snap_enabled;
        self.prefs.timeline_snap_enabled = self.timeline_snap_enabled;
    }

    /// Toggle fixed-playhead mode (`video.toggle_fixed_playhead`, K-A10).
    /// View state only — zero undo units.
    pub(crate) fn timeline_toggle_fixed_playhead(&mut self) {
        self.timeline_view.fixed_playhead = !self.timeline_view.fixed_playhead;
    }

    /// Move the playhead to the sequence start (`Home`, `video.playhead_home`).
    pub(crate) fn timeline_playhead_home(&mut self) {
        self.playhead = Tick::ZERO;
    }

    /// Move the playhead to the sequence end (`End`, `video.playhead_end`).
    pub(crate) fn timeline_playhead_end(&mut self, doc: &Document) {
        if let Some(s) = active_sequence(doc) {
            self.playhead = s.content_end();
        }
    }

    /// Jump to the previous edit point (`Shift+←`, `video.prev_edit_point`).
    pub(crate) fn timeline_prev_edit_point(&mut self, doc: &Document) {
        if let Some(s) = active_sequence(doc) {
            let pts = edit_points(s);
            if let Some(prev) = pts.iter().rev().find(|t| **t < self.playhead) {
                self.playhead = *prev;
            }
        }
    }

    /// Jump to the next edit point (`Shift+→`, `video.next_edit_point`).
    pub(crate) fn timeline_next_edit_point(&mut self, doc: &Document) {
        if let Some(s) = active_sequence(doc) {
            let pts = edit_points(s);
            if let Some(next) = pts.iter().find(|t| **t > self.playhead) {
                self.playhead = *next;
            }
        }
    }

    /// Jump to the previous snap point (`Alt+←`, `video.prev_snap`, 26 K-A4).
    ///
    /// Snap *navigation* walks the same target set a drag snaps to
    /// ([`interact::snap_points`]) — a superset of the edit points
    /// `timeline_prev_edit_point` walks, since it also includes zone in/out,
    /// clip markers and keyframes. It moves only the playhead, so there is
    /// nothing to undo.
    pub(crate) fn timeline_prev_snap(&mut self, doc: &Document) {
        if let Some(s) = active_sequence(doc) {
            let pts = interact::snap_points(s);
            if let Some(prev) = pts.iter().rev().find(|t| **t < self.playhead) {
                self.playhead = *prev;
            }
        }
    }

    /// Jump to the next snap point (`Alt+→`, `video.next_snap`, 26 K-A4).
    pub(crate) fn timeline_next_snap(&mut self, doc: &Document) {
        if let Some(s) = active_sequence(doc) {
            let pts = interact::snap_points(s);
            if let Some(next) = pts.iter().find(|t| **t > self.playhead) {
                self.playhead = *next;
            }
        }
    }

    /// Split the selected clip(s) at the playhead (`S`, `video.split_at_playhead`).
    pub(crate) fn timeline_split_at_playhead(
        &mut self,
        doc: &mut Document,
        history: &mut CommandHistory,
    ) {
        let Some(seq_id) = doc.timeline.as_ref().and_then(|p| p.active_sequence) else {
            return;
        };
        let at = self.playhead;
        // Collect (track, clip) targets: selected clips the playhead is strictly
        // inside. If nothing selected, split whatever clip is under the playhead.
        let mut targets: Vec<(TrackId, ClipId)> = Vec::new();
        if let Some(s) = doc.timeline.as_ref().and_then(|p| p.sequences.get(&seq_id)) {
            for t in s.tracks() {
                // Locked tracks reject the split too (14-nle-parity QW-2) —
                // this is a keyboard/command path, not `hit_at`-gated, so it
                // needs its own guard.
                if t.locked {
                    continue;
                }
                for c in &t.clips {
                    let inside = at > c.start && at < c.end();
                    let hit = self.timeline_selection.contains(&c.id)
                        || self.timeline_selection.is_empty();
                    if inside && hit {
                        targets.push((t.id, c.id));
                    }
                }
            }
        }
        let mut did = false;
        for (track, clip) in targets {
            if ops_bridge::split(doc, history, seq_id, track, clip, at) {
                did = true;
            }
        }
        // Proposal 213 coach: Import → Split → Export.
        if did && !self.prefs.video_coach_dismissed && self.prefs.video_coach_step == 1 {
            self.prefs.video_coach_step = 2;
            self.prefs.save();
        }
    }
}

// ── Free helpers ────────────────────────────────────────────────────────────

fn active_frame_rate(doc: &Document) -> FrameRate {
    doc.timeline
        .as_ref()
        .map(|p| {
            p.active_sequence
                .and_then(|id| p.sequences.get(&id))
                .map(|s| s.frame_rate)
                .unwrap_or(p.settings.default_frame_rate)
        })
        .unwrap_or(FrameRate::FPS_30)
}

#[allow(dead_code)] // used by the not-yet-wired command methods above
fn active_sequence(doc: &Document) -> Option<&Sequence> {
    let p = doc.timeline.as_ref()?;
    p.active_sequence.and_then(|id| p.sequences.get(&id))
}

/// All edit points (clip edges + markers) of a sequence, sorted/deduped.
#[allow(dead_code)] // used by the not-yet-wired prev/next edit-point commands
fn edit_points(seq: &Sequence) -> Vec<Tick> {
    let mut pts = vec![Tick::ZERO];
    for t in seq.tracks() {
        for c in &t.clips {
            pts.push(c.start);
            pts.push(c.end());
        }
    }
    for m in &seq.markers {
        pts.push(m.at);
    }
    pts.sort();
    pts.dedup();
    pts
}

/// Empty-project affordance (04 §1.3): a hint + Add Track buttons that create the
/// project (and first sequence) lazily on first click.
impl PhotonicApp {
    /// CapCut-class empty timeline hero (proposal 213). Primary CTA = Import.
    fn draw_empty_affordance(
        &mut self,
        ui: &mut egui::Ui,
        doc: &mut Document,
        history: &mut CommandHistory,
        frame_rate: FrameRate,
    ) {
        use crate::panels::DrawerGroup;
        use egui_phosphor::regular as ph;

        ui.vertical_centered(|ui| {
            ui.add_space(24.0);
            ui.label(
                egui::RichText::new("Start a social cut")
                    .strong()
                    .size(18.0),
            );
            ui.add_space(4.0);
            ui.weak("Import a clip, cut it, caption it, export Social 9:16.");
            ui.add_space(14.0);

            // Primary CTA — Import media (opens OS picker + seeds project).
            let import = ui.add(
                egui::Button::new(format!("{}  Import media", ph::UPLOAD_SIMPLE))
                    .min_size(egui::vec2(180.0, 32.0)),
            );
            if import
                .on_hover_text("Pick video/audio files — they land in the media pool")
                .clicked()
            {
                self.open_drawer = Some(DrawerGroup::MediaPool);
                self.prefs.open_drawer = Some(DrawerGroup::MediaPool);
                // Inline import (blocking rfd) — same path as Media panel.
                self.import_media_files(doc, history, None);
            }

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let avail = ui.available_width();
                ui.add_space((avail - 340.0).max(0.0) * 0.5);
                if ui
                    .button("Add video track")
                    .on_hover_text("Create an empty video track")
                    .clicked()
                {
                    if let Some(seq) =
                        ops_bridge::ensure_project_and_sequence(doc, history, frame_rate)
                    {
                        ops_bridge::add_track(doc, history, seq, TrackKind::Video);
                    }
                }
                if ui.button("Add audio track").clicked() {
                    if let Some(seq) =
                        ops_bridge::ensure_project_and_sequence(doc, history, frame_rate)
                    {
                        ops_bridge::add_track(doc, history, seq, TrackKind::Audio);
                    }
                }
            });

            // One-click social aspect seeds (creates project + format).
            ui.add_space(12.0);
            ui.weak("Quick format");
            ui.horizontal(|ui| {
                let avail = ui.available_width();
                ui.add_space((avail - 280.0).max(0.0) * 0.5);
                for &(label, name, w, h) in &[
                    ("Social 9:16", "9:16", 1080u32, 1920u32),
                    ("Social 16:9", "16:9", 1920, 1080),
                    ("1:1", "1:1", 1080, 1080),
                ] {
                    if ui
                        .selectable_label(false, label)
                        .on_hover_text(format!("Start a {w}×{h} sequence"))
                        .clicked()
                    {
                        if let Some(seq) =
                            ops_bridge::ensure_project_and_sequence(doc, history, frame_rate)
                        {
                            ops_bridge::add_track(doc, history, seq, TrackKind::Video);
                            ops_bridge::switch_to_aspect(history, doc, seq, name, w, h);
                        }
                    }
                }
            });

            ui.add_space(10.0);
            ui.weak(
                "Tip: drop files on the window · M marker · B bookmark · S split · ? shortcuts",
            );
        });
    }

    /// Timeline mini-toolbar (zoom/snap + AS-1 Captions / Export discoverability).
    fn draw_mini_toolbar(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        doc: &Document,
        seq_id: SequenceId,
        view: &mut TimelineView,
        snap: &mut bool,
    ) {
        use crate::panels::DrawerGroup;
        use egui_phosphor::regular as ph;

        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, ui.visuals().faint_bg_color);

        // Real horizontal layout, not hand-computed rects. The previous version
        // placed every control at a fixed `bh`-derived x, which only lined up at
        // one font size: at any other UI scale the buttons drew wider than their
        // slots and collided (the ± pair ran into Snap), and "Captions" wrapped
        // onto a second line that the 24 px strip then clipped. Letting egui size
        // the widgets and advance the cursor makes the strip correct at every
        // scale; `Extend` keeps labels on one line so a narrow timeline scrolls
        // the strip's tail off instead of growing it vertically.
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect), |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
            ui.horizontal_centered(|ui| {
                ui.add_space(4.0);

                if ui
                    .button(ph::ARROWS_OUT_SIMPLE)
                    .on_hover_text("Zoom to fit (Shift+Z)")
                    .clicked()
                {
                    let extent = doc
                        .timeline
                        .as_ref()
                        .and_then(|p| p.sequences.get(&seq_id))
                        .map(|s| s.content_end())
                        .unwrap_or(Tick::ZERO);
                    view.fit(extent, view.last_lane_width_px.max(1.0));
                }
                if ui.button(ph::PLUS).on_hover_text("Zoom in (+)").clicked() {
                    view.zoom_around(ZOOM_STEP, view.scroll_ticks, 0.0);
                }
                if ui.button(ph::MINUS).on_hover_text("Zoom out (−)").clicked() {
                    view.zoom_around(1.0 / ZOOM_STEP, view.scroll_ticks, 0.0);
                }

                ui.separator();

                if ui
                    .selectable_label(*snap, "Snap")
                    .on_hover_text("Toggle snapping (N)")
                    .clicked()
                {
                    *snap = !*snap;
                }
                if ui
                    .selectable_label(view.fixed_playhead, "Fixed")
                    .on_hover_text("Fixed playhead: centre playhead during playback (K-A10)")
                    .clicked()
                {
                    view.fixed_playhead = !view.fixed_playhead;
                }

                let shift = ui.input(|i| i.modifiers.shift);
                ui.label(egui::RichText::new("Ripple").small().color(if shift {
                    ui.visuals().selection.stroke.color
                } else {
                    ui.visuals().weak_text_color()
                }))
                .on_hover_text("Hold Shift while trimming to ripple");

                ui.separator();

                // Proposal 213: Captions + Export on the timeline strip (AS-1 steps 3/8).
                if ui
                    .button(format!("{} Captions", ph::CLOSED_CAPTIONING))
                    .on_hover_text("Open captions panel — auto-caption and styles (AS-1)")
                    .clicked()
                {
                    self.open_drawer = Some(DrawerGroup::Captions);
                    self.prefs.open_drawer = Some(DrawerGroup::Captions);
                }
                if ui
                    .button(format!("{} Export", ph::EXPORT))
                    .on_hover_text("Export — Social 9:16 / 16:9 presets (AS-1)")
                    .clicked()
                {
                    self.export_dialog_open = true;
                    if !self.prefs.video_coach_dismissed && self.prefs.video_coach_step >= 2 {
                        self.prefs.video_coach_dismissed = true;
                        self.prefs.save();
                    }
                }
            });
        });
    }
}

fn handle_scroll_zoom(ui: &egui::Ui, full: egui::Rect, lane_left: f32, view: &mut TimelineView) {
    if !ui.rect_contains_pointer(full) {
        return;
    }
    let (scroll, ctrl, shift, pointer) = ui.input(|i| {
        (
            i.raw_scroll_delta,
            i.modifiers.command || i.modifiers.ctrl,
            i.modifiers.shift,
            i.pointer.hover_pos(),
        )
    });
    if scroll == egui::Vec2::ZERO {
        return;
    }
    if ctrl {
        // Ctrl+scroll → zoom toward the cursor (04 §2.1).
        let anchor = pointer
            .map(|p| view.x_to_tick(p.x, lane_left))
            .unwrap_or(view.scroll_ticks);
        let factor = if scroll.y > 0.0 {
            ZOOM_STEP
        } else {
            1.0 / ZOOM_STEP
        };
        view.zoom_around(factor, anchor, lane_left);
    } else if shift {
        // Shift+scroll → horizontal pan.
        let dt = view.px_to_ticks(-scroll.y);
        view.scroll_ticks = Tick((view.scroll_ticks.0 + dt.0).max(0));
    } else {
        // Plain scroll → vertical track pan.
        view.track_scroll_px = (view.track_scroll_px - scroll.y).max(0.0);
    }
}

/// Which part of the navigator thumb a drag grabbed (spec 17 G8): the body pans,
/// an end handle zooms.
#[derive(Clone, Copy, PartialEq)]
enum NavGrab {
    Body,
    Left,
    Right,
}

/// The bottom navigator / horizontal scrollbar (spec 17 G8): a thumb showing the
/// visible tick window over the whole sequence extent. Drag the body to pan, drag
/// an end handle to zoom, click the trough to jump. Writes `scroll_ticks` and
/// `pixels_per_tick` — the same view fields `handle_scroll_zoom` drives.
fn draw_navigator(
    ui: &mut egui::Ui,
    nav_rect: egui::Rect,
    lane_left: f32,
    seq: &Sequence,
    view: &mut TimelineView,
) {
    let painter = ui.painter_at(nav_rect);
    painter.rect_filled(nav_rect, 0.0, ui.visuals().faint_bg_color);
    // The trough spans the lane column (aligned under the lanes).
    let trough = egui::Rect::from_min_max(
        egui::pos2(lane_left, nav_rect.top() + 2.0),
        egui::pos2(nav_rect.right() - 2.0, nav_rect.bottom() - 2.0),
    );
    if trough.width() < 8.0 {
        return;
    }
    let lane_w = view.last_lane_width_px.max(1.0);
    let visible = view.px_to_ticks(lane_w).0.max(1);
    let scroll = view.scroll_ticks.0.max(0);
    // Whole-sequence extent: cover the content and the current window so the
    // thumb never runs off the trough.
    let extent = seq.content_end().0.max(scroll + visible).max(1);
    let ticks_per_px = extent as f64 / trough.width() as f64;
    let to_x = |t: i64| trough.left() + (t as f64 / extent as f64) as f32 * trough.width();
    let x0 = to_x(scroll).max(trough.left());
    let x1 = to_x(scroll + visible).min(trough.right());
    let thumb = egui::Rect::from_min_max(
        egui::pos2(x0, trough.top()),
        egui::pos2(x1.max(x0 + 8.0), trough.bottom()),
    );
    let accent = ui.visuals().selection.stroke.color;
    painter.rect_filled(trough, 3.0, ui.visuals().extreme_bg_color);
    painter.rect(
        thumb,
        egui::Rounding::same(3.0),
        accent.gamma_multiply(0.35),
        egui::Stroke::new(1.0, accent),
    );

    let resp = ui.interact(
        trough,
        ui.id().with("tl_navigator"),
        egui::Sense::click_and_drag(),
    );
    if resp.hovered() {
        ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::Grab);
    }
    let grab_id = egui::Id::new("tl_nav_grab");
    let handle = 6.0_f32;
    if resp.drag_started() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let grab = if (pos.x - thumb.left()).abs() <= handle {
                NavGrab::Left
            } else if (pos.x - thumb.right()).abs() <= handle {
                NavGrab::Right
            } else {
                NavGrab::Body
            };
            ui.data_mut(|d| d.insert_temp(grab_id, grab));
        }
    }
    if resp.dragged() {
        let grab = ui
            .data(|d| d.get_temp::<NavGrab>(grab_id))
            .unwrap_or(NavGrab::Body);
        let dx = resp.drag_delta().x as f64;
        match grab {
            NavGrab::Body => {
                let dt = (dx * ticks_per_px) as i64;
                view.scroll_ticks = Tick((scroll + dt).max(0));
            }
            NavGrab::Right => {
                // Drag the right edge → change the visible span, keep left fixed.
                let new_right = (((thumb.right() + dx as f32) as f64 - trough.left() as f64)
                    * ticks_per_px) as i64;
                let new_visible = (new_right - scroll).max(1);
                view.pixels_per_tick = (lane_w as f64 / new_visible as f64)
                    .clamp(layout::MIN_PIXELS_PER_TICK, layout::MAX_PIXELS_PER_TICK);
            }
            NavGrab::Left => {
                // Drag the left edge → change scroll + span, keep right fixed.
                let right = scroll + visible;
                let new_left = ((((thumb.left() + dx as f32) as f64 - trough.left() as f64)
                    * ticks_per_px) as i64)
                    .clamp(0, right - 1);
                let new_visible = (right - new_left).max(1);
                view.scroll_ticks = Tick(new_left);
                view.pixels_per_tick = (lane_w as f64 / new_visible as f64)
                    .clamp(layout::MIN_PIXELS_PER_TICK, layout::MAX_PIXELS_PER_TICK);
            }
        }
    }
    if resp.drag_stopped() {
        ui.data_mut(|d| d.remove::<NavGrab>(grab_id));
    }
    // Click the trough outside the thumb → center the window on that tick.
    if resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            if pos.x < thumb.left() || pos.x > thumb.right() {
                let center = ((pos.x - trough.left()) as f64 * ticks_per_px) as i64;
                view.scroll_ticks = Tick((center - visible / 2).max(0));
            }
        }
    }
}

fn lane_colors(ui: &egui::Ui) -> clips::LaneColors {
    let v = ui.visuals();
    clips::LaneColors {
        clip_fill: v.faint_bg_color,
        clip_stroke: v.widgets.noninteractive.bg_stroke.color,
        selected_fill: v.selection.bg_fill,
        selected_stroke: v.selection.stroke.color,
        label: v.text_color(),
        transition: v.warn_fg_color,
        offline: v.error_fg_color,
        locked_hatch: v.weak_text_color().gamma_multiply(0.5),
        sync_lock_seam: v.hyperlink_color,
        waveform_fill: v.weak_text_color().gamma_multiply(0.8),
        waveform_rms: v.text_color().gamma_multiply(0.6),
        // Fixed teal — a functional data-code hue (like the clip-label
        // swatches), not the selection violet, so an FX-badged clip never reads
        // as selected. Legible on both light and dark lane fills.
        fx_badge: egui::Color32::from_rgb(0x2D, 0xD4, 0xBF),
    }
}

// ── Timeline media caches (spec 15 — NLE parity gap 10) ─────────────────────
//
// Wave 2 built the engine half (`photonic_video::media::thumbnails`'s
// `ThumbnailCache`/`WaveformCache`, background-decoded + sidecar-cached) and the
// draw half (`clips::paint_lane`, which reads a cache and registers egui
// textures), but the call site below passed `None, None`, so clips rendered
// flat. This is the connecting tissue: `PhotonicApp` holds one
// `TimelineMediaCaches` for the session and `draw_timeline_panel` refreshes its
// asset snapshot from the active document's media pool each frame, then hands
// the two caches to `paint_lane`. Every path degrades gracefully — no ffmpeg,
// an unsaved project, a non-file asset, or an in-flight decode all just leave
// the flat clip fill showing (the caches never block the draw thread).

/// One media-pool asset resolved to what the background cache workers need: an
/// on-disk path plus its content hash (the sidecar disk-cache key). Held behind
/// a shared `Mutex` and rebuilt every frame from `TimelineProject::media` so the
/// `Send + Sync` resolver closures never borrow the document.
#[derive(Clone, PartialEq, Eq)]
struct ResolvedMedia {
    path: PathBuf,
    content_hash: Option<String>,
}

/// The clip-thumbnail + waveform caches backing the timeline lane painter,
/// owned by `PhotonicApp` for the session and constructed lazily on first
/// video-mode paint.
pub(crate) struct TimelineMediaCaches {
    thumbnails: ThumbnailCache,
    waveforms: WaveformCache,
    /// `AssetId` → resolved path/hash, shared with the resolver closures and
    /// rebuilt from the active media pool every frame (see [`Self::refresh`]).
    resolved: Arc<Mutex<HashMap<AssetId, ResolvedMedia>>>,
    /// Memoized probe/keyframe specs must be invalidated with the media cache.
    thumb_memo: Arc<Mutex<HashMap<AssetId, Option<ThumbSpecParts>>>>,
    cache_dir: Arc<Mutex<PathBuf>>,
    project_path: Option<PathBuf>,
    tab_key: usize,
    media_snapshot: Option<HashMap<AssetId, ResolvedMedia>>,
}

impl TimelineMediaCaches {
    /// Build the caches. `project_path` is the open project file, used for the
    /// sidecar cache dir (`<project>.photon.cache/`); an unsaved project falls
    /// back to a session temp dir so thumbnails still cache within the run. A
    /// missing ffmpeg toolchain leaves both sources decode-less: every request
    /// then misses and the flat fill shows — graceful, never an error.
    ///
    /// Constructing spawns the two caches' background worker threads (one each);
    /// they idle until the first `request` and exit cleanly on drop.
    pub(crate) fn new(project_path: Option<&Path>, tab_key: usize) -> Self {
        let cache_dir = project_path
            .map(cache_dir_for_project)
            .unwrap_or_else(|| std::env::temp_dir().join("photonic-timeline-cache"));
        let cache_dir_state = Arc::new(Mutex::new(cache_dir.clone()));
        let tools = locate().ok();
        let resolved: Arc<Mutex<HashMap<AssetId, ResolvedMedia>>> = Arc::default();

        // Thumbnail decode source. The cache calls the resolver twice per bucket
        // (once for the disk-key hash, once for the decode), so the per-asset
        // probe + keyframe build is memoized to run at most once per asset.
        let thumb_resolved = Arc::clone(&resolved);
        let thumb_tools = tools.clone();
        let thumb_memo: Arc<Mutex<HashMap<AssetId, Option<ThumbSpecParts>>>> = Arc::default();
        let thumb_cache_dir = Arc::clone(&cache_dir_state);
        let thumb_memo_for_source = Arc::clone(&thumb_memo);
        let thumb_source = Arc::new(DecodeThumbnailSource::new(move |asset: AssetId| {
            let tools = thumb_tools.clone()?;
            if let Some(parts) = thumb_memo_for_source.lock().unwrap().get(&asset) {
                return parts.clone().map(ThumbSpecParts::into_spec);
            }
            let media = thumb_resolved.lock().unwrap().get(&asset).cloned();
            let cache_dir = thumb_cache_dir.lock().unwrap().clone();
            let parts = media.and_then(|m| ThumbSpecParts::build(&tools, &m, &cache_dir));
            thumb_memo_for_source
                .lock()
                .unwrap()
                .insert(asset, parts.clone());
            parts.map(ThumbSpecParts::into_spec)
        }));
        let thumbnails = ThumbnailCache::new(thumb_source, cache_dir.clone());

        // Waveform source: decode the asset's first audio stream once and build
        // its peak pyramid (the cache persists it to the sidecar and reuses it).
        let wave_source = Arc::new(PoolWaveformSource {
            resolved: Arc::clone(&resolved),
            tools,
        });
        let waveforms = WaveformCache::new(wave_source, cache_dir);

        TimelineMediaCaches {
            thumbnails,
            waveforms,
            resolved,
            thumb_memo,
            cache_dir: cache_dir_state,
            project_path: project_path.map(Path::to_path_buf),
            tab_key,
            media_snapshot: None,
        }
    }

    /// Rebuild the asset snapshot from `pool` (clears + re-inserts every
    /// file-backed asset — cheap for a typical pool). Called once per frame
    /// before painting lanes so the resolver closures always see the active
    /// document's media without holding a borrow across the worker threads.
    fn refresh(&mut self, project_path: Option<&Path>, tab_key: usize, pool: &MediaPool) {
        let next = pool
            .assets
            .iter()
            .filter_map(|(id, asset)| match &asset.source {
                AssetSource::File { path, .. } => Some((
                    *id,
                    ResolvedMedia {
                        path: path.clone(),
                        content_hash: asset.content_hash.clone(),
                    },
                )),
                _ => None,
            })
            .collect::<HashMap<_, _>>();
        let next_project_path = project_path.map(Path::to_path_buf);
        let owner_changed = self.project_path != next_project_path || self.tab_key != tab_key;
        let media_changed = self
            .media_snapshot
            .as_ref()
            .is_some_and(|previous| previous != &next);

        if owner_changed {
            let cache_dir = project_path
                .map(cache_dir_for_project)
                .unwrap_or_else(|| std::env::temp_dir().join("photonic-timeline-cache"));
            *self.cache_dir.lock().unwrap() = cache_dir.clone();
            self.thumbnails.set_cache_dir(cache_dir.clone());
            self.waveforms.set_cache_dir(cache_dir);
        }
        if owner_changed || media_changed {
            self.thumbnails.invalidate();
            self.waveforms.invalidate();
            self.thumb_memo.lock().unwrap().clear();
        }

        self.project_path = next_project_path;
        self.tab_key = tab_key;
        self.media_snapshot = Some(next.clone());
        let mut map = self.resolved.lock().unwrap();
        *map = next;
    }

    fn has_pending(&self) -> bool {
        self.thumbnails.has_pending() || self.waveforms.has_pending()
    }
}

/// The `Clone`-able parts of a [`ThumbnailDecodeSpec`] (the spec itself is not
/// `Clone`), memoized per asset and rebuilt into a fresh spec on each resolver
/// call.
#[derive(Clone)]
struct ThumbSpecParts {
    params: SourceParams,
    tools: FfmpegTools,
    content_hash: Option<String>,
}

impl ThumbSpecParts {
    /// Probe `media` and build its keyframe/pts model into a decode spec's
    /// parts, or `None` if the file has no decodable video stream or the probe
    /// fails (offline, no codec) — the caller then leaves the flat fill. Runs on
    /// the cache's worker thread (may block on ffmpeg), never the draw thread.
    ///
    /// Keyframe/PTS indexes go through [`KeyframeIndex::load_or_build`] so a
    /// 4K drone clip is not re-scanned on every session (the previous
    /// `KeyframeIndex::build` path alone cost multiple seconds per asset and
    /// made timeline thumbs look "stuck loading").
    fn build(
        tools: &FfmpegTools,
        media: &ResolvedMedia,
        cache_dir: &Path,
    ) -> Option<ThumbSpecParts> {
        let details = probe_details(tools, &media.path).ok()?;
        let video = details.probe.video.clone()?;
        let hash = media
            .content_hash
            .clone()
            .or_else(|| photonic_video::media::probe::content_hash(&media.path).ok());
        let keyframes = match hash.as_deref() {
            Some(h) => KeyframeIndex::load_or_build(tools, &media.path, cache_dir, h).ok()?,
            None => KeyframeIndex::build(tools, &media.path).ok()?,
        };
        let pts_kind = if details.is_vfr {
            let pts = match hash.as_deref() {
                Some(h) => PtsIndex::load_or_build(tools, &media.path, cache_dir, h).ok()?,
                None => PtsIndex::build(tools, &media.path).ok()?,
            };
            PtsKind::Vfr(Arc::new(pts))
        } else {
            PtsKind::Cfr(video.frame_rate)
        };
        Some(ThumbSpecParts {
            params: SourceParams {
                input: media.path.clone(),
                width: video.width,
                height: video.height,
                pix_fmt: PixFmt::for_alpha(details.has_alpha),
                pts_kind,
                keyframes,
            },
            tools: tools.clone(),
            content_hash: hash,
        })
    }

    fn into_spec(self) -> ThumbnailDecodeSpec {
        ThumbnailDecodeSpec {
            params: self.params,
            tools: self.tools,
            content_hash: self.content_hash,
        }
    }
}

/// Media-pool-backed [`WaveformSource`]: resolves an asset to its file and
/// decodes its first audio stream (via ffmpeg) into a peak pyramid on the
/// cache's worker thread. No audio / no ffmpeg → `None` → the flat fill shows.
struct PoolWaveformSource {
    resolved: Arc<Mutex<HashMap<AssetId, ResolvedMedia>>>,
    tools: Option<FfmpegTools>,
}

impl WaveformSource for PoolWaveformSource {
    fn asset_hash(&self, asset: AssetId) -> Option<String> {
        self.resolved
            .lock()
            .unwrap()
            .get(&asset)?
            .content_hash
            .clone()
    }

    fn build_pyramid(&self, asset: AssetId) -> Option<WaveformPyramid> {
        /// Waveform decode target rate — 09 §8.1's peak pyramid is rate-agnostic
        /// (buckets are in samples), so a fixed 48 kHz keeps the bucket math
        /// deterministic regardless of the source's own rate.
        const WAVEFORM_SAMPLE_RATE: u32 = 48_000;
        let tools = self.tools.clone()?;
        let media = self.resolved.lock().unwrap().get(&asset).cloned()?;
        let hash = media.content_hash.clone().unwrap_or_default();
        let mut src =
            FfmpegPcmSource::spawn(&tools, &media.path, Tick::ZERO, WAVEFORM_SAMPLE_RATE).ok()?;
        Some(build_pyramid(&mut src, hash))
    }
}

/// The clip-area interaction: selection, drag gestures (move/trim/roll/slip/
/// slide/ripple-trim), marquee, and the context menu. Preview during drag,
/// commit once on release (one undo step per gesture).
#[allow(clippy::too_many_arguments)]
fn self_interact(
    ui: &mut egui::Ui,
    doc: &mut Document,
    history: &mut CommandHistory,
    seq_id: SequenceId,
    view: &mut TimelineView,
    playhead: &mut Tick,
    selection: &mut Vec<ClipId>,
    snap: bool,
    razor: bool,
    lane_left: f32,
    frame_rate: FrameRate,
    lanes_rect: egui::Rect,
    rows: &[tracks::TrackRow],
    hits: &[interact::HitCandidate],
    replace_candidate: Option<(ClipSource, Option<Tick>, String)>,
    open_edit_duration: &mut Option<(TrackId, ClipId)>,
) {
    let resp = ui.interact(
        lanes_rect,
        ui.id().with("timeline_lanes"),
        egui::Sense::click_and_drag(),
    );
    let drag_id = egui::Id::new(DRAG_ID);
    let marquee_id = egui::Id::new(MARQUEE_ID);

    // Which clip/zone is under a given screen pos. Locked-track candidates
    // are never a hit (14-nle-parity QW-2) — see `interact::hit_at`. This one
    // function backs selection, drag-start, and the context-menu target, so
    // the lock guard applies uniformly to all three.
    let hit_at = |pos: egui::Pos2| -> Option<(TrackId, ClipId, egui::Rect, interact::ClipZone)> {
        interact::hit_at(pos, EDGE_HIT_PX, hits)
    };

    // ── Selection on click ──────────────────────────────────────────────────
    if resp.clicked() {
        let mods = ui.input(|i| i.modifiers);
        if let Some(pos) = resp.interact_pointer_pos() {
            if let Some((track, clip, _r, _z)) = hit_at(pos) {
                if razor {
                    // Blade tool: a click splits the clip at the click tick
                    // (spec 16 §4, M-4) instead of selecting it.
                    let at = view.x_to_tick(pos.x, lane_left);
                    interact::do_razor_split(doc, history, seq_id, track, clip, at);
                } else {
                    apply_selection(selection, clip, mods);
                }
            } else if !mods.ctrl && !mods.shift && !mods.command {
                selection.clear();
            }
        }
    }

    if resp.double_clicked() {
        if let Some((_, clip, _, zone)) = resp.interact_pointer_pos().and_then(hit_at) {
            let precision = zone != interact::ClipZone::Body;
            let cut = if precision {
                doc.timeline
                    .as_ref()
                    .and_then(|p| p.sequences.get(&seq_id))
                    .and_then(|s| s.tracks().flat_map(|t| &t.clips).find(|c| c.id == clip))
                    .map(|c| {
                        if zone == interact::ClipZone::LeftEdge {
                            c.start
                        } else {
                            c.end()
                        }
                    })
            } else {
                None
            };
            ui.data_mut(|data| {
                data.insert_temp(
                    egui::Id::new("nested_precision_request"),
                    (clip, precision, cut),
                )
            });
        }
    }

    // ── Drag start ──────────────────────────────────────────────────────────
    if resp.drag_started() {
        let mods = ui.input(|i| i.modifiers);
        if let Some(pos) = resp.interact_pointer_pos() {
            let seq = doc
                .timeline
                .as_ref()
                .unwrap()
                .sequences
                .get(&seq_id)
                .unwrap();
            if let Some((track, clip, _rect, zone)) = hit_at(pos) {
                // Selection follows the grabbed clip unless it is already in a
                // multi-selection being dragged.
                if !selection.contains(&clip) {
                    apply_selection(selection, clip, mods);
                }
                if let Some(state) = start_clip_drag(
                    seq, track, clip, zone, mods, pos, view, lane_left, *playhead,
                ) {
                    ui.data_mut(|d| d.insert_temp(drag_id, state));
                }
            } else {
                // Empty space → marquee.
                ui.data_mut(|d| {
                    d.insert_temp(
                        marquee_id,
                        Marquee {
                            start: pos,
                            additive: mods.shift || mods.ctrl || mods.command,
                        },
                    )
                });
            }
        }
    }

    // ── Drag cancel: Esc abandons the gesture (210 §5 / 43 §2.1) ────────────
    // Dropping the transient state is the whole cancel: the document is never
    // touched mid-drag (only the ghost is painted), and `drag_stopped` below
    // commits nothing when it finds no `DragState`. Decision is pure
    // [`interact::should_cancel_drag`] so UI-path tests can lock it without egui.
    {
        let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
        let had_gesture = ui.data(|d| {
            d.get_temp::<DragState>(drag_id).is_some()
                || d.get_temp::<Marquee>(marquee_id).is_some()
        });
        if interact::should_cancel_drag(escape, had_gesture) {
            ui.data_mut(|d| {
                d.remove::<DragState>(drag_id);
                d.remove::<Marquee>(marquee_id);
            });
        }
    }

    // ── Drag update: preview ghost + snap guide ─────────────────────────────
    if resp.dragged() {
        if let Some(mut state) = ui.data(|d| d.get_temp::<DragState>(drag_id)) {
            state.moved = true;
            if let Some(pos) = resp.interact_pointer_pos() {
                // K-A10: continuous pan when the pointer sits in a lane edge zone
                // so long-form moves/trims don't stop at the viewport.
                let pan_px =
                    TimelineView::edge_auto_pan_speed(pos.x, lane_left, lanes_rect.width());
                if pan_px.abs() >= 0.5 {
                    view.edge_auto_pan(pan_px);
                    ui.ctx().request_repaint();
                }
                let seq = doc
                    .timeline
                    .as_ref()
                    .unwrap()
                    .sequences
                    .get(&seq_id)
                    .unwrap();
                state.dest_track = track_at_y(rows, lanes_rect, view, pos.y).unwrap_or(state.track);
                preview_drag(
                    ui, seq, &state, view, lane_left, snap, frame_rate, pos, lanes_rect, rows,
                );
            }
            ui.data_mut(|d| d.insert_temp(drag_id, state));
        } else if let Some(m) = ui.data(|d| d.get_temp::<Marquee>(marquee_id)) {
            if let Some(pos) = resp.interact_pointer_pos() {
                let rect = egui::Rect::from_two_pos(m.start, pos);
                ui.painter_at(lanes_rect).rect(
                    rect,
                    0.0,
                    egui::Color32::TRANSPARENT,
                    egui::Stroke::new(1.0, ui.visuals().selection.stroke.color),
                );
                // Locked-track clips are excluded from marquee selection too
                // (14-nle-parity QW-2 — fully inert, not just edit-blocked).
                interact::apply_marquee(
                    rect,
                    hits.iter().filter(|h| !h.locked).map(|h| (h.rect, h.clip)),
                    m.additive,
                    selection,
                );
            }
        }
    }

    // ── Drag release: commit ────────────────────────────────────────────────
    if resp.drag_stopped() {
        let state = ui.data(|d| d.get_temp::<DragState>(drag_id));
        ui.data_mut(|d| {
            d.remove::<DragState>(drag_id);
            d.remove::<Marquee>(marquee_id);
        });
        if let Some(state) = state {
            if state.moved {
                if let Some(pos) = resp.interact_pointer_pos() {
                    commit_drag(
                        doc,
                        history,
                        seq_id,
                        &state,
                        view,
                        lane_left,
                        snap,
                        frame_rate,
                        pos,
                        selection.as_slice(),
                    );
                }
            }
        }
    }

    // ── Context menu ────────────────────────────────────────────────────────
    let menu_target = resp.hover_pos().and_then(hit_at).map(|(t, c, _, _)| (t, c));
    // Over empty lane space, resolve the (track, tick) under the cursor so the
    // menu can offer Close Gap (spec 17 G1).
    let gap_target = if menu_target.is_none() {
        resp.hover_pos().and_then(|pos| {
            if lanes_rect.contains(pos) {
                track_at_y(rows, lanes_rect, view, pos.y)
                    .map(|tr| (tr, view.x_to_tick(pos.x, lane_left)))
            } else {
                None
            }
        })
    } else {
        None
    };
    let ph = *playhead;
    resp.context_menu(|ui| {
        clip_context_menu(
            ui,
            doc,
            history,
            seq_id,
            menu_target,
            gap_target,
            ph,
            selection.as_slice(),
            replace_candidate,
            open_edit_duration,
        )
    });
}

fn apply_selection(selection: &mut Vec<ClipId>, clip: ClipId, mods: egui::Modifiers) {
    if mods.ctrl || mods.command {
        if let Some(i) = selection.iter().position(|c| *c == clip) {
            selection.remove(i);
        } else {
            selection.push(clip);
        }
    } else if mods.shift {
        if !selection.contains(&clip) {
            selection.push(clip);
        }
    } else {
        selection.clear();
        selection.push(clip);
    }
}

/// Same-kind lane-index offset from `from` to `to` (210 vertical multi-move).
/// `None` when either track is missing or the kinds differ (video vs audio).
fn same_kind_track_delta(
    doc: &Document,
    seq_id: SequenceId,
    from: TrackId,
    to: TrackId,
) -> Option<i32> {
    if from == to {
        return Some(0);
    }
    let seq = doc.timeline.as_ref()?.sequences.get(&seq_id)?;
    let src = seq.track(from)?;
    let dst = seq.track(to)?;
    if src.kind != dst.kind {
        return None;
    }
    let lanes = seq.tracks_for(src.kind);
    let i = lanes.iter().position(|t| t.id == from)? as i32;
    let j = lanes.iter().position(|t| t.id == to)? as i32;
    Some(j - i)
}

/// Which track row contains screen-y `y` (for cross-track moves).
fn track_at_y(
    rows: &[tracks::TrackRow],
    lanes_rect: egui::Rect,
    view: &TimelineView,
    y: f32,
) -> Option<TrackId> {
    let mut top = lanes_rect.top() - view.track_scroll_px;
    for r in rows {
        let bot = top + r.height;
        if y >= top && y < bot {
            return Some(r.id);
        }
        top = bot;
    }
    None
}

/// The visible lane rectangle for one track. Drag previews use this instead of
/// the full timeline body so a clip move highlights only its destination row.
fn lane_rect_for_track(
    rows: &[tracks::TrackRow],
    lanes_rect: egui::Rect,
    view: &TimelineView,
    track: TrackId,
) -> Option<egui::Rect> {
    let mut top = lanes_rect.top() - view.track_scroll_px;
    for row in rows {
        let bottom = top + row.height;
        if row.id == track {
            let rect = egui::Rect::from_min_max(
                egui::pos2(lanes_rect.left(), top),
                egui::pos2(lanes_rect.right(), bottom),
            )
            .intersect(lanes_rect);
            return (!rect.is_negative()).then_some(rect);
        }
        top = bottom;
    }
    None
}

/// Resolve a drag-start into a [`DragState`] (04 §2.3/§2.4 modifier scheme).
#[allow(clippy::too_many_arguments)]
fn start_clip_drag(
    seq: &Sequence,
    track: TrackId,
    clip: ClipId,
    zone: interact::ClipZone,
    mods: egui::Modifiers,
    pos: egui::Pos2,
    view: &TimelineView,
    lane_left: f32,
    playhead: Tick,
) -> Option<DragState> {
    let t = seq.track(track)?;
    let idx = t.clips.iter().position(|c| c.id == clip)?;
    let c = &t.clips[idx];
    let grab_tick = view.x_to_tick(pos.x, lane_left);

    // Resolve (kind, primary clip). For a roll, `primary` is always the LEFT
    // clip and `Roll.right` its right neighbour, so the shared boundary is
    // `primary.end()` (04 §2.4: roll is hit-test-based, not a modifier). The
    // neighbour ids for a flush boundary drive the (pure) resolution below.
    let left_shared = idx
        .checked_sub(1)
        .and_then(|i| t.clips.get(i))
        .filter(|prev| prev.end() == c.start)
        .map(|prev| prev.id);
    let right_shared = t
        .clips
        .get(idx + 1)
        .filter(|next| next.start == c.end())
        .map(|next| next.id);
    let (kind, primary) =
        interact::resolve_drag_kind(zone, mods.alt, mods.shift, clip, left_shared, right_shared);

    let candidates = interact::build_snap_candidates(seq, track, primary, playhead);
    let orig = ClipTiming::of(t.clips.iter().find(|c| c.id == primary)?);
    Some(DragState {
        kind,
        track,
        clip: primary,
        grab_tick,
        orig,
        dest_track: track,
        candidates,
        moved: false,
    })
}

/// Snap + quantize the moving edge tick for a drag.
fn resolved_tick(
    raw: Tick,
    state: &DragState,
    snap: bool,
    frame_rate: FrameRate,
    view: &TimelineView,
) -> Tick {
    let threshold = view.px_to_ticks(SNAP_THRESHOLD_PX);
    interact::snap_and_quantize(raw, &state.candidates, threshold, snap, frame_rate)
}

/// Draw the ghost + snap guide for an in-progress drag.
#[allow(clippy::too_many_arguments)]
fn preview_drag(
    ui: &egui::Ui,
    seq: &Sequence,
    state: &DragState,
    view: &TimelineView,
    lane_left: f32,
    snap: bool,
    frame_rate: FrameRate,
    pos: egui::Pos2,
    lanes_rect: egui::Rect,
    rows: &[tracks::TrackRow],
) {
    let Some(t) = seq.track(state.track) else {
        return;
    };
    let Some(c) = t.clips.iter().find(|c| c.id == state.clip) else {
        return;
    };
    let accent = ui.visuals().selection.stroke.color;
    let painter = ui.painter_at(lanes_rect);
    let Some(track_rect) = lane_rect_for_track(rows, lanes_rect, view, state.dest_track) else {
        return;
    };
    let delta_raw = view.x_to_tick(pos.x, lane_left) - state.grab_tick;

    // The raw (pre-snap) lane tick of the edge this gesture moves, kept
    // alongside where it resolves to so the snap guide below can ask whether a
    // candidate was actually captured. `Slip` moves no edge — the clip holds
    // its place and only its source window shifts.
    let raw_edge = match state.kind {
        DragKind::Move | DragKind::Slide | DragKind::TrimStart | DragKind::RippleTrimStart => {
            Some(state.orig.start + delta_raw)
        }
        DragKind::TrimEnd | DragKind::RippleTrimEnd => {
            Some(state.orig.start + state.orig.duration + delta_raw)
        }
        DragKind::Roll { .. } => Some(c.start + delta_raw),
        DragKind::Slip => None,
    };
    let edge_tick = match raw_edge {
        Some(raw) => resolved_tick(raw, state, snap, frame_rate, view),
        None => c.start,
    };

    // Cross-track move: outline the destination lane so the drop target stays
    // legible when the ghost has travelled far from the grabbed clip's own row
    // (13 §1.2 "dragging"). Painted before the ghost so the ghost sits on top.
    // Only `Move` retargets a track — every trim/roll/slip/slide kind commits
    // back to `state.track`.
    if state.dest_track != state.track && matches!(state.kind, DragKind::Move) {
        painter.rect(
            track_rect,
            egui::Rounding::same(3.0),
            accent.gamma_multiply(0.06),
            egui::Stroke::new(1.5, accent),
        );
    }

    // Ghost outline of the clip at its previewed position.
    let (gx0, gx1) = match state.kind {
        DragKind::Move | DragKind::Slide => (edge_tick, edge_tick + c.duration),
        DragKind::TrimStart | DragKind::RippleTrimStart => (edge_tick, c.end()),
        DragKind::TrimEnd | DragKind::RippleTrimEnd | DragKind::Roll { .. } => (c.start, edge_tick),
        DragKind::Slip => (c.start, c.end()),
    };
    let x0 = view.tick_to_x(gx0, lane_left);
    let x1 = view.tick_to_x(gx1, lane_left);
    let ghost = egui::Rect::from_min_max(
        egui::pos2(x0.min(x1), track_rect.top() + 2.0),
        egui::pos2(x0.max(x1), track_rect.bottom() - 2.0),
    );
    painter.rect(
        ghost,
        egui::Rounding::same(3.0),
        accent.gamma_multiply(0.15),
        egui::Stroke::new(1.0, accent),
    );

    // Snap guide (13 §1.1): a thin accent line at the SNAP TARGET, painted only
    // while a candidate is actually captured — "for the duration of the snap".
    // Drawing it unconditionally at the moving edge (the pre-210 behaviour) made
    // it a cursor tracer that read as "snapped" at every pointer position, so it
    // carried no information. It spans all lanes because candidates come from
    // other tracks' clip edges, the playhead, and markers (04 §2.5), not just
    // this row.
    if snap {
        let threshold = view.px_to_ticks(SNAP_THRESHOLD_PX);
        if let Some(target) =
            raw_edge.and_then(|raw| interact::nearest_snap(raw, &state.candidates, threshold))
        {
            let sx = view.tick_to_x(target, lane_left);
            painter.line_segment(
                [
                    egui::pos2(sx, lanes_rect.top()),
                    egui::pos2(sx, lanes_rect.bottom()),
                ],
                egui::Stroke::new(1.0, accent),
            );
        }
    }

    // Duration tooltip for trims (13 §1.3/1.6 non-color confirmation).
    if matches!(
        state.kind,
        DragKind::TrimStart
            | DragKind::TrimEnd
            | DragKind::RippleTrimStart
            | DragKind::RippleTrimEnd
    ) {
        let dur = (gx1 - gx0).0.max(0);
        // Anchored to the moving edge, not to any snap target: 13 §1.6 makes
        // this readout the non-colour-dependent confirmation of where the edge
        // actually landed, so it must follow the edge even when nothing snaps.
        let edge_x = view.tick_to_x(edge_tick, lane_left);
        painter.text(
            egui::pos2(edge_x + 4.0, lanes_rect.top() + 4.0),
            egui::Align2::LEFT_TOP,
            ruler::timecode(Tick(dur), frame_rate),
            egui::FontId::monospace(10.0),
            ui.visuals().text_color(),
        );
    }
}

/// Pair each selected `ClipId` with the track holding it.
///
/// Selection is stored as bare `ClipId`s (04 §6), but the ops layer addresses
/// clips by `(track, clip)`, so the track has to be recovered from the
/// sequence. Returns `None` if there is no timeline; silently drops ids that no
/// longer resolve, which can happen when a selection outlives the clips it
/// named (an undo, or a delete from another surface).
fn resolve_selection_tracks(
    doc: &Document,
    seq_id: SequenceId,
    selection: &[ClipId],
) -> Option<Vec<(TrackId, ClipId)>> {
    let seq = doc.timeline.as_ref()?.sequences.get(&seq_id)?;
    let mut out = Vec::with_capacity(selection.len());
    for track in seq.tracks() {
        for clip in &track.clips {
            if selection.contains(&clip.id) {
                out.push((track.id, clip.id));
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Commit a finished drag as one undo step through `ops_bridge`.
#[allow(clippy::too_many_arguments)]
fn commit_drag(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq_id: SequenceId,
    state: &DragState,
    view: &TimelineView,
    lane_left: f32,
    snap: bool,
    frame_rate: FrameRate,
    pos: egui::Pos2,
    selection: &[ClipId],
) {
    let delta_raw = view.x_to_tick(pos.x, lane_left) - state.grab_tick;
    match state.kind {
        DragKind::Move => {
            let new_start =
                resolved_tick(state.orig.start + delta_raw, state, snap, frame_rate, view);

            // Multi-select body move (04 §2.6, 210 §5): the grabbed clip drags
            // the rest of the selection with it by the same time delta and
            // same-kind track_delta, in one undo step. Relative lane spacing
            // is preserved (every member shifts by the primary's lane offset).
            if selection.len() > 1 && selection.contains(&state.clip) {
                if let Some(moving) = resolve_selection_tracks(doc, seq_id, selection) {
                    let track_delta =
                        same_kind_track_delta(doc, seq_id, state.track, state.dest_track)
                            .unwrap_or(0);
                    // Deliberately unclamped: clamping the primary to zero
                    // would shrink the delta and squash the spacing the
                    // selection had. `ops::move_clips` refuses the whole move
                    // if any member would land before zero or off the lane list.
                    ops_bridge::move_clips(
                        doc,
                        history,
                        seq_id,
                        &moving,
                        new_start - state.orig.start,
                        track_delta,
                    );
                    return;
                }
            }

            let new_start = if new_start.0 < 0 {
                Tick::ZERO
            } else {
                new_start
            };
            if state.dest_track != state.track {
                ops_bridge::move_clip_cross_track(
                    doc,
                    history,
                    seq_id,
                    state.track,
                    state.dest_track,
                    state.clip,
                    new_start,
                );
            } else {
                ops_bridge::move_clip(doc, history, seq_id, state.track, state.clip, new_start);
            }
        }
        DragKind::TrimStart | DragKind::RippleTrimStart => {
            let new_start =
                resolved_tick(state.orig.start + delta_raw, state, snap, frame_rate, view);
            let end = state.orig.start + state.orig.duration;
            if new_start >= end {
                return;
            }
            let new = ClipTiming {
                start: new_start,
                duration: end - new_start,
                source_in: state.orig.source_in + (new_start - state.orig.start),
            };
            if matches!(state.kind, DragKind::RippleTrimStart) {
                ops_bridge::ripple_trim(doc, history, seq_id, state.track, state.clip, new);
            } else {
                ops_bridge::trim(doc, history, seq_id, state.track, state.clip, new);
            }
        }
        DragKind::TrimEnd | DragKind::RippleTrimEnd => {
            let raw_end = state.orig.start + state.orig.duration + delta_raw;
            let new_end = resolved_tick(raw_end, state, snap, frame_rate, view);
            if new_end <= state.orig.start {
                return;
            }
            let new = ClipTiming {
                start: state.orig.start,
                duration: new_end - state.orig.start,
                source_in: state.orig.source_in,
            };
            if matches!(state.kind, DragKind::RippleTrimEnd) {
                ops_bridge::ripple_trim(doc, history, seq_id, state.track, state.clip, new);
            } else {
                ops_bridge::trim(doc, history, seq_id, state.track, state.clip, new);
            }
        }
        DragKind::Roll { right } => {
            // `state.clip` is the left clip; `right` the neighbor.
            let boundary = state.orig.start + state.orig.duration;
            let new_boundary = resolved_tick(boundary + delta_raw, state, snap, frame_rate, view);
            let delta = new_boundary - boundary;
            if delta.0 != 0 {
                ops_bridge::roll(doc, history, seq_id, state.track, state.clip, right, delta);
            }
        }
        DragKind::Slip => {
            // Drag right → reveal earlier source (source_in decreases).
            let d = resolved_tick(state.grab_tick + delta_raw, state, snap, frame_rate, view)
                - state.grab_tick;
            let new_source_in = state.orig.source_in - d;
            if new_source_in.0 >= 0 {
                ops_bridge::slip(doc, history, seq_id, state.track, state.clip, new_source_in);
            }
        }
        DragKind::Slide => {
            let new_start =
                resolved_tick(state.orig.start + delta_raw, state, snap, frame_rate, view);
            let delta = new_start - state.orig.start;
            if delta.0 != 0 {
                ops_bridge::slide(doc, history, seq_id, state.track, state.clip, delta);
            }
        }
    }
}

/// Resolve what Replace With Clip (G-5, gap G5 residual) would swap onto a
/// right-clicked clip: an armed 3-point source (Match Frame `F`, or any other
/// `pending_source` arming, spec 16 §4) wins when present, since it carries a
/// specific trim range; otherwise the media-pool selection replaces from the
/// asset's head (`source_in` left `None` — the op keeps the clip's own
/// in-point). `None` when neither is armed, so the context-menu entry
/// disables itself. The `String` is the armed source's display name, shown
/// in the entry's tooltip.
fn replace_source_candidate(
    doc: &Document,
    pending: Option<&interact::PendingSource>,
    media_selected: Option<AssetId>,
) -> Option<(ClipSource, Option<Tick>, String)> {
    if let Some(ps) = pending {
        return Some((ps.source.clone(), Some(ps.src_in), ps.name.clone()));
    }
    let asset = doc.timeline.as_ref()?.media.assets.get(&media_selected?)?;
    Some((
        ops_bridge::clip_source_for_asset(asset),
        None,
        crate::panels::media_pool::asset_display_name(asset),
    ))
}

#[allow(clippy::too_many_arguments)]
fn clip_context_menu(
    ui: &mut egui::Ui,
    doc: &mut Document,
    history: &mut CommandHistory,
    seq_id: SequenceId,
    target: Option<(TrackId, ClipId)>,
    gap_target: Option<(TrackId, Tick)>,
    playhead: Tick,
    selection: &[ClipId],
    replace_candidate: Option<(ClipSource, Option<Tick>, String)>,
    open_edit_duration: &mut Option<(TrackId, ClipId)>,
) {
    let Some((track, clip)) = target else {
        // No clip under the cursor — offer Close Gap when it is over a closeable
        // gap (spec 17 G1), else nothing actionable.
        if let Some((gtrack, gat)) = gap_target {
            if ops_bridge::gap_at(doc, seq_id, gtrack, gat) {
                if ui.button("Close gap").clicked() {
                    ops_bridge::close_gap_at(doc, history, seq_id, gtrack, gat);
                    ui.close_menu();
                }
                return;
            }
        }
        ui.label("No clip");
        return;
    };
    let (inside, enabled, current_label) = doc
        .timeline
        .as_ref()
        .and_then(|p| p.sequences.get(&seq_id))
        .and_then(|s| s.track(track))
        .and_then(|t| t.clips.iter().find(|c| c.id == clip))
        .map(|c| {
            (
                playhead > c.start && playhead < c.end(),
                c.enabled,
                c.color_label,
            )
        })
        .unwrap_or((false, true, None));

    if ui
        .add_enabled(inside, egui::Button::new("Split at playhead"))
        .clicked()
    {
        ops_bridge::split(doc, history, seq_id, track, clip, playhead);
        ui.close_menu();
    }
    if ui.button("Delete").clicked() {
        ops_bridge::remove_clip(doc, history, seq_id, track, clip);
        ui.close_menu();
    }
    if ui.button("Ripple delete").clicked() {
        ops_bridge::ripple_delete(doc, history, seq_id, track, clip);
        ui.close_menu();
    }
    if ui.button("Precision trim… · Shift+T").clicked() {
        ui.data_mut(|data| {
            data.insert_temp(
                egui::Id::new("nested_precision_request"),
                (clip, true, None::<Tick>),
            )
        });
        ui.close_menu();
    }
    let nested = doc
        .timeline
        .as_ref()
        .and_then(|p| p.sequences.get(&seq_id))
        .and_then(|s| s.track(track))
        .and_then(|t| t.clips.iter().find(|c| c.id == clip))
        .is_some_and(|c| matches!(c.source, ClipSource::NestedSequence { .. }));
    if nested && ui.button("Enter nested sequence · Alt+→").clicked() {
        ui.data_mut(|data| {
            data.insert_temp(
                egui::Id::new("nested_precision_request"),
                (clip, false, None::<Tick>),
            )
        });
        ui.close_menu();
    }
    // K-A6: frame-accurate position / in / out / duration form.
    if ui
        .button("Edit duration…")
        .on_hover_text("Set position, source in/out, and duration numerically (K-A6)")
        .clicked()
    {
        *open_edit_duration = Some((track, clip));
        ui.close_menu();
    }
    // K-B14: freeze the source frame under the playhead (or the clip's first
    // frame if the playhead is outside it) for the clip's whole duration.
    if ui
        .button("Freeze frame")
        .on_hover_text(
            "Hold the source frame at the playhead for this clip's duration (zero speed; K-B14)",
        )
        .clicked()
    {
        ui.data_mut(|d| {
            d.insert_temp(egui::Id::new("k_b14_freeze_request"), (seq_id, track, clip));
        });
        ui.close_menu();
    }
    // K-A7: keyboard grab (Shift+G) — also offered here for discoverability.
    if ui
        .button("Grab item")
        .on_hover_text("Keyboard move: arrows nudge, Enter commits, Esc cancels (K-A7 / Shift+G)")
        .clicked()
    {
        // Seed via the same session type as Shift+G; parent reads this through
        // a side-channel on PhotonicApp after the menu closes.
        ui.data_mut(|d| {
            d.insert_temp(egui::Id::new("k_a7_grab_request"), (seq_id, track, clip));
        });
        ui.close_menu();
    }
    let toggle_label = if enabled { "Disable" } else { "Enable" };
    if ui.button(toggle_label).clicked() {
        ops_bridge::set_clip_enabled(doc, history, seq_id, track, clip, !enabled);
        ui.close_menu();
    }
    // Organizational color label (14-nle-parity §M-1, gap #7's UI half) —
    // sets `Clip::color_label` via `ops_bridge::set_clip_color_label`, never
    // mutating `doc.timeline` directly (module-doc rule at the top of this
    // file). Swatches painted by `clips::paint_lane` from the same palette.
    ui.menu_button("Label", |ui| {
        if ui
            .selectable_label(current_label.is_none(), "No label")
            .clicked()
        {
            ops_bridge::set_clip_color_label(doc, history, seq_id, track, clip, None);
            ui.close_menu();
        }
        ui.separator();
        for (idx, (name, color)) in clips::CLIP_LABEL_SWATCHES.iter().enumerate() {
            let selected = current_label == Some(idx as u8);
            ui.horizontal(|ui| {
                let (swatch_rect, _) =
                    ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                ui.painter()
                    .rect_filled(swatch_rect, egui::Rounding::same(2.0), *color);
                if ui.selectable_label(selected, *name).clicked() {
                    ops_bridge::set_clip_color_label(
                        doc,
                        history,
                        seq_id,
                        track,
                        clip,
                        Some(idx as u8),
                    );
                    ui.close_menu();
                }
            });
        }
    });
    // Replace With Clip (G-5, Premiere, gap G5 residual): swap this clip's
    // SOURCE for whatever is armed — a Match-Frame source (`F`) wins over a
    // media-pool selection (`replace_source_candidate` picks one), keeping
    // start/duration/effects/grade untouched (`ops_bridge::replace_clip_source`,
    // one undo step). Disabled with a tooltip when nothing is armed.
    let replace_hint = replace_candidate
        .as_ref()
        .map(|(_, _, name)| format!("Swap this clip's source for \"{name}\""));
    let replace_btn = ui.add_enabled(
        replace_candidate.is_some(),
        egui::Button::new("Replace with clip"),
    );
    let replace_btn = match &replace_hint {
        Some(hint) => replace_btn.on_hover_text(hint),
        None => replace_btn.on_disabled_hover_text(
            "Arm a source first: Match Frame (F) on a timeline clip, or select an asset in the media pool",
        ),
    };
    if replace_btn.clicked() {
        if let Some((source, source_in, _)) = replace_candidate {
            ops_bridge::replace_clip_source(doc, history, seq_id, track, clip, source, source_in);
        }
        ui.close_menu();
    }
    ui.separator();
    // Link Clips / Unlink (14-nle-parity gap #8's creation half). Acts on the
    // current multi-selection when the right-clicked clip is part of it
    // (right-click any selected clip to act on the whole group, matching
    // reference-NLE convention), else on just the hovered clip alone.
    let link_targets = context_targets(selection, clip);
    let can_link = link_targets.len() >= 2;
    if ui
        .add_enabled(can_link, egui::Button::new("Link Clips"))
        .on_hover_text("Group the selected clips so a future move carries them together")
        .clicked()
    {
        link_selected_clips(doc, history, seq_id, &link_targets);
        ui.close_menu();
    }
    let any_linked = clips_have_link_group(doc, seq_id, &link_targets);
    if ui
        .add_enabled(any_linked, egui::Button::new("Unlink"))
        .on_hover_text("Clear link groups on the selection only")
        .clicked()
    {
        unlink_selected_clips(doc, history, seq_id, &link_targets);
        ui.close_menu();
    }
    if ui
        .add_enabled(any_linked, egui::Button::new("Split A/V"))
        .on_hover_text(
            "K-A13: detach the full A/V link group so audio and video can move independently",
        )
        .clicked()
    {
        split_av_selected_clips(doc, history, seq_id, &link_targets);
        ui.close_menu();
    }
    ui.separator();
    ui.add_enabled(false, egui::Button::new("Add transition in"))
        .on_disabled_hover_text("P6");
    ui.add_enabled(false, egui::Button::new("Add transition out"))
        .on_disabled_hover_text("P6");
    ui.add_enabled(false, egui::Button::new("Open as node composition"))
        .on_disabled_hover_text("P8");
}

/// Resolve a right-click on `target` to the set of clip ids an action should
/// apply to: the whole multi-selection when `target` is part of it (so
/// right-clicking any selected clip acts on the group), else just `target`
/// alone.
fn context_targets(selection: &[ClipId], target: ClipId) -> Vec<ClipId> {
    if selection.contains(&target) {
        selection.to_vec()
    } else {
        vec![target]
    }
}

/// Resolve clip ids to their `(TrackId, ClipId)` within `seq_id`, scanning
/// every track once. An id that doesn't resolve (stale selection) is skipped.
fn resolve_clip_locations(
    doc: &Document,
    seq_id: SequenceId,
    ids: &[ClipId],
) -> Vec<(TrackId, ClipId)> {
    let Some(seq) = doc.timeline.as_ref().and_then(|p| p.sequences.get(&seq_id)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for t in seq.tracks() {
        for c in &t.clips {
            if ids.contains(&c.id) {
                out.push((t.id, c.id));
            }
        }
    }
    out
}

/// Whether any of `ids` currently belongs to a link group (gates the
/// "Unlink" menu entry).
fn clips_have_link_group(doc: &Document, seq_id: SequenceId, ids: &[ClipId]) -> bool {
    let Some(seq) = doc.timeline.as_ref().and_then(|p| p.sequences.get(&seq_id)) else {
        return false;
    };
    seq.tracks()
        .flat_map(|t| t.clips.iter())
        .any(|c| ids.contains(&c.id) && c.link_group.is_some())
}

/// Link every clip in `ids` into one link group (14-nle-parity §M-2's
/// "creation half" — see the module-doc exception at the top of this file for
/// why this calls `ops::set_clip_prop` directly rather than through
/// `ops_bridge.rs`). Reuses an existing group if any targeted clip already
/// belongs to one (mirroring `ops::link_clips`'s own resolution for a pair,
/// extended to N clips — calling `link_clips` itself repeatedly wouldn't
/// work here since each call reads the *original*, not-yet-applied project
/// state, so a 3rd+ clip would mint its own group instead of joining the
/// first pair's). One undo step; a no-op below 2 resolved clips.
fn link_selected_clips(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq_id: SequenceId,
    ids: &[ClipId],
) {
    let locations = resolve_clip_locations(doc, seq_id, ids);
    if locations.len() < 2 {
        return;
    }
    let Some(p) = doc.timeline.as_ref() else {
        return;
    };
    let Some(seq) = p.sequences.get(&seq_id) else {
        return;
    };
    let mut group = None;
    for &(track, clip) in &locations {
        if let Some(g) = seq
            .track(track)
            .and_then(|t| t.clips.iter().find(|c| c.id == clip))
            .and_then(|c| c.link_group)
        {
            group = Some(g);
            break;
        }
    }
    let group = group.unwrap_or_else(LinkGroupId::new);
    let mut cmds = Vec::new();
    for &(track, clip) in &locations {
        let Some(c) = seq
            .track(track)
            .and_then(|t| t.clips.iter().find(|c| c.id == clip))
        else {
            continue;
        };
        if c.link_group == Some(group) {
            continue; // already in the target group
        }
        let mut new = c.clone();
        new.link_group = Some(group);
        if let Ok(cmd) = ops::set_clip_prop(p, seq_id, track, new) {
            cmds.push(cmd);
        }
    }
    if !cmds.is_empty() {
        let batch = cmds.into_iter().map(Command::Timeline).collect();
        history.execute_discrete(Command::Batch(batch), doc);
    }
}

/// Unlink every clip in `ids` that currently belongs to a link group (clips
/// with no group are skipped rather than emitting a no-op command). One undo
/// step covering however many clips actually change.
fn unlink_selected_clips(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq_id: SequenceId,
    ids: &[ClipId],
) {
    let locations = resolve_clip_locations(doc, seq_id, ids);
    if locations.is_empty() {
        return;
    }
    let Some(p) = doc.timeline.as_ref() else {
        return;
    };
    let Some(seq) = p.sequences.get(&seq_id) else {
        return;
    };
    let mut cmds = Vec::new();
    for (track, clip) in locations {
        let is_linked = seq
            .track(track)
            .and_then(|t| t.clips.iter().find(|c| c.id == clip))
            .is_some_and(|c| c.link_group.is_some());
        if !is_linked {
            continue;
        }
        if let Ok(cmd) = ops::unlink_clip(p, seq_id, track, clip) {
            cmds.push(cmd);
        }
    }
    if !cmds.is_empty() {
        let batch = cmds.into_iter().map(Command::Timeline).collect();
        history.execute_discrete(Command::Batch(batch), doc);
    }
}

/// K-A13: split A/V link for each selected linked clip — unlinks the **entire**
/// link group (partners included), so audio and video become independent.
/// Distinct from `unlink_selected_clips`, which only clears the selection.
fn split_av_selected_clips(
    doc: &mut Document,
    history: &mut CommandHistory,
    seq_id: SequenceId,
    ids: &[ClipId],
) {
    let locations = resolve_clip_locations(doc, seq_id, ids);
    if locations.is_empty() {
        return;
    }
    let Some(p) = doc.timeline.as_ref() else {
        return;
    };
    let mut seen_groups = std::collections::HashSet::new();
    let mut cmds = Vec::new();
    for (track, clip) in locations {
        let Ok(batch) = ops::split_av_link(p, seq_id, track, clip) else {
            continue;
        };
        // Dedup: same group from multi-select yields identical unlink cmds.
        if batch.is_empty() {
            continue;
        }
        // Use first command's clip as a coarse key — split_av returns one
        // unlink per member; hashing command count+track is enough for tests.
        let key = (track, clip);
        if !seen_groups.insert(key) {
            continue;
        }
        // If two selected clips share a group, second split is empty after first
        // apply — so collect first, apply once. Here we only collect unique
        // non-empty batches by group identity via first clip id in batch.
        cmds.extend(batch);
    }
    // Dedup so multi-select of partners doesn't double-apply unlinks.
    let mut seen = std::collections::HashSet::new();
    cmds.retain(|c| match c {
        photonic_core::timeline::TimelineCmd::SetClipProp { track, new, .. } => {
            seen.insert((*track, new.id))
        }
        _ => true,
    });
    if !cmds.is_empty() {
        let batch = cmds.into_iter().map(Command::Timeline).collect();
        history.execute_discrete(Command::Batch(batch), doc);
    }
}

#[cfg(test)]
mod media_drop_preview_tests {
    use super::{start_dropped_media_preview, PhotonicApp, Tick};

    #[test]
    fn successful_drop_starts_forward_preview_at_the_clip_start() {
        let mut app = PhotonicApp::default();
        app.monitor_play_reverse = true;
        app.monitor_play_speed = 4.0;

        start_dropped_media_preview(&mut app, Tick(42_000));

        assert_eq!(app.playhead, Tick(42_000));
        assert!(app.monitor_playing);
        assert!(!app.monitor_play_reverse);
        assert_eq!(app.monitor_play_speed, 1.0);
    }
}

#[cfg(test)]
mod panel_layout_tests {
    use super::put_fixed;

    fn screen() -> egui::Rect {
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 1000.0))
    }

    fn frame(ctx: &egui::Context, events: Vec<egui::Event>) -> f32 {
        let input = egui::RawInput {
            screen_rect: Some(screen()),
            events,
            ..Default::default()
        };
        let mut height = 0.0;
        let _ = ctx.run(input, |ctx| {
            let response = egui::TopBottomPanel::bottom("resizable_timeline_test")
                .resizable(true)
                .default_height(220.0)
                .min_height(120.0)
                .show(ctx, |ui| {
                    let full = ui.max_rect();
                    ui.set_min_size(full.size());
                    // This models the timeline's fixed-position controls.
                    put_fixed(
                        ui,
                        egui::Rect::from_min_size(
                            egui::pos2(full.left(), full.bottom() - 20.0),
                            egui::vec2(100.0, 20.0),
                        ),
                        egui::Label::new("control"),
                    );
                });
            height = response.response.rect.height();
        });
        height
    }

    #[test]
    fn fixed_canvas_does_not_block_the_timeline_splitter() {
        let ctx = egui::Context::default();
        let initial = frame(&ctx, vec![]);
        for _ in 0..5 {
            assert!(
                (frame(&ctx, vec![]) - initial).abs() < 0.1,
                "fixed controls must not feed their layout height back into the dock"
            );
        }
        let splitter_y = screen().bottom() - initial;
        let modifiers = egui::Modifiers::default();
        frame(
            &ctx,
            vec![
                egui::Event::PointerMoved(egui::pos2(600.0, splitter_y)),
                egui::Event::PointerButton {
                    pos: egui::pos2(600.0, splitter_y),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers,
                },
            ],
        );
        let grown = frame(
            &ctx,
            vec![egui::Event::PointerMoved(egui::pos2(
                600.0,
                splitter_y - 100.0,
            ))],
        );
        assert!(
            grown > initial + 50.0,
            "dragging the dock splitter should grow the timeline: initial={initial}, grown={grown}"
        );
    }
}

#[cfg(test)]
mod drag_preview_geometry_tests {
    use super::{lane_rect_for_track, tracks, TimelineView};
    use photonic_core::timeline::{TrackId, TrackKind};

    #[test]
    fn drag_preview_is_limited_to_the_destination_track_row() {
        let first = TrackId::new();
        let destination = TrackId::new();
        let rows = [
            tracks::TrackRow {
                id: first,
                kind: TrackKind::Video,
                height: 40.0,
                locked: false,
                index_in_kind: 0,
                count_in_kind: 2,
            },
            tracks::TrackRow {
                id: destination,
                kind: TrackKind::Video,
                height: 60.0,
                locked: false,
                index_in_kind: 1,
                count_in_kind: 2,
            },
        ];
        let lanes = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(500.0, 180.0));

        let rect = lane_rect_for_track(&rows, lanes, &TimelineView::default(), destination)
            .expect("destination row exists");

        assert_eq!(rect.top(), 40.0);
        assert_eq!(rect.bottom(), 100.0);
        assert_eq!(rect.width(), lanes.width());
    }
}

#[cfg(test)]
mod track_drag_tests {
    use super::{nearest_drop_row, reorder_target_index};
    use photonic_core::timeline::{TrackId, TrackKind};

    // Three video rows as `track_rows` builds them: painted top→bottom in
    // reverse Vec order, so the top row carries the *last* Vec index. Rows are
    // 30px tall starting at y=0. `(id, kind, index_in_kind, top, bot)`. The id
    // is irrelevant to the resolver, so a fresh one per row is fine.
    fn video_geom() -> Vec<(TrackId, TrackKind, usize, f32, f32)> {
        vec![
            (TrackId::new(), TrackKind::Video, 2, 0.0, 30.0), // top row = Vec idx 2
            (TrackId::new(), TrackKind::Video, 1, 30.0, 60.0), // middle = Vec idx 1
            (TrackId::new(), TrackKind::Video, 0, 60.0, 90.0), // bottom = Vec idx 0
        ]
    }

    #[test]
    fn reversed_video_top_row_maps_to_last_vec_index() {
        let geom = video_geom();
        // Pointer over the top row → the raw Vec index handed to reorder_track
        // must be the *last* index (2), not the first, despite the reversal.
        let (idx, top) = nearest_drop_row(&geom, TrackKind::Video, 5.0).unwrap();
        assert_eq!(idx, 2, "top visual row = highest Vec index (reversed lane)");
        assert_eq!(top, 0.0, "indicator sits on the target row's top edge");
    }

    #[test]
    fn reversed_video_bottom_row_maps_to_first_vec_index() {
        let geom = video_geom();
        let (idx, _) = nearest_drop_row(&geom, TrackKind::Video, 85.0).unwrap();
        assert_eq!(idx, 0, "bottom visual row = Vec index 0 (reversed lane)");
    }

    #[test]
    fn nearest_is_by_row_center_not_edge() {
        let geom = video_geom();
        // y=44 is just below the middle row's center (45) → still the middle row.
        let (idx, _) = nearest_drop_row(&geom, TrackKind::Video, 44.0).unwrap();
        assert_eq!(idx, 1);
        // y=61 is nearest the bottom row's center (75)? center distances: mid
        // center 45 (|61-45|=16) vs bottom center 75 (|61-75|=14) → bottom wins.
        let (idx, _) = nearest_drop_row(&geom, TrackKind::Video, 61.0).unwrap();
        assert_eq!(idx, 0);
    }

    #[test]
    fn filters_to_the_dragged_rows_own_kind() {
        // A mixed lane: dragging an audio row must never resolve onto a video row
        // even when a video row is vertically closer to the pointer.
        let geom = vec![
            (TrackId::new(), TrackKind::Video, 1, 0.0, 30.0),
            (TrackId::new(), TrackKind::Video, 0, 30.0, 60.0),
            (TrackId::new(), TrackKind::Audio, 0, 60.0, 90.0),
        ];
        // Pointer at y=40 is inside the video middle row, but for an audio drag
        // the only candidate is the audio row (idx 0).
        let (idx, _) = nearest_drop_row(&geom, TrackKind::Audio, 40.0).unwrap();
        assert_eq!(idx, 0);
    }

    #[test]
    fn no_same_kind_rows_yields_none() {
        let geom = vec![(TrackId::new(), TrackKind::Video, 0, 0.0, 30.0)];
        assert!(nearest_drop_row(&geom, TrackKind::Audio, 10.0).is_none());
    }

    #[test]
    fn reversed_video_drag_above_target_uses_post_removal_raw_index() {
        // Raw [A,B,C] is painted [C,B,A]. Drag visible A above visible C.
        assert_eq!(
            reorder_target_index(TrackKind::Video, 0, 2, 3),
            Some(2),
            "A must append after removing itself so it becomes the top visual row"
        );
    }

    #[test]
    fn reversed_video_drag_below_target_maps_visual_boundary_back_to_raw_vec() {
        // Raw [A,B,C] is painted [C,B,A]. Drag visible C above visible A's row.
        assert_eq!(
            reorder_target_index(TrackKind::Video, 2, 0, 3),
            Some(1),
            "C should be inserted between A and B in raw order"
        );
    }

    #[test]
    fn natural_audio_drag_uses_the_same_visual_insert_contract() {
        assert_eq!(
            reorder_target_index(TrackKind::Audio, 0, 2, 3),
            Some(1),
            "audio A above C inserts between B and C after removal"
        );
        assert_eq!(
            reorder_target_index(TrackKind::Audio, 2, 0, 3),
            Some(0),
            "audio C above A inserts at the front"
        );
    }
}

#[cfg(test)]
mod media_cache_tests {
    use super::TimelineMediaCaches;
    use photonic_core::timeline::{AssetKind, MediaAsset, MediaPool};
    use std::path::PathBuf;

    #[test]
    fn refresh_invalidates_memo_when_media_identity_changes() {
        let mut pool = MediaPool::default();
        let asset = MediaAsset::from_file(AssetKind::Video, "/old/clip.mp4");
        let asset_id = asset.id;
        pool.insert(asset.clone());

        let mut caches = TimelineMediaCaches::new(None, 0);
        caches.refresh(None, 0, &pool);
        caches.thumb_memo.lock().unwrap().insert(asset_id, None);

        let mut relinked = asset;
        relinked.source = photonic_core::timeline::AssetSource::File {
            path: PathBuf::from("/new/clip.mp4"),
            rel_path: None,
        };
        relinked.content_hash = Some("new-bytes".into());
        pool.assets.insert(asset_id, relinked);
        caches.refresh(None, 0, &pool);

        assert!(!caches.thumb_memo.lock().unwrap().contains_key(&asset_id));
    }

    #[test]
    fn refresh_invalidates_memo_when_active_tab_changes() {
        let pool = MediaPool::default();
        let mut caches = TimelineMediaCaches::new(None, 0);
        caches.refresh(None, 0, &pool);
        let asset_id = photonic_core::timeline::AssetId::new();
        caches.thumb_memo.lock().unwrap().insert(asset_id, None);

        caches.refresh(None, 1, &pool);

        assert!(!caches.thumb_memo.lock().unwrap().contains_key(&asset_id));
    }
}
