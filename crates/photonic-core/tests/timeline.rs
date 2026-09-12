//! Integration tests for the timeline data model (P2, docs/specs/video-editor).
//!
//! Covers: undo idempotency (`apply → inverse → apply == apply`) for the
//! `TimelineCmd` variants; per-op invariant enforcement; serde round-trip of a
//! fully-populated `TimelineProject`; the v2→v3 migration (v2 files load
//! unchanged); and a proptest that random edit sequences never break the
//! non-overlap invariant (11 §3.1).

use photonic_core::history::Command;
use photonic_core::timeline::commands::AnimTarget;
use photonic_core::timeline::*;
use photonic_core::Document;

// ── Fixture ─────────────────────────────────────────────────────────────────

/// A document with a small but non-trivial timeline: one sequence, a video
/// track with two clips, and an audio track with one audio-enabled clip.
struct Fixture {
    doc: Document,
    seq: SequenceId,
    vtrack: TrackId,
    atrack: TrackId,
    clip_a: ClipId,
    clip_b: ClipId,
    aclip: ClipId,
}

fn fixture() -> Fixture {
    let mut project = TimelineProject::new();
    let mut sequence = Sequence::new("Seq 1", FrameRate::FPS_30, 1920, 1080);

    let mut vtrack = Track::new(TrackKind::Video, "V1");
    let mut ca = Clip::new(
        ClipSource::SolidColor {
            color: photonic_core::Color {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
        },
        Tick(0),
        Tick(1000),
    );
    ca.name = "A".into();
    let mut cb = Clip::new(
        ClipSource::SolidColor {
            color: photonic_core::Color {
                r: 0.0,
                g: 1.0,
                b: 0.0,
                a: 1.0,
            },
        },
        Tick(1000),
        Tick(1000),
    );
    cb.name = "B".into();
    let (clip_a, clip_b) = (ca.id, cb.id);
    vtrack.clips.push(ca);
    vtrack.clips.push(cb);

    let mut atrack = Track::new(TrackKind::Audio, "A1");
    let mut ac = Clip::new(ClipSource::Adjustment, Tick(0), Tick(2000));
    ac.audio = Some(ClipAudio::new());
    let aclip = ac.id;
    atrack.clips.push(ac);

    let (vt, at) = (vtrack.id, atrack.id);
    sequence.video_tracks.push(vtrack);
    sequence.audio_tracks.push(atrack);
    let seq = sequence.id;
    project.insert_sequence(sequence);

    let mut doc = Document::new("t", 100.0, 100.0);
    doc.timeline = Some(project);
    Fixture {
        doc,
        seq,
        vtrack: vt,
        atrack: at,
        clip_a,
        clip_b,
        aclip,
    }
}

impl Fixture {
    fn project(&self) -> &TimelineProject {
        self.doc.timeline.as_ref().unwrap()
    }
}

/// Assert the undo contract for a single command: applying it, then its inverse,
/// restores the timeline; and applying it again reproduces the post-apply state
/// (`apply → inverse → apply == apply`).
fn assert_undo_roundtrip(doc: &Document, cmd: &TimelineCmd) {
    let before = doc.timeline.clone();

    let mut d1 = doc.clone();
    Command::Timeline(cmd.clone()).apply(&mut d1);
    let after_apply = d1.timeline.clone();

    // inverse restores the original state.
    let inv = cmd
        .inverse(&d1)
        .expect("every implemented variant has an inverse");
    let mut d2 = d1.clone();
    Command::Timeline(inv).apply(&mut d2);
    assert_eq!(
        d2.timeline,
        before,
        "inverse did not restore original for {}",
        cmd.description()
    );

    // Re-applying forward reproduces the post-apply state.
    let mut d3 = d2.clone();
    Command::Timeline(cmd.clone()).apply(&mut d3);
    assert_eq!(
        d3.timeline,
        after_apply,
        "apply→inverse→apply != apply for {}",
        cmd.description()
    );
}

/// Same undo contract as [`assert_undo_roundtrip`], for a batch of timeline
/// commands applied in order (inverse applied in reverse).
fn assert_batch_undo_roundtrip(doc: &Document, cmds: &[TimelineCmd]) {
    let before = doc.timeline.clone();

    let mut d1 = doc.clone();
    for cmd in cmds {
        Command::Timeline(cmd.clone()).apply(&mut d1);
    }
    let after_apply = d1.timeline.clone();

    let mut d2 = d1.clone();
    for cmd in cmds.iter().rev() {
        let inv = cmd
            .inverse(&d2)
            .expect("every implemented variant has an inverse");
        Command::Timeline(inv).apply(&mut d2);
    }
    assert_eq!(
        d2.timeline, before,
        "batch inverse did not restore original"
    );

    let mut d3 = d2.clone();
    for cmd in cmds {
        Command::Timeline(cmd.clone()).apply(&mut d3);
    }
    assert_eq!(
        d3.timeline, after_apply,
        "batch apply→inverse→apply != apply"
    );
}

// ── Undo idempotency across variants ────────────────────────────────────────

#[test]
fn create_and_remove_project_roundtrip() {
    let mut doc = Document::new("t", 100.0, 100.0);
    assert!(doc.timeline.is_none());
    let cmd = ops::create_project();
    assert_undo_roundtrip(&doc, &cmd);
    // Applying it actually creates the project.
    Command::Timeline(cmd).apply(&mut doc);
    assert!(doc.timeline.is_some());
}

#[test]
fn asset_proxy_update_is_undoable() {
    let mut f = fixture();
    let asset = MediaAsset::from_file(AssetKind::Video, "/tmp/proxy-source.mp4");
    let id = asset.id;
    f.doc
        .timeline
        .as_mut()
        .unwrap()
        .media
        .assets
        .insert(id, asset);
    let proxy = ProxyRef::ready_attached("/tmp/proxy-source.proxy.mp4");
    let cmd = ops::set_asset_proxy(f.project(), id, Some(proxy.clone())).unwrap();
    assert_undo_roundtrip(&f.doc, &cmd);

    Command::Timeline(cmd).apply(&mut f.doc);
    assert_eq!(
        f.project()
            .media
            .assets
            .get(&id)
            .and_then(|asset| asset.proxy.clone()),
        Some(proxy)
    );
    assert_eq!(
        f.project()
            .media
            .assets
            .get(&id)
            .and_then(|a| a.proxy.as_ref())
            .map(|p| p.origin),
        Some(photonic_core::timeline::ProxyOrigin::Attached)
    );
}

#[test]
fn clip_edit_ops_roundtrip() {
    let f = fixture();
    let p = f.project();

    let cmds = vec![
        ops::move_clip(p, f.seq, f.vtrack, f.clip_b, Tick(1500)).unwrap(),
        ops::trim_clip(
            p,
            f.seq,
            f.vtrack,
            f.clip_a,
            ClipTiming {
                start: Tick(0),
                duration: Tick(500),
                source_in: Tick(0),
            },
        )
        .unwrap(),
        ops::split_clip(p, f.seq, f.vtrack, f.clip_a, Tick(500)).unwrap(),
        ops::slip_clip(p, f.seq, f.vtrack, f.clip_a, Tick(200)).unwrap(),
        ops::remove_clip(p, f.seq, f.vtrack, f.clip_b).unwrap(),
        ops::roll_edit(p, f.seq, f.vtrack, f.clip_a, f.clip_b, Tick(200)).unwrap(),
        ops::slide_clip(p, f.seq, f.vtrack, f.clip_b, Tick(200)).unwrap(),
    ];
    for c in &cmds {
        assert_undo_roundtrip(&f.doc, c);
    }
}

#[test]
fn sequence_and_track_ops_roundtrip() {
    let f = fixture();
    let p = f.project();
    let cmds = vec![
        ops::add_sequence(Sequence::new("Seq 2", FrameRate::FPS_25, 1080, 1920)),
        ops::remove_sequence(p, f.seq).unwrap(),
        ops::set_active_format(p, f.seq, 0).unwrap(),
        ops::add_format(f.seq, SequenceFormat::new("9:16", 1080, 1920)),
        ops::add_track(p, f.seq, Track::new(TrackKind::Video, "V2"), None).unwrap(),
        ops::remove_track(p, f.seq, f.atrack).unwrap(),
        ops::set_track_prop(p, f.seq, f.vtrack, {
            let mut ts = TrackSettings::of(p.sequences[&f.seq].track(f.vtrack).unwrap());
            ts.enabled = false;
            ts
        })
        .unwrap(),
    ];
    for c in &cmds {
        assert_undo_roundtrip(&f.doc, c);
    }
}

#[test]
fn effect_and_grade_ops_roundtrip() {
    let f = fixture();
    let p = f.project();
    let cmds = vec![
        ops::add_effect(
            p,
            f.seq,
            f.vtrack,
            f.clip_a,
            ClipEffect::new(EffectKind::Blur),
            None,
        )
        .unwrap(),
        ops::set_grade(p, f.seq, f.vtrack, f.clip_a, Some(Grade::new())).unwrap(),
    ];
    for c in &cmds {
        assert_undo_roundtrip(&f.doc, c);
    }

    // Removing an effect that exists.
    let mut d = f.doc.clone();
    Command::Timeline(cmds[0].clone()).apply(&mut d);
    let rem =
        ops::remove_effect(d.timeline.as_ref().unwrap(), f.seq, f.vtrack, f.clip_a, 0).unwrap();
    assert_undo_roundtrip(&d, &rem);
}

#[test]
fn keyframe_ops_roundtrip() {
    let f = fixture();
    let p = f.project();
    let target = AnimTarget::ClipTransform { clip: f.clip_a };
    let path = PropPath::new("transform.opacity");
    let kf = Keyframe::new(Tick(100), PropValue::Float(0.5), Interp::Linear);

    let set = ops::set_keyframe(p, target.clone(), path.clone(), kf);
    assert_undo_roundtrip(&f.doc, &set);

    // After setting, removing and re-interp'ing round-trip too.
    let mut d = f.doc.clone();
    Command::Timeline(set.clone()).apply(&mut d);
    let p2 = d.timeline.as_ref().unwrap();
    let rem = ops::remove_keyframe(p2, target.clone(), path.clone(), Tick(100)).unwrap();
    assert_undo_roundtrip(&d, &rem);
    let interp = ops::set_keyframe_interp(p2, target, path, Tick(100), Interp::Hold).unwrap();
    assert_undo_roundtrip(&d, &interp);
}

#[test]
fn audio_ops_roundtrip() {
    let f = fixture();
    let p = f.project();

    let set_track = TimelineCmd::AudioEdit(commands::AudioCmd::SetTrackAudioProp {
        track: f.atrack,
        old: TrackAudioParams::default(),
        new: TrackAudioParams {
            volume_db: -6.0,
            pan: 0.3,
        },
    });
    assert_undo_roundtrip(&f.doc, &set_track);

    let duck = ops::apply_ducking_preset(p, f.atrack, f.atrack);
    assert_eq!(
        duck,
        Err(ops::EditError::SidechainCycle),
        "self-duck must be rejected"
    );

    // A second audio track to duck against.
    let mut d = f.doc.clone();
    let a2 = Track::new(TrackKind::Audio, "A2");
    let a2_id = a2.id;
    Command::Timeline(ops::add_track(d.timeline.as_ref().unwrap(), f.seq, a2, None).unwrap())
        .apply(&mut d);
    let duck = ops::apply_ducking_preset(d.timeline.as_ref().unwrap(), f.atrack, a2_id).unwrap();
    assert_undo_roundtrip(&d, &duck);
}

#[test]
fn composition_ops_roundtrip_and_reject_adjustment() {
    let f = fixture();
    let p = f.project();

    // Creating a composition on a normal clip yields [AddGraph, SetClipComposition].
    let batch = ops::create_clip_composition(p, f.seq, f.vtrack, f.clip_a).unwrap();
    assert_eq!(batch.len(), 2);
    let mut d = f.doc.clone();
    for c in &batch {
        assert_undo_roundtrip(&d, c);
        Command::Timeline(c.clone()).apply(&mut d);
    }
    assert!(d.timeline.as_ref().unwrap().graphs.len() == 1);

    // Rejected on an Adjustment-source clip (07 §6.6).
    let adj = ops::create_clip_composition(p, f.seq, f.atrack, f.aclip);
    assert_eq!(adj, Err(ops::EditError::CompositionOnAdjustment));
}

#[test]
fn graph_ops_cycle_refusal() {
    let f = fixture();
    // Build a composition graph in the arena.
    let mut d = f.doc.clone();
    for c in ops::create_clip_composition(f.project(), f.seq, f.vtrack, f.clip_a).unwrap() {
        Command::Timeline(c).apply(&mut d);
    }
    let p = d.timeline.as_ref().unwrap();
    let (gid, g) = p.graphs.iter().next().unwrap();
    // The seed is ClipIn → Output; adding Output → ClipIn would cycle.
    let clip_in = g
        .nodes
        .values()
        .find(|n| matches!(n.op, GraphOp::ClipIn))
        .unwrap()
        .id;
    let out = g.output;
    let err = graph_ops::add_edge(p, *gid, (out, OutPort::PRIMARY), (clip_in, InPort::PRIMARY));
    assert_eq!(err, Err(ops::EditError::WouldCreateCycle));
}

// ── Invariant enforcement ───────────────────────────────────────────────────

#[test]
fn insert_clip_rejects_overlap_and_zero_duration() {
    let f = fixture();
    let p = f.project();
    // Overlaps clip A (0..1000).
    let overlapping = Clip::new(ClipSource::Adjustment, Tick(500), Tick(100));
    assert_eq!(
        ops::insert_clip(p, f.seq, f.vtrack, overlapping),
        Err(ops::EditError::Overlap)
    );
    // Zero duration.
    let zero = Clip::new(ClipSource::Adjustment, Tick(3000), Tick(0));
    assert_eq!(
        ops::insert_clip(p, f.seq, f.vtrack, zero),
        Err(ops::EditError::NonPositiveDuration)
    );
    // A valid placement after B succeeds.
    let ok = Clip::new(ClipSource::Adjustment, Tick(2000), Tick(500));
    assert!(ops::insert_clip(p, f.seq, f.vtrack, ok).is_ok());
}

#[test]
fn move_into_overlap_is_rejected() {
    let f = fixture();
    let p = f.project();
    // Move B onto A.
    assert_eq!(
        ops::move_clip(p, f.seq, f.vtrack, f.clip_b, Tick(500)),
        Err(ops::EditError::Overlap)
    );
}

#[test]
fn split_outside_clip_is_rejected() {
    let f = fixture();
    let p = f.project();
    assert_eq!(
        ops::split_clip(p, f.seq, f.vtrack, f.clip_a, Tick(0)),
        Err(ops::EditError::InvalidSplit)
    );
    assert_eq!(
        ops::split_clip(p, f.seq, f.vtrack, f.clip_a, Tick(5000)),
        Err(ops::EditError::InvalidSplit)
    );
}

#[test]
fn split_then_validate_holds() {
    let f = fixture();
    let mut d = f.doc.clone();
    let cmd = ops::split_clip(f.project(), f.seq, f.vtrack, f.clip_a, Tick(400)).unwrap();
    Command::Timeline(cmd).apply(&mut d);
    let seq = &d.timeline.as_ref().unwrap().sequences[&f.seq];
    assert!(seq.validate().is_ok());
    // The split produced 3 clips (A-left, A-right, B).
    assert_eq!(seq.track(f.vtrack).unwrap().clips.len(), 3);
}

#[test]
fn nested_sequence_cycle_is_rejected() {
    let f = fixture();
    let p = f.project();
    // A clip that nests the very sequence it lives in.
    let c = Clip::new(
        ClipSource::NestedSequence { sequence: f.seq },
        Tick(2000),
        Tick(100),
    );
    assert_eq!(
        ops::insert_clip(p, f.seq, f.vtrack, c),
        Err(ops::EditError::SequenceCycle)
    );
}

// ── Follow-up gaps: markers, work range, cross-track move, ripple-trim, bins ──

#[test]
fn marker_and_work_range_ops_roundtrip() {
    let f = fixture();
    let p = f.project();

    let add = ops::add_marker(p, f.seq, Marker::new(Tick(500), "cue A")).unwrap();
    assert_undo_roundtrip(&f.doc, &add);
    let set_wr = ops::set_work_range(p, f.seq, Some((Tick(100), Tick(1500)))).unwrap();
    assert_undo_roundtrip(&f.doc, &set_wr);

    // After adding two markers, remove and edit round-trip against that state.
    let mut d = f.doc.clone();
    Command::Timeline(add.clone()).apply(&mut d);
    let m2 = Marker::new(Tick(200), "cue B");
    let m2_id = m2.id;
    Command::Timeline(ops::add_marker(d.timeline.as_ref().unwrap(), f.seq, m2).unwrap())
        .apply(&mut d);

    let rem = ops::remove_marker(d.timeline.as_ref().unwrap(), f.seq, m2_id).unwrap();
    assert_undo_roundtrip(&d, &rem);

    let mut edited = Marker::new(Tick(900), "renamed");
    edited.id = m2_id; // edit the existing marker (id preserved)
    edited.note = "note".into();
    let set = ops::set_marker(d.timeline.as_ref().unwrap(), f.seq, edited).unwrap();
    assert_undo_roundtrip(&d, &set);

    // Clearing the work range (None) also round-trips.
    let mut d2 = f.doc.clone();
    Command::Timeline(set_wr).apply(&mut d2);
    let clear = ops::set_work_range(d2.timeline.as_ref().unwrap(), f.seq, None).unwrap();
    assert_undo_roundtrip(&d2, &clear);
}

// ── K-A2: marker categories, clip markers, ranged markers ───────────────────

/// Every marker-category CRUD op is one exactly-invertible undo unit.
#[test]
fn marker_category_crud_roundtrips() {
    let f = fixture();

    // Seeding is one batch of adds; each is individually invertible, and the
    // whole set restores an empty registry.
    let seeds = ops::seed_marker_categories(f.project());
    assert_eq!(
        seeds.len(),
        MarkerCategory::default_seed().len(),
        "seed emits one command per default category"
    );
    let mut d = f.doc.clone();
    for cmd in &seeds {
        assert_undo_roundtrip(&d, cmd);
        Command::Timeline(cmd.clone()).apply(&mut d);
    }
    let names: Vec<&str> = d
        .timeline
        .as_ref()
        .unwrap()
        .marker_categories
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(
        names,
        ["Marker", "Cut", "Note", "Todo", "Chapter", "Bookmarks"]
    );

    // Seeding twice is a no-op, not a duplicate set.
    assert!(
        ops::seed_marker_categories(d.timeline.as_ref().unwrap()).is_empty(),
        "seeding an already-seeded project must be idempotent"
    );

    // Rename + recolour + re-glyph, in place, id preserved.
    let cut = d.timeline.as_ref().unwrap().marker_categories[1].clone();
    let mut renamed = cut.clone();
    renamed.name = "Hard cut".into();
    renamed.color = photonic_core::Color::rgb(0.1, 0.2, 0.3);
    renamed.glyph = MarkerGlyph::Square;
    let set = ops::set_marker_category(d.timeline.as_ref().unwrap(), renamed).unwrap();
    assert_undo_roundtrip(&d, &set);

    // A plain add appends.
    let fresh = MarkerCategory::new("VFX", photonic_core::Color::rgb(0.5, 0.0, 0.5));
    let add = ops::add_marker_category(d.timeline.as_ref().unwrap(), fresh.clone()).unwrap();
    assert_undo_roundtrip(&d, &add);
    let mut d2 = d.clone();
    Command::Timeline(add).apply(&mut d2);
    assert_eq!(
        d2.timeline
            .as_ref()
            .unwrap()
            .marker_categories
            .last()
            .unwrap()
            .id,
        fresh.id
    );
    // A duplicate id is refused rather than making `marker_category` ambiguous.
    assert!(ops::add_marker_category(d2.timeline.as_ref().unwrap(), fresh.clone()).is_err());

    // Editing an unknown category is an error, not a silent no-op.
    assert!(ops::set_marker_category(
        f.project(),
        MarkerCategory::new("ghost", photonic_core::Color::rgb(0.0, 0.0, 0.0)),
    )
    .unwrap_err()
    .to_string()
    .contains("NoMarkerCategory"));
}

/// Deleting a category retargets every referencing marker, in BOTH scopes, as
/// part of the same undo unit — and undo puts each one back on the deleted
/// category (35 §1.3 "never silently remapped").
#[test]
fn removing_a_category_reassigns_markers_in_both_scopes() {
    let f = fixture();
    let mut d = f.doc.clone();
    for cmd in ops::seed_marker_categories(f.project()) {
        Command::Timeline(cmd).apply(&mut d);
    }
    let cats = d.timeline.as_ref().unwrap().marker_categories.clone();
    let (cut, note) = (cats[1].id, cats[2].id);

    // One sequence marker and one CLIP marker, both on "Cut".
    let mut seq_marker = Marker::new(Tick(500), "seq");
    seq_marker.category = Some(cut);
    let seq_marker_id = seq_marker.id;
    Command::Timeline(ops::add_marker(d.timeline.as_ref().unwrap(), f.seq, seq_marker).unwrap())
        .apply(&mut d);
    let mut clip_marker = Marker::clip_scoped(Tick(50), "clip");
    clip_marker.category = Some(cut);
    let clip_marker_id = clip_marker.id;
    Command::Timeline(
        ops::add_clip_marker(d.timeline.as_ref().unwrap(), f.clip_a, clip_marker).unwrap(),
    )
    .apply(&mut d);

    // The fixture is non-vacuous: both markers really do reference "Cut"
    // before the delete, so the assertions below can fail.
    let found = d.timeline.as_ref().unwrap().markers_in_category(cut);
    assert_eq!(
        found.len(),
        2,
        "expected both scopes to be found: {found:?}"
    );
    assert!(found.contains(&MarkerRef::Sequence {
        seq: f.seq,
        marker: seq_marker_id
    }));
    assert!(found.contains(&MarkerRef::Clip {
        clip: f.clip_a,
        marker: clip_marker_id
    }));

    // Reassign-on-delete.
    let rm = ops::remove_marker_category(d.timeline.as_ref().unwrap(), cut, Some(note)).unwrap();
    assert_undo_roundtrip(&d, &rm);
    let mut after = d.clone();
    Command::Timeline(rm).apply(&mut after);
    let p = after.timeline.as_ref().unwrap();
    assert!(p.marker_category(cut).is_none(), "category removed");
    assert_eq!(
        p.sequences[&f.seq]
            .markers
            .iter()
            .find(|m| m.id == seq_marker_id)
            .unwrap()
            .category,
        Some(note),
        "sequence marker reassigned"
    );
    assert_eq!(
        p.sequences[&f.seq].track(f.vtrack).unwrap().clips[0]
            .markers
            .iter()
            .find(|m| m.id == clip_marker_id)
            .unwrap()
            .category,
        Some(note),
        "clip marker reassigned"
    );

    // Clearing instead of reassigning is the other honest choice.
    let clear = ops::remove_marker_category(d.timeline.as_ref().unwrap(), cut, None).unwrap();
    assert_undo_roundtrip(&d, &clear);
    let mut cleared = d.clone();
    Command::Timeline(clear).apply(&mut cleared);
    assert_eq!(
        cleared.timeline.as_ref().unwrap().sequences[&f.seq]
            .markers
            .iter()
            .find(|m| m.id == seq_marker_id)
            .unwrap()
            .category,
        None
    );

    // Reassigning to the category being deleted is refused — it would leave
    // every marker pointing at a missing id, the outcome the rule forbids.
    assert!(ops::remove_marker_category(d.timeline.as_ref().unwrap(), cut, Some(cut)).is_err());
    // ...as is reassigning to a category that does not exist.
    assert!(ops::remove_marker_category(
        d.timeline.as_ref().unwrap(),
        cut,
        Some(MarkerCategoryId::new()),
    )
    .is_err());
    // Deleting a category that is not there at all is an error, not a no-op.
    assert!(
        ops::remove_marker_category(f.project(), MarkerCategoryId::new(), None).is_err(),
        "unknown category delete must be rejected"
    );
}

/// Clip markers are a real, undoable, validated write surface.
#[test]
fn clip_marker_ops_roundtrip() {
    let f = fixture();
    let mut d = f.doc.clone();

    let m = Marker::clip_scoped(Tick(120), "beat");
    let m_id = m.id;
    assert_eq!(
        m.anchor,
        MarkerAnchor::Content,
        "clip markers are Content-anchored (35 §1.5)"
    );
    let add = ops::add_clip_marker(f.project(), f.clip_a, m).unwrap();
    assert_undo_roundtrip(&d, &add);
    Command::Timeline(add).apply(&mut d);

    // Clip-relative `at` maps onto the timeline through the clip's own helper.
    let clip = &d.timeline.as_ref().unwrap().sequences[&f.seq]
        .track(f.vtrack)
        .unwrap()
        .clips[0];
    assert_eq!(
        clip.marker_sequence_tick(&clip.markers[0]),
        clip.start + Tick(120)
    );

    // A ranged clip marker (duration > 0) round-trips.
    let mut ranged = clip.markers[0].clone();
    ranged.duration = Tick(300);
    let set = ops::set_clip_marker(d.timeline.as_ref().unwrap(), f.clip_a, ranged).unwrap();
    assert_undo_roundtrip(&d, &set);

    let rem = ops::remove_clip_marker(d.timeline.as_ref().unwrap(), f.clip_a, m_id).unwrap();
    assert_undo_roundtrip(&d, &rem);

    // Out-of-clip positions are refused: clip A is 1000 ticks long, so a
    // marker at 1001 would map outside the clip and never be drawn.
    assert!(
        ops::add_clip_marker(f.project(), f.clip_a, Marker::clip_scoped(Tick(1001), "x")).is_err()
    );
    assert!(
        ops::add_clip_marker(
            f.project(),
            f.clip_a,
            Marker::clip_scoped(Tick(1000), "edge")
        )
        .is_ok(),
        "the clip's exclusive end is still an addressable marker position"
    );
    // An unknown marker id is an error, not a silent no-op.
    assert!(ops::remove_clip_marker(f.project(), f.clip_a, MarkerId::new()).is_err());
    assert!(ops::add_clip_marker(
        f.project(),
        ClipId::new(),
        Marker::clip_scoped(Tick(0), "y")
    )
    .is_err());
}

/// Splitting a clip PARTITIONS its clip markers instead of duplicating them,
/// and merging folds them back — the pair round-trips losslessly.
#[test]
fn split_partitions_clip_markers_and_merge_folds_them_back() {
    let f = fixture();
    let mut d = f.doc.clone();
    // Clip A is [0, 1000). Three markers: before, at, and after the cut at 400.
    for (at, name) in [
        (Tick(100), "early"),
        (Tick(400), "on-cut"),
        (Tick(700), "late"),
    ] {
        let cmd = ops::add_clip_marker(
            d.timeline.as_ref().unwrap(),
            f.clip_a,
            Marker::clip_scoped(at, name),
        )
        .unwrap();
        Command::Timeline(cmd).apply(&mut d);
    }
    let before_ids: Vec<MarkerId> = d.timeline.as_ref().unwrap().sequences[&f.seq]
        .track(f.vtrack)
        .unwrap()
        .clips[0]
        .markers
        .iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(before_ids.len(), 3, "fixture is non-vacuous");

    let split = ops::split_clip(
        d.timeline.as_ref().unwrap(),
        f.seq,
        f.vtrack,
        f.clip_a,
        Tick(400),
    )
    .unwrap();
    assert_undo_roundtrip(&d, &split);

    let mut after = d.clone();
    Command::Timeline(split).apply(&mut after);
    let t = after.timeline.as_ref().unwrap().sequences[&f.seq]
        .track(f.vtrack)
        .unwrap();
    let (left, right) = (&t.clips[0], &t.clips[1]);
    // "early" stays left; "on-cut" and "late" move right and REBASE.
    assert_eq!(
        left.markers
            .iter()
            .map(|m| (m.at, m.name.as_str()))
            .collect::<Vec<_>>(),
        vec![(Tick(100), "early")]
    );
    assert_eq!(
        right
            .markers
            .iter()
            .map(|m| (m.at, m.name.as_str()))
            .collect::<Vec<_>>(),
        vec![(Tick(0), "on-cut"), (Tick(300), "late")]
    );
    // Every id survives exactly once across the two halves — no duplicates.
    let mut after_ids: Vec<MarkerId> = left
        .markers
        .iter()
        .chain(right.markers.iter())
        .map(|m| m.id)
        .collect();
    after_ids.sort();
    let mut expect = before_ids.clone();
    expect.sort();
    assert_eq!(
        after_ids, expect,
        "split must partition ids, not clone them"
    );
}

#[test]
fn media_bin_ops_roundtrip() {
    let f = fixture();
    // Put an asset in the pool first.
    let mut d = f.doc.clone();
    let asset = MediaAsset::from_file(AssetKind::Video, "/clips/a.mp4");
    let asset_id = asset.id;
    Command::Timeline(ops::add_asset(asset)).apply(&mut d);

    let create = ops::create_bin("Footage", None);
    assert_undo_roundtrip(&d, &create);
    Command::Timeline(create.clone()).apply(&mut d);
    let bin_id = match &create {
        TimelineCmd::AddBin { bin } => bin.id,
        _ => unreachable!(),
    };

    let assign =
        ops::assign_asset_bin(d.timeline.as_ref().unwrap(), asset_id, Some(bin_id)).unwrap();
    assert_undo_roundtrip(&d, &assign);
    Command::Timeline(assign).apply(&mut d);
    assert_eq!(
        d.timeline.as_ref().unwrap().media.assets[&asset_id].bin,
        Some(bin_id)
    );

    let remove = ops::remove_bin(d.timeline.as_ref().unwrap(), bin_id).unwrap();
    assert_undo_roundtrip(&d, &remove);

    // Assigning to a nonexistent bin is rejected.
    assert_eq!(
        ops::assign_asset_bin(d.timeline.as_ref().unwrap(), asset_id, Some(BinId::new())),
        Err(ops::EditError::IndexOutOfRange)
    );
}

#[test]
fn cross_track_move_roundtrip_and_invariants() {
    let f = fixture();
    // Add a second video track to move onto.
    let mut d = f.doc.clone();
    let v2 = Track::new(TrackKind::Video, "V2");
    let v2_id = v2.id;
    Command::Timeline(ops::add_track(d.timeline.as_ref().unwrap(), f.seq, v2, None).unwrap())
        .apply(&mut d);

    // Move clip B from V1 to V2 (lossless inverse returns it).
    let mv = ops::move_clip_to_track(
        d.timeline.as_ref().unwrap(),
        f.seq,
        f.vtrack,
        f.clip_b,
        Tick(0),
        Some(v2_id),
    )
    .unwrap();
    assert_undo_roundtrip(&d, &mv);

    // Apply it: B leaves V1, lands on V2.
    let mut d2 = d.clone();
    Command::Timeline(mv).apply(&mut d2);
    let seq = &d2.timeline.as_ref().unwrap().sequences[&f.seq];
    assert!(seq
        .track(f.vtrack)
        .unwrap()
        .clips
        .iter()
        .all(|c| c.id != f.clip_b));
    assert!(seq
        .track(v2_id)
        .unwrap()
        .clips
        .iter()
        .any(|c| c.id == f.clip_b));
    assert!(seq.validate().is_ok());

    // Moving onto an occupied region of the destination is rejected.
    let occupied = ops::move_clip_to_track(
        d.timeline.as_ref().unwrap(),
        f.seq,
        f.vtrack,
        f.clip_b,
        Tick(0), // clip A occupies 0..1000 on V1; move onto V1 start collides via A
        None,
    );
    assert_eq!(occupied, Err(ops::EditError::Overlap));

    // Cross-kind moves (video → audio) are rejected.
    let wrong_kind = ops::move_clip_to_track(
        d.timeline.as_ref().unwrap(),
        f.seq,
        f.vtrack,
        f.clip_a,
        Tick(3000),
        Some(f.atrack),
    );
    assert_eq!(wrong_kind, Err(ops::EditError::NoTrack(f.atrack)));

    // The plain 5-arg move_clip still works (same-track) and round-trips.
    let same = ops::move_clip(
        d.timeline.as_ref().unwrap(),
        f.seq,
        f.vtrack,
        f.clip_b,
        Tick(2200),
    )
    .unwrap();
    assert_undo_roundtrip(&d, &same);
}

#[test]
fn ripple_trim_end_and_start() {
    let f = fixture();
    let p = f.project();
    // V1: A[0..1000], B[1000..2000]. Ripple-trim A's END to 600 → A[0..600],
    // and B shifts left by -400 to [600..1600].
    let end = ops::ripple_trim(p, f.seq, f.vtrack, f.clip_a, ClipEdge::End, Tick(600)).unwrap();
    assert_batch_undo_roundtrip(&f.doc, &end);
    let mut d = f.doc.clone();
    for cmd in &end {
        Command::Timeline(cmd.clone()).apply(&mut d);
    }
    let seq = &d.timeline.as_ref().unwrap().sequences[&f.seq];
    let clips = &seq.track(f.vtrack).unwrap().clips;
    assert_eq!(clips[0].duration, Tick(600));
    assert_eq!(clips[1].start, Tick(600));
    assert_eq!(clips[1].end(), Tick(1600));
    assert!(seq.validate().is_ok());

    // Ripple-trim B's START later to 1200 → B head trimmed by 200, B keeps its
    // timeline start, later clips (none) unaffected; invariant holds.
    let start = ops::ripple_trim(
        f.project(),
        f.seq,
        f.vtrack,
        f.clip_b,
        ClipEdge::Start,
        Tick(1200),
    )
    .unwrap();
    assert_batch_undo_roundtrip(&f.doc, &start);
    let mut d2 = f.doc.clone();
    for cmd in &start {
        Command::Timeline(cmd.clone()).apply(&mut d2);
    }
    let seq2 = &d2.timeline.as_ref().unwrap().sequences[&f.seq];
    assert!(seq2.validate().is_ok());

    // Trimming the end before the start is rejected.
    assert_eq!(
        ops::ripple_trim(
            f.project(),
            f.seq,
            f.vtrack,
            f.clip_a,
            ClipEdge::End,
            Tick(0)
        ),
        Err(ops::EditError::NonPositiveDuration)
    );
}

// ── Exhaustive per-variant undo coverage (P2 gate: every TimelineCmd leaf) ───
//
// `assert_undo_roundtrip` proves `apply → inverse → apply == apply` for each
// constructible leaf. The internal inverse-only partners `MergeSplit`,
// `RemoveProject`, and `CaptionCmd::UndoBulkInsert` are NOT self-invertible
// (they exist solely as the inverse of `SplitClip` / `CreateProject` /
// `BulkInsertCues`) and are exercised as the *applied inverse* inside those
// forward ops' round-trips; the `variant_exhaustiveness_guard` below still names
// them so a newly added variant fails to compile until it is wired in here.

/// Project + media leaves: `AddAsset` (and its `RemoveAsset` inverse),
/// `RemoveAsset`, `RelinkAsset`, and the `RemoveProject` inverse partner.
#[test]
fn undo_roundtrip_project_and_media() {
    let f = fixture();

    // AddAsset on a project that lacks the asset (inverse is RemoveAsset).
    let asset = MediaAsset::from_file(AssetKind::Video, "/clips/a.mp4");
    let asset_id = asset.id;
    let add = ops::add_asset(asset);
    assert_undo_roundtrip(&f.doc, &add);

    // With the asset present: RemoveAsset and RelinkAsset.
    let mut d = f.doc.clone();
    Command::Timeline(add).apply(&mut d);
    let rem = ops::remove_asset(d.timeline.as_ref().unwrap(), asset_id).unwrap();
    assert_undo_roundtrip(&d, &rem);
    let relink = ops::relink_asset(
        d.timeline.as_ref().unwrap(),
        asset_id,
        "/clips/b.mp4".into(),
    )
    .unwrap();
    assert_undo_roundtrip(&d, &relink);

    // RemoveProject is a true self-contained round-trip on a doc with a timeline.
    let remove_project = TimelineCmd::RemoveProject {
        project: Box::new(f.project().clone()),
    };
    assert_undo_roundtrip(&f.doc, &remove_project);
}

/// `SetActiveSequence` and the `SetSequenceFormat` Update/Remove ops (the Remove
/// exercises the position-preserving `Insert` inverse for a non-last index).
#[test]
fn undo_roundtrip_active_sequence_and_formats() {
    let f = fixture();

    // Switch the active sequence to None and back.
    let deselect = ops::set_active_sequence(f.project(), None);
    assert_undo_roundtrip(&f.doc, &deselect);

    // Give the sequence a second format so a middle removal is meaningful.
    let mut d = f.doc.clone();
    Command::Timeline(ops::add_format(
        f.seq,
        SequenceFormat::new("9:16", 1080, 1920),
    ))
    .apply(&mut d);
    let fmt0 = d.timeline.as_ref().unwrap().sequences[&f.seq].formats[0].clone();

    // Update index 0 (old must match the live format for a lossless inverse).
    let update = TimelineCmd::SetSequenceFormat {
        seq: f.seq,
        op: FormatOp::Update {
            index: 0,
            old: fmt0.clone(),
            new: SequenceFormat::new("wide", 1280, 720),
        },
    };
    assert_undo_roundtrip(&d, &update);

    // Remove the *first* format: a plain append inverse would reorder the list,
    // so the inverse must re-insert at index 0 (regression guard for the fix).
    let remove = TimelineCmd::SetSequenceFormat {
        seq: f.seq,
        op: FormatOp::Remove {
            index: 0,
            format: fmt0,
        },
    };
    assert_undo_roundtrip(&d, &remove);
}

/// `SetClipProp` and `ReorderEffects`.
#[test]
fn undo_roundtrip_clip_prop_and_reorder_effects() {
    let f = fixture();

    // Rename clip A (same timing → no overlap, no re-sort surprises).
    let mut renamed = {
        let p = f.project();
        let t = p.sequences[&f.seq].track(f.vtrack).unwrap();
        t.clips.iter().find(|c| c.id == f.clip_a).unwrap().clone()
    };
    renamed.name = "renamed".into();
    let set_prop = ops::set_clip_prop(f.project(), f.seq, f.vtrack, renamed).unwrap();
    assert_undo_roundtrip(&f.doc, &set_prop);

    // Give clip A two effects, then reorder them.
    let mut d = f.doc.clone();
    for kind in [EffectKind::Blur, EffectKind::Glow] {
        let add = ops::add_effect(
            d.timeline.as_ref().unwrap(),
            f.seq,
            f.vtrack,
            f.clip_a,
            ClipEffect::new(kind),
            None,
        )
        .unwrap();
        Command::Timeline(add).apply(&mut d);
    }
    let reorder = ops::reorder_effects(
        d.timeline.as_ref().unwrap(),
        f.seq,
        f.vtrack,
        f.clip_a,
        vec![1, 0],
    )
    .unwrap();
    assert_undo_roundtrip(&d, &reorder);
}

/// `SetProjectGraph` (built as a literal; the arena reference need not resolve
/// for the pure Option swap).
#[test]
fn undo_roundtrip_set_project_graph() {
    let f = fixture();
    let set = TimelineCmd::SetProjectGraph {
        old: None,
        new: Some(GraphId::new()),
    };
    assert_undo_roundtrip(&f.doc, &set);
}

/// All 8 `GraphCmd` leaves against a real per-clip composition graph.
#[test]
fn undo_roundtrip_all_graph_cmds() {
    let f = fixture();
    let mut d = f.doc.clone();
    for c in ops::create_clip_composition(f.project(), f.seq, f.vtrack, f.clip_a).unwrap() {
        Command::Timeline(c).apply(&mut d);
    }
    let (gid, clip_in, seed_edge) = {
        let p = d.timeline.as_ref().unwrap();
        let (gid, g) = p.graphs.iter().next().unwrap();
        let clip_in = g
            .nodes
            .values()
            .find(|n| matches!(n.op, GraphOp::ClipIn))
            .unwrap()
            .id;
        (*gid, clip_in, g.edges[0])
    };

    // AddNode / MoveNode / SetNodeParam / RemoveEdge against the seeded graph.
    let add_node = graph_ops::add_node(
        gid,
        GraphNode::new(GraphOp::Blur),
        NodePos { x: 10.0, y: 10.0 },
    );
    assert_undo_roundtrip(&d, &add_node);

    let move_node = graph_ops::move_node(
        d.timeline.as_ref().unwrap(),
        gid,
        clip_in,
        NodePos { x: 50.0, y: 60.0 },
    )
    .unwrap();
    assert_undo_roundtrip(&d, &move_node);

    let mut ep = EffectParams::new();
    ep.set("params.radius", PropValue::Float(7.0));
    let set_param = graph_ops::set_node_param(
        d.timeline.as_ref().unwrap(),
        gid,
        clip_in,
        GraphNodeParams(ep),
    )
    .unwrap();
    assert_undo_roundtrip(&d, &set_param);

    let remove_edge = graph_ops::remove_edge(gid, seed_edge);
    assert_undo_roundtrip(&d, &remove_edge);

    // AddEdge / RemoveNode / (Set|Remove)Keyframe need a second node present.
    let mut d2 = d.clone();
    let blur = GraphNode::new(GraphOp::Blur);
    let blur_id = blur.id;
    Command::Timeline(graph_ops::add_node(gid, blur, NodePos { x: 10.0, y: 10.0 })).apply(&mut d2);

    let add_edge = graph_ops::add_edge(
        d2.timeline.as_ref().unwrap(),
        gid,
        (clip_in, OutPort::PRIMARY),
        (blur_id, InPort::PRIMARY),
    )
    .unwrap();
    assert_undo_roundtrip(&d2, &add_edge);

    let remove_node = graph_ops::remove_node(d2.timeline.as_ref().unwrap(), gid, blur_id).unwrap();
    assert_undo_roundtrip(&d2, &remove_node);

    let kf = Keyframe::new(Tick(0), PropValue::Float(3.0), Interp::Linear);
    let set_kf = TimelineCmd::GraphEdit(commands::GraphCmd::SetKeyframe {
        graph: gid,
        node: blur_id,
        path: PropPath::new("params.radius"),
        old: None,
        new: kf,
    });
    assert_undo_roundtrip(&d2, &set_kf);

    // RemoveKeyframe against a graph that already has the keyframe.
    let mut d3 = d2.clone();
    Command::Timeline(set_kf).apply(&mut d3);
    let remove_kf = TimelineCmd::GraphEdit(commands::GraphCmd::RemoveKeyframe {
        graph: gid,
        node: blur_id,
        path: PropPath::new("params.radius"),
        keyframe: kf,
    });
    assert_undo_roundtrip(&d3, &remove_kf);
}

/// All 8 `CaptionCmd` leaves against a sequence with a caption track.
/// `UndoBulkInsert` is inverse-only (covered as `BulkInsertCues`' inverse).
#[test]
fn undo_roundtrip_all_caption_cmds() {
    let f = fixture();
    let mut d = f.doc.clone();

    // Build a caption track with two cues; cue1 has two words.
    let (ctid, cue1_id, cue2_id, cue1, cue2) = {
        let mut ct = CaptionTrack::new("Captions");
        let cue1 = CaptionCue::new(
            Tick(0),
            Tick(500),
            vec![
                CaptionWord::new("hello", Tick(0), Tick(250)),
                CaptionWord::new("world", Tick(250), Tick(500)),
            ],
        );
        let cue2 = CaptionCue::new(
            Tick(600),
            Tick(1000),
            vec![CaptionWord::new("again", Tick(600), Tick(1000))],
        );
        let (ctid, c1, c2) = (ct.id, cue1.id, cue2.id);
        ct.cues.push(cue1.clone());
        ct.cues.push(cue2.clone());
        let p = d.timeline.as_mut().unwrap();
        p.sequences.get_mut(&f.seq).unwrap().caption_tracks.push(ct);
        (ctid, c1, c2, cue1, cue2)
    };
    let base_style = CaptionStyle::default();

    // BulkInsertCues on the existing track (no created_track, nothing replaced).
    let new_cue = CaptionCue::new(
        Tick(1200),
        Tick(1500),
        vec![CaptionWord::new("more", Tick(1200), Tick(1500))],
    );
    let bulk = TimelineCmd::CaptionEdit(commands::CaptionCmd::BulkInsertCues {
        track: ctid,
        cues: vec![new_cue],
        replace_range: None,
        replaced: vec![],
        created_track: None,
    });
    assert_undo_roundtrip(&d, &bulk);

    // SetCueText (old_words must match the live cue).
    let set_text = TimelineCmd::CaptionEdit(commands::CaptionCmd::SetCueText {
        track: ctid,
        cue: cue1_id,
        old_words: cue1.words.clone(),
        new_words: vec![CaptionWord::new("hi", Tick(0), Tick(500))],
    });
    assert_undo_roundtrip(&d, &set_text);

    // SplitCue at word boundary 1 (inverse merges the halves back).
    let split = TimelineCmd::CaptionEdit(commands::CaptionCmd::SplitCue {
        track: ctid,
        cue: cue1_id,
        at_word_index: 1,
        new_cue_id: CueId::new(),
    });
    assert_undo_roundtrip(&d, &split);

    // MergeCues (carries both originals so the inverse restores them verbatim).
    let merge = TimelineCmd::CaptionEdit(commands::CaptionCmd::MergeCues {
        track: ctid,
        a: cue1_id,
        b: cue2_id,
        old_a: Box::new(cue1.clone()),
        old_b: Box::new(cue2.clone()),
    });
    assert_undo_roundtrip(&d, &merge);

    // RetimeCue / RetimeWord (old must match live state).
    let retime_cue = TimelineCmd::CaptionEdit(commands::CaptionCmd::RetimeCue {
        track: ctid,
        cue: cue1_id,
        old: (Tick(0), Tick(500)),
        new: (Tick(100), Tick(550)),
    });
    assert_undo_roundtrip(&d, &retime_cue);

    let retime_word = TimelineCmd::CaptionEdit(commands::CaptionCmd::RetimeWord {
        track: ctid,
        cue: cue1_id,
        word: 0,
        old: (Tick(0), Tick(250)),
        new: (Tick(10), Tick(260)),
    });
    assert_undo_roundtrip(&d, &retime_word);

    // SetStyle at Track scope (both sides Some — the Track arm only sets on Some).
    let mut new_style = base_style.clone();
    new_style.font_size = 60.0;
    let set_style = TimelineCmd::CaptionEdit(commands::CaptionCmd::SetStyle {
        track: ctid,
        target: commands::StyleTarget::Track,
        old: Some(Box::new(base_style)),
        new: Some(Box::new(new_style)),
    });
    assert_undo_roundtrip(&d, &set_style);
}

/// Both `TtsCmd` leaves: `GenerateAndPlace` (structural no-op at this phase) and
/// `Regenerate` (asset swap on an `Asset`-source clip).
#[test]
fn undo_roundtrip_tts_cmds() {
    let f = fixture();

    let generate = TimelineCmd::TtsEdit(commands::TtsCmd::GenerateAndPlace {
        asset: AssetId::new(),
        clip: ClipId::new(),
        track: f.atrack,
        caption: None,
    });
    assert_undo_roundtrip(&f.doc, &generate);

    // Add an Asset-source clip after B, then regenerate its asset.
    let old_asset = AssetId::new();
    let clip = {
        let mut c = Clip::new(
            ClipSource::Asset { asset: old_asset },
            Tick(2000),
            Tick(500),
        );
        c.name = "vo".into();
        c
    };
    let clip_id = clip.id;
    let mut d = f.doc.clone();
    Command::Timeline(
        ops::insert_clip(d.timeline.as_ref().unwrap(), f.seq, f.vtrack, clip).unwrap(),
    )
    .apply(&mut d);
    let regen = TimelineCmd::TtsEdit(commands::TtsCmd::Regenerate {
        clip: clip_id,
        old_asset,
        new_asset: AssetId::new(),
    });
    assert_undo_roundtrip(&d, &regen);
}

/// The remaining 9 `AudioCmd` leaves (`SetTrackAudioProp` + `ApplyDuckingPreset`
/// are covered in `audio_ops_roundtrip`).
#[test]
fn undo_roundtrip_remaining_audio_cmds() {
    let f = fixture();

    let cmds = vec![
        TimelineCmd::AudioEdit(AudioCmd::SetTrackMuteSolo {
            track: f.atrack,
            old: (false, false),
            new: (true, false),
        }),
        TimelineCmd::AudioEdit(AudioCmd::SetClipAudioProp {
            clip: f.aclip,
            old: ClipAudioParams { gain_db: 0.0 },
            new: ClipAudioParams { gain_db: -3.0 },
        }),
        TimelineCmd::AudioEdit(AudioCmd::SetClipFade {
            clip: f.aclip,
            edge: commands::FadeEdge::In,
            old: None,
            new: Some(AudioFade {
                duration: Tick(100),
                shape: FadeShape::Linear,
            }),
        }),
        TimelineCmd::AudioEdit(AudioCmd::SetChannelMap {
            clip: f.aclip,
            old: ChannelMap::AsSource,
            new: ChannelMap::MonoDownmix,
        }),
        TimelineCmd::AudioEdit(AudioCmd::AddAudioFx {
            owner: FxOwner::Track(f.atrack),
            index: 0,
            unit: AudioFxUnit::new(AudioFxKind::Eq),
        }),
        TimelineCmd::AudioEdit(AudioCmd::SetMasterBusProp {
            old: MasterBusParams { volume_db: 0.0 },
            new: MasterBusParams { volume_db: -2.0 },
        }),
        TimelineCmd::AudioEdit(AudioCmd::SetLoudnessTarget {
            old: None,
            new: Some(LoudnessTarget::streaming()),
        }),
    ];
    for c in &cmds {
        assert_undo_roundtrip(&f.doc, c);
    }

    // RemoveAudioFx / ReorderAudioFx need existing units on the track.
    let mut d = f.doc.clone();
    let u1 = AudioFxUnit::new(AudioFxKind::Eq);
    let u2 = AudioFxUnit::new(AudioFxKind::Compressor);
    Command::Timeline(TimelineCmd::AudioEdit(AudioCmd::AddAudioFx {
        owner: FxOwner::Track(f.atrack),
        index: 0,
        unit: u1.clone(),
    }))
    .apply(&mut d);
    Command::Timeline(TimelineCmd::AudioEdit(AudioCmd::AddAudioFx {
        owner: FxOwner::Track(f.atrack),
        index: 1,
        unit: u2.clone(),
    }))
    .apply(&mut d);

    let remove_fx = TimelineCmd::AudioEdit(AudioCmd::RemoveAudioFx {
        owner: FxOwner::Track(f.atrack),
        index: 0,
        unit: u1,
    });
    assert_undo_roundtrip(&d, &remove_fx);

    let reorder_fx = TimelineCmd::AudioEdit(AudioCmd::ReorderAudioFx {
        owner: FxOwner::Track(f.atrack),
        old_order: vec![0, 1],
        new_order: vec![1, 0],
    });
    assert_undo_roundtrip(&d, &reorder_fx);
}

// ── Slide / roll post-state (not just round-trip) ────────────────────────────

/// A three-clip track: slide the MIDDLE clip by a nonzero delta and assert all
/// three resulting `ClipTiming`s (prev grows, clip shifts, next shrinks+shifts),
/// with total span preserved (04 §2.4).
#[test]
fn slide_middle_clip_nonzero_delta_post_state() {
    let f = fixture();
    // fixture V1: A[0..1000], B[1000..2000]. Add C[2000..3000] to get three.
    let mut d = f.doc.clone();
    let c = Clip::new(ClipSource::Adjustment, Tick(2000), Tick(1000));
    let c_id = c.id;
    Command::Timeline(ops::insert_clip(d.timeline.as_ref().unwrap(), f.seq, f.vtrack, c).unwrap())
        .apply(&mut d);

    let before_span = {
        let clips = &d.timeline.as_ref().unwrap().sequences[&f.seq]
            .track(f.vtrack)
            .unwrap()
            .clips;
        clips.last().unwrap().end() - clips[0].start
    };

    let delta = Tick(200);
    let slide = ops::slide_clip(
        d.timeline.as_ref().unwrap(),
        f.seq,
        f.vtrack,
        f.clip_b,
        delta,
    )
    .unwrap();
    Command::Timeline(slide).apply(&mut d);

    let seq = &d.timeline.as_ref().unwrap().sequences[&f.seq];
    let clips = &seq.track(f.vtrack).unwrap().clips;
    let by_id = |id: ClipId| clips.iter().find(|c| c.id == id).unwrap();

    // prev A grows by delta.
    let a = by_id(f.clip_a);
    assert_eq!((a.start, a.duration), (Tick(0), Tick(1200)));
    // middle B shifts by delta, duration unchanged.
    let b = by_id(f.clip_b);
    assert_eq!((b.start, b.duration), (Tick(1200), Tick(1000)));
    // next C shifts start by delta and shrinks by delta; source_in advances.
    let cc = by_id(c_id);
    assert_eq!(
        (cc.start, cc.duration, cc.source_in),
        (Tick(2200), Tick(800), Tick(200))
    );

    // Total span unchanged and invariant intact.
    let after_span = clips.last().unwrap().end() - clips[0].start;
    assert_eq!(after_span, before_span);
    assert!(seq.validate().is_ok());
}

/// Roll the shared edit point between A and B: the edit point moves, but the two
/// clips' combined net duration (their outer span) is unchanged (04 §2.4).
#[test]
fn roll_edit_preserves_net_duration_post_state() {
    let f = fixture();
    let mut d = f.doc.clone();
    let delta = Tick(200);
    let roll = ops::roll_edit(f.project(), f.seq, f.vtrack, f.clip_a, f.clip_b, delta).unwrap();
    Command::Timeline(roll).apply(&mut d);

    let seq = &d.timeline.as_ref().unwrap().sequences[&f.seq];
    let clips = &seq.track(f.vtrack).unwrap().clips;
    let a = clips.iter().find(|c| c.id == f.clip_a).unwrap();
    let b = clips.iter().find(|c| c.id == f.clip_b).unwrap();

    // A gained delta, B lost delta and shifted; the shared edit point is A.end.
    assert_eq!(a.duration, Tick(1200));
    assert_eq!(b.start, Tick(1200));
    assert_eq!(b.duration, Tick(800));
    assert_eq!(a.end(), b.start, "roll keeps the two clips butt-joined");
    // Outer span A.start..B.end unchanged (net duration preserved).
    assert_eq!(b.end() - a.start, Tick(2000));
    assert!(seq.validate().is_ok());
}

// ── Serde compatibility & load-time passes ──────────────────────────────────

/// A `MoveClip` serialized before the additive `new_track` field existed omits
/// that key entirely (it is `skip_serializing_if = "Option::is_none"`), so such
/// JSON must load with `new_track == None` (01 §10 additive-field rule).
#[test]
fn move_clip_serde_compat_without_new_track() {
    let cmd = TimelineCmd::MoveClip {
        seq: SequenceId::new(),
        track: TrackId::new(),
        clip: ClipId::new(),
        old_start: Tick(0),
        new_start: Tick(500),
        new_track: None,
    };
    let json = serde_json::to_string(&cmd).unwrap();
    // Same on-disk shape a pre-`new_track` build produced: no such key.
    assert!(
        !json.contains("new_track"),
        "a same-track move must not serialize new_track: {json}"
    );
    let back: TimelineCmd = serde_json::from_str(&json).unwrap();
    match back {
        TimelineCmd::MoveClip { new_track, .. } => assert_eq!(new_track, None),
        other => panic!("expected MoveClip, got {other:?}"),
    }
}

/// Load-time validation: a document whose on-disk timeline has overlapping clips
/// is rejected with a load error (repair would require an editorial decision the
/// loader cannot safely make). Corrupt the JSON, then `Document::from_value`.
#[test]
fn load_rejects_overlapping_sequence_on_disk() {
    let f = fixture();
    let mut value = serde_json::to_value(&f.doc).unwrap();

    // Reach the (single) sequence's first video track and overlap clip[1] onto
    // clip[0] by pulling its start back inside clip[0]'s span (0..1000).
    let seq_obj = value["timeline"]["sequences"]
        .as_object_mut()
        .expect("sequences object");
    let seq_val = seq_obj.values_mut().next().expect("one sequence");
    seq_val["video_tracks"][0]["clips"][1]["start"] = serde_json::json!(500);

    let err = Document::from_value(value);
    assert!(
        err.is_err(),
        "an overlapping on-disk timeline must be rejected at load"
    );
}

/// Load-time orphan flagging: a property track whose path does not resolve in
/// the registry survives the load flagged `orphaned == true` (never dropped),
/// and evaluation falls back to `base` (01 §6.2).
#[test]
fn load_flags_orphaned_prop_path_and_eval_returns_base() {
    let f = fixture();
    let mut d = f.doc.clone();
    // Add an unknown-path transform lane to clip A (with a keyframe, so a
    // non-orphaned eval would return the keyframe value, not base).
    {
        let p = d.timeline.as_mut().unwrap();
        let clip = p
            .sequences
            .get_mut(&f.seq)
            .unwrap()
            .track_mut(f.vtrack)
            .unwrap()
            .clips
            .iter_mut()
            .find(|c| c.id == f.clip_a)
            .unwrap();
        let mut lane = PropertyTrack::new(PropPath::new("params.does_not_exist"));
        lane.keyframes.push(Keyframe::new(
            Tick(100),
            PropValue::Float(0.5),
            Interp::Linear,
        ));
        clip.transform.tracks.push(lane);
    }

    let value = serde_json::to_value(&d).unwrap();
    let loaded = Document::from_value(value).expect("orphaned paths are repaired, not rejected");

    let clip = loaded.timeline.as_ref().unwrap().sequences[&f.seq]
        .track(f.vtrack)
        .unwrap()
        .clips
        .iter()
        .find(|c| c.id == f.clip_a)
        .unwrap();
    let lane = clip
        .transform
        .tracks
        .iter()
        .find(|t| t.property.as_str() == "params.does_not_exist")
        .expect("orphaned lane must survive load");
    assert!(lane.orphaned, "unresolved path must be flagged orphaned");
    assert_eq!(lane.keyframes.len(), 1, "keyframes must be preserved");

    // An orphaned lane evaluates to base regardless of its keyframes.
    let base = PropValue::Float(9.0);
    assert_eq!(
        photonic_core::timeline::eval(lane, &base, Tick(100)),
        base,
        "orphaned lane must fall back to base"
    );
}

// ── Compile-time exhaustiveness guard ────────────────────────────────────────
//
// This match forces every `TimelineCmd` (and every sub-command) variant to be
// accounted for by this test module: adding a new variant breaks the build here
// until a maintainer wires it into the round-trip coverage above. It is never
// executed — its sole job is to fail compilation on an un-covered variant.
#[allow(dead_code)]
fn variant_exhaustiveness_guard(cmd: &TimelineCmd) {
    match cmd {
        // Covered by undo_roundtrip_project_and_media / create_and_remove_project.
        TimelineCmd::CreateProject { .. } => {}
        TimelineCmd::RemoveProject { .. } => {} // inverse partner of CreateProject
        TimelineCmd::AddAsset { .. } => {}
        TimelineCmd::RemoveAsset { .. } => {}
        TimelineCmd::RelinkAsset { .. } => {}
        TimelineCmd::SetAssetProxy { .. } => {}
        TimelineCmd::SetAssetMeta { .. } => {}
        TimelineCmd::SetAssetRating { .. } => {} // K-C2
        TimelineCmd::SetAssetTags { .. } => {}   // K-C2
        TimelineCmd::SetAssetTagIds { .. } => {} // K-C2 TagId registry
        TimelineCmd::AddMediaTag { .. } => {}    // K-C2
        TimelineCmd::RemoveMediaTag { .. } => {} // K-C2
        TimelineCmd::SetMediaTag { .. } => {}    // K-C2
        TimelineCmd::SetGenerateProxiesOnImport { .. } => {}
        // Sequences / formats / tracks.
        TimelineCmd::AddSequence { .. } => {}
        TimelineCmd::RemoveSequence { .. } => {}
        TimelineCmd::RenameSequence { .. } => {} // covered in ops::tests (rename_sequence)
        TimelineCmd::SetActiveSequence { .. } => {}
        TimelineCmd::SetActiveFormat { .. } => {}
        TimelineCmd::SetSequenceFormat { .. } => {}
        TimelineCmd::SetSequenceStartTimecode { .. } => {} // K-A12
        TimelineCmd::AddTrack { .. } => {}
        TimelineCmd::RemoveTrack { .. } => {}
        TimelineCmd::SetTrackProp { .. } => {}
        // Clip edits.
        TimelineCmd::InsertClip { .. } => {}
        TimelineCmd::RemoveClip { .. } => {}
        TimelineCmd::MoveClip { .. } => {}
        TimelineCmd::TrimClip { .. } => {}
        TimelineCmd::SplitClip { .. } => {}
        TimelineCmd::MergeSplit { .. } => {} // inverse partner of SplitClip
        TimelineCmd::RippleEdit { .. } => {}
        TimelineCmd::RollEdit { .. } => {}
        TimelineCmd::SlipClip { .. } => {}
        TimelineCmd::SlideClip { .. } => {}
        TimelineCmd::SetClipProp { .. } => {}
        // Keyframes / effects / grade.
        TimelineCmd::SetKeyframe { .. } => {}
        TimelineCmd::RemoveKeyframe { .. } => {}
        TimelineCmd::SetKeyframeInterp { .. } => {}
        TimelineCmd::AddEffect { .. } => {}
        TimelineCmd::RemoveEffect { .. } => {}
        TimelineCmd::ReorderEffects { .. } => {}
        TimelineCmd::SetEffect { .. } => {} // K-B1/K-B2 scoped stack param edit
        TimelineCmd::SetGrade { .. } => {}
        // Graphs / compositions.
        TimelineCmd::AddGraph { .. } => {}
        TimelineCmd::RemoveGraph { .. } => {}
        TimelineCmd::SetClipComposition { .. } => {}
        TimelineCmd::SetProjectGraph { .. } => {}
        // Markers / work range / bins.
        TimelineCmd::AddMarker { .. } => {}
        TimelineCmd::RemoveMarker { .. } => {}
        TimelineCmd::SetMarker { .. } => {}
        // K-A2 — clip markers + the category registry.
        TimelineCmd::AddClipMarker { .. } => {} // clip_marker_ops_roundtrip
        TimelineCmd::RemoveClipMarker { .. } => {}
        TimelineCmd::SetClipMarker { .. } => {}
        TimelineCmd::AddMarkerCategory { .. } => {} // marker_category_crud_roundtrips
        TimelineCmd::RemoveMarkerCategory { .. } => {} // …_reassigns_markers_in_both_scopes
        TimelineCmd::SetMarkerCategory { .. } => {}
        TimelineCmd::SetPreviewZones { .. } => {}
        TimelineCmd::SetWorkRange { .. } => {}
        TimelineCmd::AddBin { .. } => {}
        TimelineCmd::RemoveBin { .. } => {}
        TimelineCmd::AssignAssetBin { .. } => {}
        // Sub-command families — delegate to per-enum guards.
        TimelineCmd::GraphEdit(g) => graph_cmd_guard(g),
        TimelineCmd::CaptionEdit(c) => caption_cmd_guard(c),
        TimelineCmd::TtsEdit(t) => tts_cmd_guard(t),
        TimelineCmd::AudioEdit(a) => audio_cmd_guard(a),
    }
}

#[allow(dead_code)]
fn graph_cmd_guard(cmd: &commands::GraphCmd) {
    use commands::GraphCmd::*;
    match cmd {
        AddNode { .. } => {}
        RemoveNode { .. } => {}
        AddEdge { .. } => {}
        RemoveEdge { .. } => {}
        SetNodeParam { .. } => {}
        SetKeyframe { .. } => {}
        RemoveKeyframe { .. } => {}
        MoveNode { .. } => {}
    }
}

#[allow(dead_code)]
fn caption_cmd_guard(cmd: &commands::CaptionCmd) {
    use commands::CaptionCmd::*;
    match cmd {
        BulkInsertCues { .. } => {}
        UndoBulkInsert { .. } => {} // inverse-only partner of BulkInsertCues
        SetCueText { .. } => {}
        SplitCue { .. } => {}
        MergeCues { .. } => {}
        RetimeCue { .. } => {}
        RetimeWord { .. } => {}
        SetStyle { .. } => {}
    }
}

#[allow(dead_code)]
fn tts_cmd_guard(cmd: &commands::TtsCmd) {
    use commands::TtsCmd::*;
    match cmd {
        GenerateAndPlace { .. } => {}
        Regenerate { .. } => {}
    }
}

#[allow(dead_code)]
fn audio_cmd_guard(cmd: &AudioCmd) {
    use commands::AudioCmd::*;
    match cmd {
        SetTrackAudioProp { .. } => {}
        SetTrackMuteSolo { .. } => {}
        SetClipAudioProp { .. } => {}
        SetClipFade { .. } => {}
        SetChannelMap { .. } => {}
        AddAudioFx { .. } => {}
        RemoveAudioFx { .. } => {}
        ReorderAudioFx { .. } => {}
        SetMasterBusProp { .. } => {}
        SetLoudnessTarget { .. } => {}
        ApplyDuckingPreset { .. } => {}
    }
}

// ── Serde round-trip ────────────────────────────────────────────────────────

#[test]
fn fully_populated_project_round_trips() {
    let f = fixture();
    let mut d = f.doc.clone();
    // Enrich: add an effect, a grade, a keyframe, a composition, captions, markers.
    let steps: Vec<TimelineCmd> = {
        let p = d.timeline.as_ref().unwrap();
        let mut v = vec![
            ops::add_effect(
                p,
                f.seq,
                f.vtrack,
                f.clip_a,
                ClipEffect::new(EffectKind::Glow),
                None,
            )
            .unwrap(),
            ops::set_grade(
                p,
                f.seq,
                f.vtrack,
                f.clip_a,
                Some(Grade {
                    ops: vec![GradeOp::new(
                        GradeOpKind::Exposure,
                        GradeOpParams::Exposure { stops: 1.0 },
                    )],
                    bypass: false,
                }),
            )
            .unwrap(),
            ops::set_keyframe(
                p,
                AnimTarget::ClipTransform { clip: f.clip_b },
                PropPath::new("transform.x"),
                Keyframe::new(
                    Tick(0),
                    PropValue::Float(0.0),
                    Interp::Bezier {
                        out_handle: [0.42, 0.0],
                        in_handle: [0.58, 1.0],
                    },
                ),
            ),
        ];
        v.extend(ops::create_clip_composition(p, f.seq, f.vtrack, f.clip_b).unwrap());
        v
    };
    for c in steps {
        Command::Timeline(c).apply(&mut d);
    }
    // Add a caption track + marker directly on the sequence.
    {
        let p = d.timeline.as_mut().unwrap();
        let s = p.sequences.get_mut(&f.seq).unwrap();
        let mut ct = CaptionTrack::new("Captions");
        ct.cues.push(CaptionCue::new(
            Tick(0),
            Tick(500),
            vec![
                CaptionWord::new("hello", Tick(0), Tick(250)),
                CaptionWord::new("world", Tick(250), Tick(500)),
            ],
        ));
        s.caption_tracks.push(ct);
        s.markers.push(Marker::new(Tick(100), "cue"));
        s.work_range = Some((Tick(0), Tick(1500)));
    }
    // K-A2: the category registry, a categorized RANGED sequence marker, and a
    // clip-scoped marker must all survive the round-trip. Each is
    // serde-additive on v5 — no format bump, so a v5 file written before K-A2
    // still loads (`marker_categories` / `Clip.markers` default to empty and
    // `duration` to 0, i.e. a point marker).
    {
        let p = d.timeline.as_mut().unwrap();
        p.marker_categories = MarkerCategory::default_seed();
        let mut cat = p.marker_categories[3].clone();
        cat.glyph = MarkerGlyph::Flag;
        let cat_id = cat.id;
        p.marker_categories[3] = cat;
        let s = p.sequences.get_mut(&f.seq).unwrap();
        let mut ranged = Marker::new(Tick(300), "chapter one");
        ranged.duration = Tick(600);
        ranged.category = Some(cat_id);
        ranged.note = "review this".into();
        assert!(ranged.is_range(), "fixture must exercise the ranged arm");
        s.markers.push(ranged);
        // A clip marker referencing a category that is NOT in the registry —
        // the 35 §1.3 dangling case must survive load verbatim rather than
        // being repaired or dropped.
        let mut orphan = Marker::clip_scoped(Tick(10), "orphaned category");
        orphan.category = Some(MarkerCategoryId::new());
        s.video_tracks[0].clips[0].markers.push(orphan);
    }

    let project = d.timeline.as_ref().unwrap();
    let json = serde_json::to_string(project).unwrap();
    let back: TimelineProject = serde_json::from_str(&json).unwrap();
    assert_eq!(project, &back, "TimelineProject serde round-trip mismatch");

    // And the whole Document round-trips (with the timeline present).
    let dj = serde_json::to_string(&d).unwrap();
    let dback: Document = serde_json::from_str(&dj).unwrap();
    assert_eq!(dback.timeline.as_ref(), Some(&back));
}

// ── Migration: v2 files load unchanged ──────────────────────────────────────

#[test]
fn v2_document_loads_as_current_without_timeline() {
    use photonic_core::migration::{detect_version, run_migrations};

    // A v2 document is the current serialization minus `timeline`, with
    // format_version pinned to 2.
    let mut value = serde_json::to_value(Document::new("legacy", 640.0, 480.0)).unwrap();
    let obj = value.as_object_mut().unwrap();
    obj.insert("format_version".into(), serde_json::json!(2));
    obj.remove("timeline");
    assert_eq!(detect_version(&value), 2);

    // Migrate forward to the current version and deserialize.
    let out = run_migrations(&mut value, photonic_core::document::CURRENT_FORMAT_VERSION).unwrap();
    assert_eq!(out, 5, "v2 must migrate through v3/v4 to v5");
    let doc: Document = serde_json::from_value(value).unwrap();
    assert_eq!(doc.format_version, 5);
    assert!(doc.timeline.is_none(), "v2 file must load with no timeline");
    assert_eq!(doc.name, "legacy");
}

// ── Proptest: random edits never break non-overlap ──────────────────────────

use proptest::prelude::*;

#[derive(Debug, Clone)]
enum RandomEdit {
    Insert {
        start: i64,
        dur: i64,
    },
    Move {
        idx: usize,
        start: i64,
    },
    Trim {
        idx: usize,
        start: i64,
        dur: i64,
    },
    Remove {
        idx: usize,
    },
    Split {
        idx: usize,
        at: i64,
    },
    RippleTrimEnd {
        idx: usize,
        boundary: i64,
    },
    RippleTrimStart {
        idx: usize,
        boundary: i64,
    },
    /// Roll the shared edit point between clips `idx` and `idx + 1`.
    Roll {
        idx: usize,
        delta: i64,
    },
    /// Slide clip `idx` over its neighbours by `delta`.
    Slide {
        idx: usize,
        delta: i64,
    },
}

fn edit_strategy() -> impl Strategy<Value = RandomEdit> {
    prop_oneof![
        (0i64..5000, 1i64..2000).prop_map(|(start, dur)| RandomEdit::Insert { start, dur }),
        (0usize..8, 0i64..6000).prop_map(|(idx, start)| RandomEdit::Move { idx, start }),
        (0usize..8, 0i64..5000, 1i64..2000).prop_map(|(idx, start, dur)| RandomEdit::Trim {
            idx,
            start,
            dur
        }),
        (0usize..8).prop_map(|idx| RandomEdit::Remove { idx }),
        (0usize..8, 0i64..6000).prop_map(|(idx, at)| RandomEdit::Split { idx, at }),
        (0usize..8, 0i64..6000)
            .prop_map(|(idx, boundary)| RandomEdit::RippleTrimEnd { idx, boundary }),
        (0usize..8, 0i64..6000)
            .prop_map(|(idx, boundary)| RandomEdit::RippleTrimStart { idx, boundary }),
        (0usize..8, -1500i64..1500).prop_map(|(idx, delta)| RandomEdit::Roll { idx, delta }),
        (0usize..8, -1500i64..1500).prop_map(|(idx, delta)| RandomEdit::Slide { idx, delta }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// A random sequence of edit ops — each applied only if the op accepts it —
    /// must leave every sequence satisfying `validate()` (sorted, non-overlapping,
    /// duration > 0). Ops that would violate invariants return `Err` and are
    /// skipped, so the invariant is an unconditional postcondition (11 §3.1).
    #[test]
    fn random_edits_preserve_non_overlap(edits in prop::collection::vec(edit_strategy(), 0..40)) {
        let f = fixture();
        let mut doc = f.doc.clone();

        for edit in edits {
            let p = doc.timeline.as_ref().unwrap();
            let clips = &p.sequences[&f.seq].track(f.vtrack).unwrap().clips;
            let nth_id = |i: usize| clips.get(i).map(|c| c.id);

            let cmds: Option<Vec<TimelineCmd>> = match edit {
                RandomEdit::Insert { start, dur } => {
                    ops::insert_clip(
                        p, f.seq, f.vtrack,
                        Clip::new(ClipSource::Adjustment, Tick(start), Tick(dur)),
                    ).ok().map(|c| vec![c])
                }
                RandomEdit::Move { idx, start } => {
                    nth_id(idx).and_then(|id| ops::move_clip(p, f.seq, f.vtrack, id, Tick(start)).ok()).map(|c| vec![c])
                }
                RandomEdit::Trim { idx, start, dur } => {
                    nth_id(idx).and_then(|id| ops::trim_clip(
                        p, f.seq, f.vtrack, id,
                        ClipTiming { start: Tick(start), duration: Tick(dur), source_in: Tick(0) },
                    ).ok()).map(|c| vec![c])
                }
                RandomEdit::Remove { idx } => {
                    nth_id(idx).and_then(|id| ops::remove_clip(p, f.seq, f.vtrack, id).ok()).map(|c| vec![c])
                }
                RandomEdit::Split { idx, at } => {
                    nth_id(idx).and_then(|id| ops::split_clip(p, f.seq, f.vtrack, id, Tick(at)).ok()).map(|c| vec![c])
                }
                RandomEdit::RippleTrimEnd { idx, boundary } => {
                    nth_id(idx).and_then(|id| ops::ripple_trim(
                        p, f.seq, f.vtrack, id, ClipEdge::End, Tick(boundary),
                    ).ok())
                }
                RandomEdit::RippleTrimStart { idx, boundary } => {
                    nth_id(idx).and_then(|id| ops::ripple_trim(
                        p, f.seq, f.vtrack, id, ClipEdge::Start, Tick(boundary),
                    ).ok())
                }
                RandomEdit::Roll { idx, delta } => {
                    // Roll the shared edit between adjacent clips `idx`, `idx + 1`.
                    match (nth_id(idx), nth_id(idx + 1)) {
                        (Some(l), Some(r)) => {
                            ops::roll_edit(p, f.seq, f.vtrack, l, r, Tick(delta)).ok().map(|c| vec![c])
                        }
                        _ => None,
                    }
                }
                RandomEdit::Slide { idx, delta } => {
                    nth_id(idx).and_then(|id| ops::slide_clip(p, f.seq, f.vtrack, id, Tick(delta)).ok()).map(|c| vec![c])
                }
            };

            if let Some(cmds) = cmds {
                for cmd in cmds {
                    Command::Timeline(cmd).apply(&mut doc);
                }
                // The invariant must hold after every accepted edit.
                let seq = &doc.timeline.as_ref().unwrap().sequences[&f.seq];
                prop_assert!(seq.validate().is_ok(), "invariant broken: {:?}", seq.validate());
            }
        }
    }
}
