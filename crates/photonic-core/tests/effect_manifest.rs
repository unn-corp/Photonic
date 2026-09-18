//! Integration coverage for the data-driven effect manifest (spec 30 §2, §10).
//!
//! The brief scopes these to `tests/timeline.rs`, but that file is outside this
//! change's file boundary, so the manifest's load-path assertions live here in a
//! dedicated integration test instead. The pure-schema and migration invariants
//! are covered by the inline `#[cfg(test)]` module in `effect_manifest.rs`; this
//! file exercises the *load seam* (`Document::from_value` → `finalize_load`):
//! id/version backfill, unknown-id inert preservation, and round-trip fidelity.

use photonic_core::timeline::{
    Clip, ClipEffect, ClipSource, EffectId, EffectKind, EffectParams, FrameRate, PropValue,
    Sequence, Tick, TimelineProject, Track, TrackKind, UnknownTag,
};
use photonic_core::Document;
use serde_json::Value;

/// Three explicit params that must survive an inert load byte-for-byte.
fn three_params() -> EffectParams {
    let mut p = EffectParams::new();
    p.set("params.a", PropValue::Float(0.25));
    p.set("params.b", PropValue::Float(7.5));
    p.set("params.c", PropValue::Bool(true));
    p
}

/// A one-clip video document whose single clip carries `effect`.
fn doc_with_effect(effect: ClipEffect) -> Document {
    let mut project = TimelineProject::new();
    let mut seq = Sequence::new("Seq 1", FrameRate::FPS_30, 1920, 1080);
    let mut track = Track::new(TrackKind::Video, "V1");
    let mut clip = Clip::new(ClipSource::Adjustment, Tick(0), Tick(1000));
    clip.effects.push(effect);
    track.clips.push(clip);
    seq.video_tracks.push(track);
    project.insert_sequence(seq);

    let mut doc = Document::new("t", 100.0, 100.0);
    doc.timeline = Some(project);
    doc
}

/// The single effect of a loaded one-clip document.
fn only_effect(doc: &Document) -> &ClipEffect {
    &doc.timeline
        .as_ref()
        .unwrap()
        .sequences
        .values()
        .next()
        .unwrap()
        .video_tracks[0]
        .clips[0]
        .effects[0]
}

/// A v4-shaped effect (no `id`/`version` on the wire, since `ClipEffect::new`
/// leaves them at their skipped sentinels) must gain its manifest id and version
/// backfilled from `kind` at load — spec §10.
#[test]
fn clip_effect_v4_json_loads_and_backfills_id() {
    let doc = doc_with_effect(ClipEffect::new(EffectKind::Blur));

    // The v4 wire shape carries no id/version (both skipped at their sentinels).
    let json = doc.to_json().unwrap();
    let v: Value = serde_json::from_str(&json).unwrap();
    let fx = &v["timeline"]["sequences"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap()["video_tracks"][0]["clips"][0]["effects"][0];
    assert!(fx.get("id").is_none(), "v4 shape must not carry an id");
    assert!(
        fx.get("version").is_none(),
        "v4 shape must not carry a version"
    );

    // Loading backfills id and version from the legacy kind.
    let loaded = Document::from_json(&json).expect("v4 effect loads");
    let fx = only_effect(&loaded);
    assert_eq!(fx.id.as_str(), "blur.gaussian");
    assert_eq!(fx.version, 1);
    assert_eq!(fx.kind, EffectKind::Blur);
    assert!(!fx.inert && fx.enabled);
}

/// An effect whose `id` this build has no manifest for loads inert-and-preserved
/// (§2.6): disabled, flagged inert, params untouched — never guessed at a known
/// kind, never dropped.
#[test]
fn unknown_effect_id_loads_inert_and_params_preserved() {
    let mut fx = ClipEffect::new(EffectKind::Blur);
    fx.id = EffectId::new("future.thing".to_string());
    fx.kind = EffectKind::Unknown(UnknownTag::intern("future.thing"));
    // Three explicit params that must survive the load byte-for-byte.
    fx.params.base = three_params();
    let params_before = fx.params.base.clone();

    let doc = doc_with_effect(fx);
    let json = doc.to_json().unwrap();
    let loaded = Document::from_json(&json).expect("an unknown-id effect loads");
    let fx = only_effect(&loaded);

    assert!(fx.inert, "an unknown-manifest effect must load inert");
    assert!(!fx.enabled, "an inert effect is disabled");
    assert_eq!(
        fx.id.as_str(),
        "future.thing",
        "the unknown id is preserved"
    );
    assert_eq!(
        fx.params.base, params_before,
        "params must be left untouched"
    );
}

/// The idempotence half of §2.6: once loaded inert, re-serializing and reloading
/// is a fixed point, and the effect subtree round-trips value-equal.
#[test]
fn unknown_effect_id_reserializes_unchanged() {
    let mut fx = ClipEffect::new(EffectKind::Blur);
    fx.id = EffectId::new("future.thing".to_string());
    fx.kind = EffectKind::Unknown(UnknownTag::intern("future.thing"));
    fx.enabled = false;
    fx.inert = true;
    fx.params.base = three_params();

    // x = the already-inert on-disk state.
    let x = doc_with_effect(fx).to_json().unwrap();
    // load(x) → save must equal x (fixed point; params untouched).
    let y = Document::from_json(&x).unwrap().to_json().unwrap();
    let vx: Value = serde_json::from_str(&x).unwrap();
    let vy: Value = serde_json::from_str(&y).unwrap();
    assert_eq!(
        vy, vx,
        "loading an inert effect and re-saving must be byte-identical"
    );
}

/// A newer build may author an explicit id for one of the seven known effects;
/// the load keeps that id and reconciles `kind` from it (round-trip preserves
/// the known id).
#[test]
fn clip_effect_roundtrip_preserves_known_explicit_id() {
    let fx = ClipEffect::from_manifest(EffectId::new_static("stylize.glow"))
        .expect("glow has a manifest");
    assert_eq!(fx.kind, EffectKind::Glow);
    assert_eq!(fx.version, 1);

    let doc = doc_with_effect(fx);
    let json = doc.to_json().unwrap();
    let loaded = Document::from_json(&json).expect("explicit-id effect loads");
    let fx = only_effect(&loaded);
    assert_eq!(fx.id.as_str(), "stylize.glow");
    assert_eq!(fx.kind, EffectKind::Glow);
    assert!(!fx.inert);
}
