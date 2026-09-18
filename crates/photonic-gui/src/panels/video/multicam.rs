//! `DrawerGroup::Multicam` panel (17-nle-parity-round2.md §G-20) — angle
//! picker + sync controls for a multi-camera source sequence. Rated
//! **Larger** in the round-2 gap list; the real surface (multicam source
//! sequence + audio/timecode/marker sync, live 1-9 angle cutting) is a
//! `photonic-video-engine` + `monitor` build (`crates/photonic-video/src`,
//! `app/monitor.rs`), out of this panel's reach — this left-rail entry exists
//! so the drawer is reachable and its session state
//! (`VideoPanelUi::multicam_active_angle` / `multicam_view_open`) has a
//! stable home while that story is unwritten.
//!
//! Stub — filled by the multicam (17 G-20) panel story.

use crate::panels::PropPanelCtx;
use egui::Ui;

/// Left-rail Multicam drawer.
pub(crate) fn draw_multicam(_ui: &mut Ui, _ctx: &mut PropPanelCtx) {
    // Multicam (17 G-20) panel story fills this.
}
