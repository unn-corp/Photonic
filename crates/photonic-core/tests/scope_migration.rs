//! End-to-end coverage for the spec-35 scope / marker / group model shipped as
//! the v4 → v5 format step (see `migration::V4ToV5` and
//! docs/format-versions.md). Exercises two things at the crate boundary:
//!
//! 1. The additive effect/grade scope fields on `Track`, `Sequence` (master) and
//!    `MediaAsset` round-trip through serde and stay absent when neutral (§2.6).
//! 2. The deprecated `link_group` → `GroupKind::AvLink` projection migrates a v4
//!    document into a v5 group tree that deserializes and passes `finalize_load`.
//!
//! The projection is driven through `run_migrations` with an explicit target of
//! 5 so the coverage does not depend on `CURRENT_FORMAT_VERSION` (the const bump
//! is coordinated across the sibling sections that share this format step).

use photonic_core::layer::BlendMode;
use photonic_core::migration::run_migrations;
use photonic_core::timeline::clip::LinkGroupId;
use photonic_core::timeline::load::finalize_load;
use photonic_core::timeline::{
    AssetKind, Clip, ClipEffect, ClipSource, EffectKind, FrameRate, GroupKind, MediaAsset,
    Sequence, Tick, TimelineProject, Track, TrackKind,
};
use serde_json::json;

/// A `Track`, `Sequence`-master and `MediaAsset` carrying effect scopes round-trip
/// losslessly, and the neutral defaults are omitted from JSON (35 §2 / §2.6).
#[test]
fn scope_fields_round_trip_and_are_absent_when_neutral() {
    // Neutral case: the empty stacks are omitted (skip_serializing_if). `blend`
    // and `opacity` are always-present live fields (§2 / 26 K-A9), so they stay
    // in the JSON at their neutral values.
    let neutral = Track::new(TrackKind::Video, "V1");
    let json = serde_json::to_string(&neutral).unwrap();
    for key in ["effects", "grade"] {
        assert!(!json.contains(key), "neutral track must omit {key:?}");
    }
    assert_eq!(neutral.blend, BlendMode::Normal);
    assert_eq!(neutral.opacity, 1.0);

    // Populated case: a non-default blend/opacity and an effect/grade scope on a
    // track, a sequence master, and an asset all survive a serde round-trip.
    let mut track = Track::new(TrackKind::Video, "V1");
    track.effects.push(ClipEffect::new(EffectKind::Blur));
    track.blend = BlendMode::Screen;
    track.opacity = 0.5;
    let back: Track = serde_json::from_str(&serde_json::to_string(&track).unwrap()).unwrap();
    assert_eq!(back, track);
    assert_eq!(back.blend, BlendMode::Screen);
    assert_eq!(back.opacity, 0.5);
    assert_eq!(back.effects.len(), 1);

    let mut seq = Sequence::new("seq", FrameRate::FPS_30, 1920, 1080);
    seq.master_effects.push(ClipEffect::new(EffectKind::Glow));
    let seq_back: Sequence = serde_json::from_str(&serde_json::to_string(&seq).unwrap()).unwrap();
    assert_eq!(seq_back.master_effects.len(), 1);

    let mut asset = MediaAsset::from_file(AssetKind::Video, "/tmp/a.mp4");
    asset.effects.push(ClipEffect::new(EffectKind::Sharpen));
    let asset_back: MediaAsset =
        serde_json::from_str(&serde_json::to_string(&asset).unwrap()).unwrap();
    assert_eq!(asset_back.effects.len(), 1);
}

/// A v4 document whose video + audio clips share one `link_group` migrates to a
/// v5 group tree: one `GroupKind::AvLink` group both clips point at, which loads
/// cleanly through `finalize_load` (35 §3).
#[test]
fn link_group_projection_produces_a_loadable_avlink_group() {
    let lg = LinkGroupId::new();
    let mut seq = Sequence::new("seq", FrameRate::FPS_30, 1920, 1080);

    let mut vt = Track::new(TrackKind::Video, "V1");
    let mut vc = Clip::new(ClipSource::Adjustment, Tick(0), Tick(100));
    vc.link_group = Some(lg);
    let vclip_id = vc.id;
    vt.clips.push(vc);
    seq.video_tracks.push(vt);

    let mut at = Track::new(TrackKind::Audio, "A1");
    let mut ac = Clip::new(ClipSource::Adjustment, Tick(0), Tick(100));
    ac.link_group = Some(lg);
    let aclip_id = ac.id;
    at.clips.push(ac);
    seq.audio_tracks.push(at);

    let mut project = TimelineProject::new();
    project.insert_sequence(seq);

    // Wrap the project as a raw v4 document and migrate it forward to v5.
    let mut document = json!({
        "format_version": 4,
        "timeline": serde_json::to_value(&project).unwrap(),
    });
    assert_eq!(run_migrations(&mut document, 5).unwrap(), 5);

    // The migrated timeline deserializes and finalizes without rejection.
    let mut migrated: TimelineProject =
        serde_json::from_value(document["timeline"].clone()).unwrap();
    let report = finalize_load(&mut migrated).expect("projected AvLink groups must load");
    assert!(
        report.dissolved_groups.is_empty(),
        "an AvLink group is never dissolved, even as a pair"
    );

    let seq = migrated.sequences.values().next().unwrap();
    assert_eq!(seq.groups.len(), 1, "one link_group → one AvLink group");
    let (gid, node) = seq.groups.iter().next().unwrap();
    assert_eq!(node.kind, GroupKind::AvLink);

    let vclip_group = seq.video_tracks[0]
        .clips
        .iter()
        .find(|c| c.id == vclip_id)
        .unwrap()
        .group;
    let aclip_group = seq.audio_tracks[0]
        .clips
        .iter()
        .find(|c| c.id == aclip_id)
        .unwrap()
        .group;
    assert_eq!(vclip_group, Some(*gid));
    assert_eq!(aclip_group, Some(*gid), "both clips bind to the same group");

    // The deprecated link_group is retained for one format version.
    assert_eq!(
        seq.video_tracks[0]
            .clips
            .iter()
            .find(|c| c.id == vclip_id)
            .unwrap()
            .link_group,
        Some(lg)
    );
}
