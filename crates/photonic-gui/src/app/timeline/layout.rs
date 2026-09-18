//! Timeline zoom/scroll session state (video-editor-module `04-ui-mode-timeline.md`
//! §2.1). Types + tick↔pixel mapping only — no drawing here (that's `ruler.rs`,
//! `tracks.rs`, `clips.rs`).

use photonic_core::timeline::{Tick, TICKS_PER_SECOND};

/// Default zoom: 100 screen px per second of timeline.
const DEFAULT_PIXELS_PER_SECOND: f64 = 100.0;

/// Zoom floor: ~2 px per second (a whole 10-min sequence fits a wide panel).
pub const MIN_PIXELS_PER_TICK: f64 = 2.0 / TICKS_PER_SECOND as f64;
/// Zoom ceiling: ~20 000 px per second (individual frames are wide apart).
pub const MAX_PIXELS_PER_TICK: f64 = 20_000.0 / TICKS_PER_SECOND as f64;
/// Multiplicative step for one `+`/`-` press or one Ctrl+scroll notch.
pub const ZOOM_STEP: f64 = 1.25;

/// Track-header column width bounds (04 §2.1, mirrors the drawer-width clamp).
pub const HEADER_MIN_PX: f32 = 120.0;
pub const HEADER_MAX_PX: f32 = 320.0;
pub const HEADER_DEFAULT_PX: f32 = 160.0;

/// Ruler strip height (04 §2.1).
pub const RULER_HEIGHT_PX: f32 = 24.0;
/// Trim-handle *painted* width at each clip edge (04 §2.3, 13 §1.1 "6px
/// trim-handle hit zones ... invisible until hover"). This is the design size;
/// the pointer target is the wider [`EDGE_HIT_PX`].
pub const EDGE_ZONE_PX: f32 = 6.0;
/// Trim-handle *hit* width (210 §5, 41 R-9). WCAG 2.2 SC 2.5.8 asks for
/// 24 x 24 logical px; 41 §4 rules trim handles down to **12 x 24** — "paint
/// 6px, hit 12 x 24", discharged further by the `,` `.` `[` `]` keyboard
/// equivalents. The 24px vertical half needs no code: a clip rect is a track
/// row tall and `ops_bridge::set_track_height` clamps that to `28.0..=240.0`.
/// `interact::hit_zone` clamps this back down on narrow clips so widening the
/// handle can never swallow the body (move) zone.
pub const EDGE_HIT_PX: f32 = 12.0;
/// Snap magnet threshold in *screen* pixels, converted to ticks via zoom (04 §2.5).
pub const SNAP_THRESHOLD_PX: f32 = 8.0;
/// K-A10: how far from a lane edge (px) starts continuous auto-pan while dragging.
pub const EDGE_AUTO_PAN_ZONE_PX: f32 = 32.0;
/// K-A10: max scroll speed (screen px/frame) when the pointer is flush with the edge.
pub const EDGE_AUTO_PAN_MAX_PX: f32 = 24.0;

/// Timeline panel zoom/scroll — GUI session state (04 §6), not document state.
/// `pixels_per_tick` is the one deliberate `f64` time value in the video-editor
/// surface: 01 §1 bans `f32`/`f64` time in the *data model*, but this struct is
/// GUI-only and converts at the edge, per that rule's own carve-out.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimelineView {
    /// Zoom: screen pixels per tick. Clamped to `[MIN_PIXELS_PER_TICK, MAX_PIXELS_PER_TICK]`.
    pub pixels_per_tick: f64,
    /// Leftmost visible tick.
    pub scroll_ticks: Tick,
    /// Vertical scroll across track rows, in pixels.
    pub track_scroll_px: f32,
    /// Track-header column width (px). Persisted-but-session, clamped on set.
    pub header_width_px: f32,
    /// Lane width painted on the previous frame — the anchor `zoom_fit`/scroll
    /// clamping need but which is only known inside `draw`. Cached here so the
    /// `pub(crate)` command methods (04 §6) can reach it without threading a
    /// rect through command dispatch.
    pub last_lane_width_px: f32,
    /// K-A10: when true during playback, keep the playhead centred and scroll
    /// the timeline underneath (fixed-playhead mode).
    pub fixed_playhead: bool,
}

impl Default for TimelineView {
    fn default() -> Self {
        Self {
            pixels_per_tick: DEFAULT_PIXELS_PER_SECOND / TICKS_PER_SECOND as f64,
            scroll_ticks: Tick::ZERO,
            track_scroll_px: 0.0,
            header_width_px: HEADER_DEFAULT_PX,
            last_lane_width_px: 0.0,
            fixed_playhead: false,
        }
    }
}

impl TimelineView {
    /// Map a tick to an x coordinate within a lane whose left edge is at
    /// `lane_left_px` (04 §2.1).
    pub fn tick_to_x(&self, t: Tick, lane_left_px: f32) -> f32 {
        lane_left_px + ((t.0 - self.scroll_ticks.0) as f64 * self.pixels_per_tick) as f32
    }

    /// Inverse of [`Self::tick_to_x`].
    pub fn x_to_tick(&self, x: f32, lane_left_px: f32) -> Tick {
        Tick(((x - lane_left_px) as f64 / self.pixels_per_tick) as i64 + self.scroll_ticks.0)
    }

    /// Width in pixels of a tick span at the current zoom.
    pub fn ticks_to_px(&self, ticks: Tick) -> f32 {
        (ticks.0 as f64 * self.pixels_per_tick) as f32
    }

    /// Convert a screen-pixel distance to a tick distance at the current zoom
    /// (used for the constant-feel snap threshold, 04 §2.5).
    pub fn px_to_ticks(&self, px: f32) -> Tick {
        Tick((px as f64 / self.pixels_per_tick) as i64)
    }

    /// Clamp the header width to its bounds (04 §2.1).
    pub fn set_header_width(&mut self, w: f32) {
        self.header_width_px = w.clamp(HEADER_MIN_PX, HEADER_MAX_PX);
    }

    fn clamp_zoom(&mut self) {
        self.pixels_per_tick = self
            .pixels_per_tick
            .clamp(MIN_PIXELS_PER_TICK, MAX_PIXELS_PER_TICK);
        if self.scroll_ticks.0 < 0 {
            self.scroll_ticks = Tick::ZERO;
        }
    }

    /// Multiply the zoom by `factor`, keeping `anchor` tick pinned under its
    /// current x (so Ctrl+scroll zooms toward the cursor, `+`/`-` toward the
    /// playhead). `anchor` is a tick; `lane_left_px` is the lane's left edge.
    pub fn zoom_around(&mut self, factor: f64, anchor: Tick, lane_left_px: f32) {
        let anchor_x = self.tick_to_x(anchor, lane_left_px);
        let old = self.pixels_per_tick;
        self.pixels_per_tick = (old * factor).clamp(MIN_PIXELS_PER_TICK, MAX_PIXELS_PER_TICK);
        // Solve scroll so `anchor` maps back to `anchor_x` at the new zoom.
        let new_scroll = anchor.0 as f64 - (anchor_x - lane_left_px) as f64 / self.pixels_per_tick;
        self.scroll_ticks = Tick(new_scroll.max(0.0) as i64);
        self.clamp_zoom();
    }

    /// Fit `[0, extent]` to `lane_width_px`, resetting scroll to the start
    /// (04 §2.1 zoom-to-fit). A zero/negative extent leaves the view unchanged.
    pub fn fit(&mut self, extent: Tick, lane_width_px: f32) {
        if extent.0 <= 0 || lane_width_px <= 1.0 {
            return;
        }
        // Leave a small right margin so the last clip edge isn't flush.
        let usable = (lane_width_px * 0.98) as f64;
        self.pixels_per_tick = usable / extent.0 as f64;
        self.scroll_ticks = Tick::ZERO;
        self.clamp_zoom();
    }

    /// K-A10: scroll so `playhead` sits at the horizontal centre of the lane.
    pub fn center_on_playhead(&mut self, playhead: Tick, lane_width_px: f32) {
        if lane_width_px <= 1.0 || self.pixels_per_tick <= 0.0 {
            return;
        }
        let half = (lane_width_px as f64 / 2.0) / self.pixels_per_tick;
        let scroll = playhead.0 as f64 - half;
        self.scroll_ticks = Tick(scroll.max(0.0) as i64);
    }

    /// K-A10: while dragging near a viewport edge, pan by `px` (positive =
    /// later times). Returns the tick delta applied to scroll.
    pub fn edge_auto_pan(&mut self, px: f32) -> Tick {
        if px.abs() < 0.5 {
            return Tick::ZERO;
        }
        let delta = self.px_to_ticks(px);
        let next = self.scroll_ticks.0 + delta.0;
        self.scroll_ticks = Tick(next.max(0));
        delta
    }

    /// K-A10: screen-px pan for the current frame given a pointer x over a lane
    /// of width `lane_width_px` whose left edge is at `lane_left_px`. Positive =
    /// later times (scroll right). Zero outside the edge zones.
    pub fn edge_auto_pan_speed(pointer_x: f32, lane_left_px: f32, lane_width_px: f32) -> f32 {
        if lane_width_px <= EDGE_AUTO_PAN_ZONE_PX * 2.0 {
            return 0.0;
        }
        let rel = pointer_x - lane_left_px;
        if rel < EDGE_AUTO_PAN_ZONE_PX {
            let t = (1.0 - rel / EDGE_AUTO_PAN_ZONE_PX).clamp(0.0, 1.0);
            -EDGE_AUTO_PAN_MAX_PX * t
        } else if rel > lane_width_px - EDGE_AUTO_PAN_ZONE_PX {
            let over = rel - (lane_width_px - EDGE_AUTO_PAN_ZONE_PX);
            let t = (over / EDGE_AUTO_PAN_ZONE_PX).clamp(0.0, 1.0);
            EDGE_AUTO_PAN_MAX_PX * t
        } else {
            0.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_pixel_roundtrip_at_default_zoom() {
        let v = TimelineView::default();
        let t = Tick::from_seconds(3);
        let x = v.tick_to_x(t, 100.0);
        let back = v.x_to_tick(x, 100.0);
        // Rounding to whole pixels loses sub-pixel ticks; within one pixel worth.
        assert!((back.0 - t.0).abs() < v.px_to_ticks(1.0).0.max(1));
    }

    #[test]
    fn center_on_playhead_puts_playhead_near_mid() {
        let mut v = TimelineView::default();
        let ph = Tick::from_seconds(10);
        let lane_w = 400.0_f32;
        v.center_on_playhead(ph, lane_w);
        let x = v.tick_to_x(ph, 0.0);
        assert!(
            (x - lane_w / 2.0).abs() < 2.0,
            "playhead should be near centre: x={x}"
        );
    }

    #[test]
    fn edge_auto_pan_moves_scroll_forward() {
        let mut v = TimelineView::default();
        let before = v.scroll_ticks;
        let d = v.edge_auto_pan(50.0);
        assert!(d.0 > 0);
        assert!(v.scroll_ticks.0 > before.0);
    }

    #[test]
    fn edge_auto_pan_speed_left_negative_right_positive() {
        let lane_left = 100.0_f32;
        let lane_w = 400.0_f32;
        let left = TimelineView::edge_auto_pan_speed(lane_left + 2.0, lane_left, lane_w);
        let mid = TimelineView::edge_auto_pan_speed(lane_left + lane_w / 2.0, lane_left, lane_w);
        let right = TimelineView::edge_auto_pan_speed(lane_left + lane_w - 2.0, lane_left, lane_w);
        assert!(left < 0.0, "left edge should pan earlier: {left}");
        assert_eq!(mid, 0.0);
        assert!(right > 0.0, "right edge should pan later: {right}");
    }

    #[test]
    fn zoom_around_keeps_anchor_pinned() {
        let mut v = TimelineView::default();
        let anchor = Tick::from_seconds(5);
        let before_x = v.tick_to_x(anchor, 0.0);
        v.zoom_around(ZOOM_STEP, anchor, 0.0);
        let after_x = v.tick_to_x(anchor, 0.0);
        assert!(
            (before_x - after_x).abs() <= 1.0,
            "anchor drifted: {before_x} vs {after_x}"
        );
    }

    #[test]
    fn zoom_is_clamped() {
        let mut v = TimelineView::default();
        for _ in 0..200 {
            v.zoom_around(ZOOM_STEP, Tick::ZERO, 0.0);
        }
        assert!(v.pixels_per_tick <= MAX_PIXELS_PER_TICK + f64::EPSILON);
        for _ in 0..400 {
            v.zoom_around(1.0 / ZOOM_STEP, Tick::ZERO, 0.0);
        }
        assert!(v.pixels_per_tick >= MIN_PIXELS_PER_TICK - f64::EPSILON);
    }

    #[test]
    fn fit_maps_extent_to_lane_width() {
        let mut v = TimelineView::default();
        let extent = Tick::from_seconds(10);
        v.fit(extent, 1000.0);
        assert_eq!(v.scroll_ticks, Tick::ZERO);
        let end_x = v.tick_to_x(extent, 0.0);
        // Fits within the lane (with the 2% right margin).
        assert!(end_x <= 1000.0 && end_x > 900.0, "end_x = {end_x}");
    }

    #[test]
    fn header_width_clamps() {
        let mut v = TimelineView::default();
        v.set_header_width(40.0);
        assert_eq!(v.header_width_px, HEADER_MIN_PX);
        v.set_header_width(9999.0);
        assert_eq!(v.header_width_px, HEADER_MAX_PX);
    }
}
