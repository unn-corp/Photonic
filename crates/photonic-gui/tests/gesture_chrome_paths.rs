//! Gesture & first-run chrome UI paths (spec 43) + daily transport/edit paths.
//!
//! Locks pure decision/mutation entry points that paint and pointer code depend
//! on, plus high-ROI ops_bridge/command surfaces: Esc cancel, trim hit, snap,
//! K-A10, K-B5 compare, coach, transport registry, split, markers/bookmarks,
//! freeze, alpha view.

#![expect(clippy::assertions_on_constants)]

use photonic_core::history::CommandHistory;
use photonic_core::timeline::{
    Clip, ClipId, ClipSource, FrameRate, Sequence, SequenceId, Tick, TimelineProject, Track,
    TrackId, TrackKind, TICKS_PER_SECOND,
};
use photonic_core::Document;
use photonic_gui::app::engine::EngineBridge;
use photonic_gui::app::timeline::interact::{hit_zone, nearest_snap, should_cancel_drag, ClipZone};
use photonic_gui::app::timeline::layout::{
    TimelineView, EDGE_AUTO_PAN_ZONE_PX, EDGE_HIT_PX, EDGE_ZONE_PX,
};
use photonic_gui::app::timeline::ops_bridge;
use photonic_gui::commands;
use photonic_gui::preferences::{
    coach_advance_button, coach_auto_advance_on_clips, coach_skip_dismisses, AppPreferences,
};

/// Minimal timeline: one video track + one solid adjustment clip (2s).
fn doc_with_clip() -> (Document, CommandHistory, SequenceId, TrackId, ClipId) {
    let mut project = TimelineProject::new();
    let mut seq = Sequence::new("S", FrameRate::FPS_30, 1920, 1080);
    let seq_id = seq.id;
    let mut track = Track::new(TrackKind::Video, "V1");
    let track_id = track.id;
    let clip = Clip::new(ClipSource::Adjustment, Tick(0), Tick::from_seconds(2));
    let clip_id = clip.id;
    track.clips.push(clip);
    seq.video_tracks.push(track);
    project.insert_sequence(seq);
    project.active_sequence = Some(seq_id);
    let mut doc = Document::new("g", 1920.0, 1080.0);
    doc.timeline = Some(project);
    (doc, CommandHistory::new(64), seq_id, track_id, clip_id)
}

// ── 2.1 Esc drag cancel ─────────────────────────────────────────────────────

#[test]
fn esc_cancels_only_when_a_drag_is_active() {
    assert!(!should_cancel_drag(false, false));
    assert!(!should_cancel_drag(true, false), "Esc alone is a no-op");
    assert!(
        !should_cancel_drag(false, true),
        "active drag without Esc stays"
    );
    assert!(
        should_cancel_drag(true, true),
        "Esc + active drag must cancel (no commit_drag)"
    );
}

// ── 2.2 Trim hit target (41 R-9 / 210) ───────────────────────────────────────

#[test]
fn trim_hit_uses_twelve_px_not_paint_six() {
    assert_eq!(EDGE_HIT_PX, 12.0);
    assert_eq!(EDGE_ZONE_PX, 6.0);
    // 10px inside the left edge: Body at paint width, LeftEdge at hit width.
    assert_eq!(
        hit_zone(200.0, 300.0, 210.0, EDGE_ZONE_PX),
        Some(ClipZone::Body)
    );
    assert_eq!(
        hit_zone(200.0, 300.0, 210.0, EDGE_HIT_PX),
        Some(ClipZone::LeftEdge)
    );
}

#[test]
fn widened_trim_handle_preserves_body_zone_for_short_clips() {
    for width in [13.0f32, 20.0, 24.0, 48.0] {
        let (x0, x1) = (0.0, width);
        let mid = width * 0.5;
        assert_eq!(
            hit_zone(x0, x1, mid, EDGE_HIT_PX),
            Some(ClipZone::Body),
            "{width}px clip must stay body-draggable"
        );
    }
}

// ── 2.3 Snap guide only when captured ───────────────────────────────────────

#[test]
fn snap_guide_none_outside_threshold() {
    let value = Tick(1000);
    let candidates = [Tick(1050), Tick(2000)];
    assert_eq!(
        nearest_snap(value, &candidates, Tick(5)),
        None,
        "outside threshold → no guide paint"
    );
    assert_eq!(
        nearest_snap(value, &candidates, Tick(60)),
        Some(Tick(1050)),
        "inside threshold → first priority candidate"
    );
}

// ── 2.4 Fixed playhead + edge pan (K-A10) ───────────────────────────────────

#[test]
fn fixed_playhead_toggles_as_view_state() {
    let mut v = TimelineView::default();
    assert!(!v.fixed_playhead);
    v.fixed_playhead = !v.fixed_playhead;
    assert!(v.fixed_playhead);
    v.fixed_playhead = !v.fixed_playhead;
    assert!(!v.fixed_playhead);
}

#[test]
fn center_on_playhead_and_edge_pan_zones() {
    let mut v = TimelineView::default();
    let ph = Tick::from_seconds(8);
    let lane_w = 500.0_f32;
    v.center_on_playhead(ph, lane_w);
    let x = v.tick_to_x(ph, 0.0);
    assert!(
        (x - lane_w / 2.0).abs() < 3.0,
        "playhead near centre, got x={x}"
    );

    let left = TimelineView::edge_auto_pan_speed(5.0, 0.0, 400.0);
    let mid = TimelineView::edge_auto_pan_speed(200.0, 0.0, 400.0);
    let right = TimelineView::edge_auto_pan_speed(395.0, 0.0, 400.0);
    assert!(left < 0.0);
    assert_eq!(mid, 0.0);
    assert!(right > 0.0);
    assert!(EDGE_AUTO_PAN_ZONE_PX > 0.0);
}

// ── 2.5 Compare effects (K-B5) ──────────────────────────────────────────────

#[test]
fn compare_effects_toggle_flips_bridge_flag() {
    let Some(engine) = photonic_video::VideoEngine::headless() else {
        eprintln!("no GPU adapter — skipping compare_effects toggle");
        return;
    };
    let mut bridge = EngineBridge::new(engine);
    assert!(!bridge.compare_effects());
    bridge.toggle_compare_effects();
    assert!(bridge.compare_effects());
    bridge.toggle_compare_effects();
    assert!(!bridge.compare_effects());
}

#[test]
fn compare_and_fixed_playhead_commands_are_registered() {
    let ids: Vec<&str> = commands::REGISTRY.iter().map(|c| c.id).collect();
    assert!(
        ids.contains(&"video.compare_effects"),
        "K-B5 command missing from REGISTRY"
    );
    assert!(
        ids.contains(&"video.toggle_fixed_playhead"),
        "K-A10 command missing from REGISTRY"
    );
}

// ── 2.6 Social coach (213) ──────────────────────────────────────────────────

#[test]
fn coach_defaults_favour_social_velocity() {
    let p = AppPreferences::default();
    assert!(!p.video_coach_dismissed);
    assert_eq!(p.video_coach_step, 0);
    assert!(p.auto_place_import_on_timeline);
}

#[test]
fn coach_step_machine_import_split_export() {
    assert_eq!(coach_auto_advance_on_clips(0, false), 0);
    assert_eq!(coach_auto_advance_on_clips(0, true), 1);
    assert_eq!(coach_auto_advance_on_clips(1, true), 1);

    assert_eq!(coach_advance_button(0), (1, false));
    assert_eq!(coach_advance_button(1), (2, false));
    assert_eq!(coach_advance_button(2), (2, true));
    assert!(coach_skip_dismisses());
}

// ── Transport / split / razor / snap (command registry + ops paths) ─────────

#[test]
fn transport_split_razor_snap_commands_are_registered() {
    let ids: Vec<&str> = commands::REGISTRY.iter().map(|c| c.id).collect();
    for id in [
        "video.play_pause",
        "video.play_reverse",
        "video.pause",
        "video.play_forward",
        "video.step_back",
        "video.step_forward",
        "video.playhead_home",
        "video.playhead_end",
        "video.set_in",
        "video.set_out",
        "video.split_at_playhead",
        "video.toggle_razor",
        "video.toggle_snap",
        "video.freeze_frame",
        "video.alpha_view",
        "video.add_marker",
        "video.add_bookmark",
    ] {
        assert!(ids.contains(&id), "missing command {id}");
    }
}

#[test]
fn split_at_mid_clip_via_ops_bridge_one_undo() {
    let (mut doc, mut history, seq, track, clip) = doc_with_clip();
    let at = Tick::from_seconds(1);
    assert!(
        ops_bridge::split(&mut doc, &mut history, seq, track, clip, at),
        "split at mid-clip must commit"
    );
    let n = doc
        .timeline
        .as_ref()
        .unwrap()
        .sequences
        .get(&seq)
        .unwrap()
        .track(track)
        .unwrap()
        .clips
        .len();
    assert_eq!(n, 2, "split produces two halves");
    history.undo(&mut doc);
    let n = doc
        .timeline
        .as_ref()
        .unwrap()
        .sequences
        .get(&seq)
        .unwrap()
        .track(track)
        .unwrap()
        .clips
        .len();
    assert_eq!(n, 1, "one undo restores single clip");
}

// ── Markers / bookmarks (210) ───────────────────────────────────────────────

#[test]
fn add_marker_and_bookmark_via_ops_bridge() {
    let (mut doc, mut history, seq, _track, _clip) = doc_with_clip();
    ops_bridge::add_marker(
        &mut doc,
        &mut history,
        seq,
        Tick(TICKS_PER_SECOND / 2),
        "Mark",
    );
    {
        let s = &doc.timeline.as_ref().unwrap().sequences[&seq];
        assert_eq!(s.markers.len(), 1);
        assert_eq!(s.markers[0].name, "Mark");
    }
    history.undo(&mut doc);
    assert!(doc.timeline.as_ref().unwrap().sequences[&seq]
        .markers
        .is_empty());

    ops_bridge::add_bookmark(
        &mut doc,
        &mut history,
        seq,
        Tick::from_seconds(1),
        "Bookmark 1",
    );
    let project = doc.timeline.as_ref().unwrap();
    let cat = project
        .marker_categories
        .iter()
        .find(|c| c.name == photonic_core::timeline::MarkerCategory::BOOKMARKS_CATEGORY_NAME)
        .map(|c| c.id);
    assert!(cat.is_some(), "Bookmarks category must be seeded");
    let s = &project.sequences[&seq];
    assert_eq!(s.markers.len(), 1);
    assert_eq!(s.markers[0].category, cat);
    assert_eq!(s.markers[0].name, "Bookmark 1");
}

// ── Freeze + alpha view ─────────────────────────────────────────────────────

#[test]
fn freeze_frame_via_ops_bridge_one_undo() {
    let (mut doc, mut history, seq, track, clip) = doc_with_clip();
    ops_bridge::freeze_frame(
        &mut doc,
        &mut history,
        seq,
        track,
        clip,
        Tick(TICKS_PER_SECOND / 2),
    );
    let c = doc
        .timeline
        .as_ref()
        .unwrap()
        .sequences
        .get(&seq)
        .unwrap()
        .track(track)
        .unwrap()
        .clips
        .iter()
        .find(|c| c.id == clip)
        .unwrap();
    // Freeze holds via zero-rate speed map (K-B14).
    use photonic_core::timeline::clip::{Ratio, SpeedMap};
    assert_eq!(
        c.speed,
        SpeedMap::Constant(Ratio::new(0, 1)),
        "frozen clip is zero rate"
    );
    history.undo(&mut doc);
    let c = doc
        .timeline
        .as_ref()
        .unwrap()
        .sequences
        .get(&seq)
        .unwrap()
        .track(track)
        .unwrap()
        .clips
        .iter()
        .find(|c| c.id == clip)
        .unwrap();
    assert_eq!(
        c.speed,
        SpeedMap::Constant(Ratio::ONE),
        "undo restores default 1× speed"
    );
}

#[test]
fn alpha_view_toggle_on_engine_bridge() {
    let Some(engine) = photonic_video::VideoEngine::headless() else {
        eprintln!("no GPU adapter — skipping alpha_view toggle");
        return;
    };
    let mut bridge = EngineBridge::new(engine);
    assert!(!bridge.alpha_view());
    bridge.toggle_alpha_view();
    assert!(bridge.alpha_view());
    bridge.toggle_alpha_view();
    assert!(!bridge.alpha_view());
}
