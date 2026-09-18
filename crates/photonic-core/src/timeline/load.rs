//! Load-time timeline finalization (01 §4 invariant enforcement, §6.2 orphaned
//! paths).
//!
//! `Document` derives `Deserialize`, so raw serde cannot run the timeline
//! invariants. Instead [`Document::from_value`](crate::document::Document::from_value)
//! — the single load seam shared by `from_json` and the `.photon` wrapper —
//! calls [`finalize_load`] on the deserialized project. Two passes run there:
//!
//! 1. **Orphan flagging (repair).** Every [`PropertyTrack`] whose
//!    [`PropPath`](super::anim::PropPath) does not resolve in
//!    [`prop_registry`](super::prop_registry) for its owning target kind is
//!    flagged [`orphaned`](super::anim::PropertyTrack::orphaned) rather than
//!    dropped (01 §6.2): an asset/plugin may register the path later, and eval
//!    falls back to `base`. This is idempotent and re-derived on every load, so a
//!    path that becomes known un-orphans automatically.
//!
//! 2. **Sequence validation (reject).** Each loaded [`Sequence`] must satisfy
//!    [`Sequence::validate`] (sorted, non-overlapping, `duration > 0`). A file
//!    whose on-disk clips overlap is *rejected* with a load error rather than
//!    silently repaired: closing a gap requires an editorial decision (which clip
//!    moves, by how much) that the loader cannot make without risking data loss,
//!    so failing loudly is safer than guessing.
//!
//! The effect/grade/audio-fx param bags all declare
//! [`PropSet::TARGET_KIND`](super::anim::PropSet::TARGET_KIND) `= GraphNode`
//! (their generic surface is intentionally lenient), so orphan resolution here
//! keys off the *concrete* kind field (`EffectKind`, `GradeOpKind`, `AudioFxKind`)
//! rather than the generic bound — that is where the real registry blocks live.

use super::anim::PropertyTrack;
use super::clip::{ClipEffect, ClipSource};
use super::grade::{Grade, GradeOpParams};
use super::graph::GraphOp;
use super::ids::{
    ClipId, GradeOpId, GraphId, GraphNodeId, GroupId, MarkerCategoryId, MarkerId, SequenceId,
    TrackId,
};
use super::prop_registry::{self, PropTargetKind};
use super::sequence::{TimelineProject, ValidationError};

/// The error returned by [`finalize_load`]: either a per-sequence invariant
/// failure or a corrupt-known-variant rejection (39 §2.2 rule 4). Kept in this
/// crate as a plain `Display` error — [`Document::from_value`] maps it straight
/// into a `serde_json::Error` via `serde::de::Error::custom`.
///
/// [`Document::from_value`]: crate::document::Document::from_value
// `Eq` is intentionally NOT derived: the `Validation` arm wraps
// `ValidationError`, which is not `Eq`. `PartialEq` is sufficient for the
// `assert_eq!`-based tests and for `Document::from_value`'s error mapping.
#[derive(Clone, Debug, PartialEq)]
pub enum LoadError {
    /// A loaded [`Sequence`](super::sequence::Sequence) failed its invariants
    /// (sorted / non-overlapping / positive duration).
    Validation(ValidationError),
    /// A KNOWN internally-tagged variant's payload was malformed and serde's
    /// greedy per-variant `#[serde(untagged)] Unknown` fallback swallowed it,
    /// retaining the KNOWN tag. That is data corruption, not a forward-compat
    /// variant, so the load rejects it rather than rendering it inert.
    CorruptKnownVariant {
        /// The enum whose known variant was corrupted (`"ClipSource"`,
        /// `"GraphOp"`, `"GradeOpParams"`).
        enum_name: &'static str,
        /// The known serialized tag whose payload failed to deserialize.
        tag: String,
    },
}

impl LoadError {
    fn corrupt(enum_name: &'static str, tag: &str) -> Self {
        LoadError::CorruptKnownVariant {
            enum_name,
            tag: tag.to_owned(),
        }
    }
}

impl From<ValidationError> for LoadError {
    fn from(e: ValidationError) -> Self {
        LoadError::Validation(e)
    }
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Validation(e) => write!(f, "{e}"),
            LoadError::CorruptKnownVariant { enum_name, tag } => write!(
                f,
                "corrupt {enum_name}: the known variant `{tag}` has a malformed \
                 payload — this build cannot load it, and it is not a \
                 forward-compatible unknown variant (39 §2.2)"
            ),
        }
    }
}

impl std::error::Error for LoadError {}

/// Flag every track in `tracks` orphaned iff its path is unknown for `kind`.
/// Re-derived from scratch each call (a previously-orphaned path that is now
/// registered clears its flag, and vice versa).
fn flag_tracks(tracks: &mut [PropertyTrack], kind: PropTargetKind) {
    for t in tracks {
        t.orphaned = prop_registry::resolve(kind, t.property.as_str()).is_none();
    }
}

/// Flag orphaned lanes across an effect stack plus an optional grade — the walk
/// shared by every effect scope (35 §2): clip, track, sequence-master and asset.
/// An inert effect (unknown manifest id) is skipped so its preserved params stay
/// byte-identical on re-serialization (§2.6), exactly as the clip loop requires.
fn flag_effect_stack(effects: &mut [ClipEffect], grade: Option<&mut Grade>) {
    for effect in effects {
        if effect.inert {
            continue;
        }
        flag_tracks(
            &mut effect.params.tracks,
            PropTargetKind::Effect(effect.kind),
        );
    }
    if let Some(grade) = grade {
        for op in &mut grade.ops {
            flag_tracks(&mut op.params.tracks, PropTargetKind::GradeOp(op.kind));
        }
    }
}

/// Run the load-time passes over a freshly deserialized project: flag orphaned
/// property tracks (repair), reject any corrupt known variant serde swallowed
/// into the `Unknown` fallback (39 §2.2 rule 4), scan for preserved unknown enum
/// variants (diagnose once — 39 §2.2 rule 3), and validate every sequence
/// (reject on violation).
///
/// The returned [`LoadReport`] carries the unknown-variant findings so a caller
/// can surface them exactly once per load; existing callers that discard it are
/// unaffected.
pub fn finalize_load(project: &mut TimelineProject) -> Result<LoadReport, LoadError> {
    finalize_effect_ids(project);
    flag_orphaned_property_tracks(project);
    // Integrity guard BEFORE diagnosing/validating: a retained `Unknown` whose
    // tag is a KNOWN catalog tag is corrupt data (a malformed known variant that
    // serde swallowed), not a forward-compat variant — reject it (39 §2.2 rule 4).
    reject_corrupt_known_variants(project)?;
    // Repair degenerate groups BEFORE validation (35 §3). Empty and
    // singleton-`Normal` groups are self-repairing, and rejecting a whole
    // project over one degenerate group would be hostile — so they are dissolved
    // here (and reported, not silently swallowed) rather than left for
    // `validate` to reject. Cycles and unknown group refs are NOT repaired:
    // closing them needs an editorial decision the loader cannot make, so they
    // stay load-rejections via `validate` below (the same argument the module
    // doc makes for overlapping clips). Sequences are visited in id-sorted order
    // for a deterministic report.
    let mut dissolved_groups: Vec<(SequenceId, GroupId)> = Vec::new();
    let mut seq_ids: Vec<SequenceId> = project.sequences.keys().copied().collect();
    seq_ids.sort();
    for seq_id in &seq_ids {
        let seq = project
            .sequences
            .get_mut(seq_id)
            .expect("id came from keys()");
        for gid in seq.dissolve_degenerate_groups() {
            dissolved_groups.push((*seq_id, gid));
        }
    }
    // Pre-validation migration (01 §5): a `transition_out` sitting at a hard cut
    // is invalid (the incoming clip's `transition_in` owns the blend there), so
    // `validate` below would reject the whole document. Rather than fail an old
    // file, DROP the offending transition and record a typed notice. Runs after
    // group repair, before validation.
    let notices = migrate_transition_out_at_cut(project);
    let report = LoadReport {
        unknown_variants: collect_unknown_variants(project),
        dissolved_groups,
        // A marker whose category is missing renders neutral and is flagged; it
        // is NEVER silently remapped (35 §1.3), so this only reports.
        dangling_categories: dangling_marker_categories(project),
        notices,
    };
    for seq in project.sequences.values() {
        seq.validate()?;
    }
    // TODO(39 §2.4): surface `report.dissolved_groups` and
    // `report.dangling_categories` on 36's diagnostic channel
    // (`Project::ValidationFailed` / `Project::UnknownVariantPreserved`). Until
    // that taxonomy lands, the returned `LoadReport` is the interim carrier.
    Ok(report)
}

/// Which marker collection a [`dangling_marker_categories`] finding came from
/// (35 §1.3). Scope is implied by location: [`Sequence::markers`] vs a clip's
/// [`markers`](super::clip::Clip).
///
/// [`Sequence::markers`]: super::sequence::Sequence
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarkerScope {
    /// A sequence-level marker.
    Sequence(SequenceId),
    /// A clip-level marker.
    Clip {
        sequence: SequenceId,
        track: TrackId,
        clip: ClipId,
    },
}

/// Every marker whose `category` names a [`MarkerCategory`] absent from
/// `project.marker_categories` (35 §1.3). Such a marker renders with a neutral
/// fallback and is flagged in the panel; it is NEVER silently remapped, so this
/// only *reports* — it does not mutate. Sequences are visited in id-sorted order
/// so the result is deterministic.
///
/// [`MarkerCategory`]: super::sequence::MarkerCategory
pub fn dangling_marker_categories(
    project: &TimelineProject,
) -> Vec<(MarkerScope, MarkerId, MarkerCategoryId)> {
    let mut out = Vec::new();
    let mut seq_ids: Vec<SequenceId> = project.sequences.keys().copied().collect();
    seq_ids.sort();
    for seq_id in seq_ids {
        let seq = &project.sequences[&seq_id];
        for marker in &seq.markers {
            if let Some(cat) = marker.category {
                if project.marker_category(cat).is_none() {
                    out.push((MarkerScope::Sequence(seq_id), marker.id, cat));
                }
            }
        }
        for track in seq.video_tracks.iter().chain(seq.audio_tracks.iter()) {
            for clip in &track.clips {
                for marker in &clip.markers {
                    if let Some(cat) = marker.category {
                        if project.marker_category(cat).is_none() {
                            out.push((
                                MarkerScope::Clip {
                                    sequence: seq_id,
                                    track: track.id,
                                    clip: clip.id,
                                },
                                marker.id,
                                cat,
                            ));
                        }
                    }
                }
            }
        }
    }
    out
}

// ── transition_out-at-a-cut migration (01 §5) ────────────────────────────────

/// Pre-validation migration (01 §5): a `transition_out` is a fade to transparent
/// into a gap/end. At a hard cut — the next clip on the track is *adjacent*, no
/// gap — the incoming clip's `transition_in` owns the blend, so an outgoing
/// transition there is invalid and [`Sequence::validate`] rejects it. Rather than
/// fail an old document that predates the rule, DROP each such `transition_out`
/// and record a [`LoadNotice`] so the caller can surface it.
///
/// A `transition_out` on the LAST clip of a track (or one whose next clip leaves
/// a gap) fades into a gap/end and is left untouched. Sequences and tracks are
/// visited in id-sorted order so the notice list is deterministic.
fn migrate_transition_out_at_cut(project: &mut TimelineProject) -> Vec<LoadNotice> {
    let mut notices = Vec::new();
    let mut seq_ids: Vec<SequenceId> = project.sequences.keys().copied().collect();
    seq_ids.sort();
    for seq_id in seq_ids {
        let seq = project
            .sequences
            .get_mut(&seq_id)
            .expect("id came from keys()");
        for track in seq
            .video_tracks
            .iter_mut()
            .chain(seq.audio_tracks.iter_mut())
        {
            // Indices whose `transition_out` sits at a hard cut (next clip
            // adjacent). Collected first so the mutating pass borrows cleanly.
            let cut_indices: Vec<usize> = track
                .clips
                .iter()
                .enumerate()
                .filter(|(i, clip)| {
                    clip.transition_out.is_some()
                        && track
                            .clips
                            .get(i + 1)
                            .is_some_and(|next| next.start == clip.end())
                })
                .map(|(i, _)| i)
                .collect();
            for i in cut_indices {
                let clip = &mut track.clips[i];
                clip.transition_out = None;
                notices.push(LoadNotice {
                    code: LoadNoticeCode::TransitionOutAtCutDropped,
                    message: format!(
                        "dropped transition_out on clip {} at a hard cut: the next \
                         clip is adjacent, so its transition_in owns the blend",
                        clip.id
                    ),
                    subject: Some(NoticeSubject {
                        sequence: seq_id,
                        track: track.id,
                        clip: clip.id,
                    }),
                });
            }
        }
    }
    notices
}

// ── Corrupt-known-variant guard (39 §2.2 rule 4) ─────────────────────────────

/// Serialized `source`-tags of the KNOWN [`ClipSource`] variants. Keep in sync
/// with the enum (clip.rs); the `Unknown` fallback is deliberately excluded.
pub const KNOWN_CLIP_SOURCE_TAGS: &[&str] = &[
    "asset",
    "vector",
    "nested_sequence",
    "solid_color",
    "adjustment",
    "text",
];

/// Serialized `kind`-tags of the KNOWN [`GradeOpParams`] variants (grade.rs).
pub const KNOWN_GRADE_PARAM_TAGS: &[&str] = &[
    "exposure",
    "contrast",
    "white_balance",
    "cdl",
    "wheels",
    "curves",
    "hsl_qualifier",
    "lut3d",
];

/// Serialized `op`-tags of the KNOWN [`GraphOp`] variants (graph.rs).
pub const KNOWN_GRAPH_OP_TAGS: &[&str] = &[
    "output",
    "clip_in",
    "media_in",
    "vector_in",
    "solid_color",
    "merge",
    "transform2_d",
    "crop",
    "resize",
    "blur",
    "sharpen",
    "glow",
    "chroma_key",
    "luma_key",
    "mask_shape",
    "mask_from_matte",
    "invert",
    "channel_split",
    "channel_combine",
    "grade",
    "lut",
    "text",
    "time_offset",
    "switch",
    "note",
];

/// The document-integrity half of 39 §2.2 rule 4. The three payload-carrying
/// enums (`ClipSource`, `GraphOp`, `GradeOpParams`) are internally tagged with a
/// per-variant `#[serde(untagged)] Unknown` fallback. That fallback is *greedy*:
/// a KNOWN tag whose payload is malformed deserializes to `Unknown` retaining
/// its original (known) tag rather than erroring. Loading such a value inert
/// would silently accept corrupt data, so any retained `Unknown` whose tag is a
/// known catalog tag is rejected. A genuine forward-compat variant always
/// carries a tag this build does not know, so it is never caught here (it is
/// preserved and diagnosed instead).
///
/// The four fieldless discriminant enums (`EffectKind`, `TransitionKind`,
/// `AudioFxKind`, `GradeOpKind`) serialize as bare strings and have no malformed
/// payload to swallow, so they need no such guard.
fn reject_corrupt_known_variants(project: &TimelineProject) -> Result<(), LoadError> {
    for seq in project.sequences.values() {
        for track in seq.video_tracks.iter().chain(seq.audio_tracks.iter()) {
            for clip in &track.clips {
                if let ClipSource::Unknown(map) = &clip.source {
                    let tag = map
                        .get("source")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    if KNOWN_CLIP_SOURCE_TAGS.contains(&tag) {
                        return Err(LoadError::corrupt("ClipSource", tag));
                    }
                }
                if let Some(grade) = clip.grade.as_ref() {
                    for op in &grade.ops {
                        if let GradeOpParams::Unknown(map) = &op.params.base {
                            let tag = map.get("kind").and_then(|v| v.as_str()).unwrap_or_default();
                            if KNOWN_GRADE_PARAM_TAGS.contains(&tag) {
                                return Err(LoadError::corrupt("GradeOpParams", tag));
                            }
                        }
                    }
                }
            }
        }
    }
    for graph in project.graphs.values() {
        for node in graph.nodes.values() {
            if let GraphOp::Unknown(map) = &node.op {
                let tag = map.get("op").and_then(|v| v.as_str()).unwrap_or_default();
                if KNOWN_GRAPH_OP_TAGS.contains(&tag) {
                    return Err(LoadError::corrupt("GraphOp", tag));
                }
            }
        }
    }
    Ok(())
}

/// The orphan-flagging pass in isolation (see module docs). Walks every
/// animatable `AnimProps` lane the project owns and flags unresolved paths.
pub fn flag_orphaned_property_tracks(project: &mut TimelineProject) {
    for seq in project.sequences.values_mut() {
        for track in seq
            .video_tracks
            .iter_mut()
            .chain(seq.audio_tracks.iter_mut())
        {
            if let Some(audio) = track.audio.as_mut() {
                flag_tracks(&mut audio.params.tracks, PropTargetKind::TrackAudioParams);
                for unit in &mut audio.fx_chain {
                    flag_tracks(&mut unit.params.tracks, PropTargetKind::AudioFx(unit.kind));
                }
            }
            // Track-level effect/grade scope (35 §2).
            flag_effect_stack(&mut track.effects, track.grade.as_mut());
            for clip in &mut track.clips {
                flag_tracks(&mut clip.transform.tracks, PropTargetKind::ClipTransform);
                flag_effect_stack(&mut clip.effects, clip.grade.as_mut());
                if let Some(audio) = clip.audio.as_mut() {
                    flag_tracks(&mut audio.params.tracks, PropTargetKind::ClipAudioParams);
                }
            }
        }
        // Master bus params + its fx chain.
        flag_tracks(
            &mut seq.audio_master.params.tracks,
            PropTargetKind::MasterBusParams,
        );
        for unit in &mut seq.audio_master.fx_chain {
            flag_tracks(&mut unit.params.tracks, PropTargetKind::AudioFx(unit.kind));
        }
        // Sequence-master effect/grade scope (35 §2).
        flag_effect_stack(&mut seq.master_effects, seq.master_grade.as_mut());
    }
    // Asset-level effect/grade scope (35 §2): walked once per asset, outside the
    // per-sequence loop.
    for asset in project.media.assets.values_mut() {
        flag_effect_stack(&mut asset.effects, asset.grade.as_mut());
    }
    // Graph-node params resolve leniently (`PropTargetKind::GraphNode` accepts
    // any path), so they are never orphaned — no pass needed for `project.graphs`.
}

// ── Effect id backfill / migration / inert preservation (spec 30 §2.6, §10) ──

/// Reconcile each clip effect's manifest identity, in place, before orphan
/// flagging runs (spec §10):
///
/// 1. **Backfill.** An absent `id` (empty sentinel — a v4 file) is filled from
///    the effect's legacy `kind`; conversely, an `id` that maps to a legacy kind
///    overwrites `kind`, keeping the two consistent.
/// 2. **Migrate.** For a known id whose stored `version` is older than the
///    manifest's, [`migrate`](super::effect_manifest::migrate) advances the
///    params to the current version. A pre-versioning sentinel (`version == 0`)
///    is assumed current and simply stamped, not migrated.
/// 3. **Inert preservation.** An id with no manifest (a future build's effect)
///    is marked `inert` and disabled, its `params` left untouched so
///    re-serialization is byte-identical (§2.6). Its `kind` is left as its serde
///    default (an `Unknown` tag), so the unknown-variant scan still reports it.
///
/// Load-time migration is deliberately *not* an undoable edit (§10): it runs
/// here, before the history is constructed, so no history code is involved.
fn finalize_effect_ids(project: &mut TimelineProject) {
    use super::effect_manifest::{manifest, migrate};
    for seq in project.sequences.values_mut() {
        for track in seq
            .video_tracks
            .iter_mut()
            .chain(seq.audio_tracks.iter_mut())
        {
            for clip in &mut track.clips {
                for effect in &mut clip.effects {
                    if effect.id.is_empty() {
                        effect.id = effect.kind.effect_id();
                    }
                    if let Some(k) = effect.id.legacy_kind() {
                        effect.kind = k;
                    }
                    match manifest(effect.id.clone()) {
                        Some(m) => {
                            if effect.version == 0 {
                                effect.version = m.version;
                            } else if effect.version < m.version {
                                if let Ok(v) =
                                    migrate(&effect.id, effect.version, &mut effect.params.base)
                                {
                                    effect.version = v;
                                }
                            }
                        }
                        None => {
                            effect.inert = true;
                            effect.enabled = false;
                        }
                    }
                }
            }
        }
    }
}

// ── Unknown-variant scan (39 §2.2 rule 3) ───────────────────────────────────

/// Where the first occurrence of an unknown variant was found. Enough to name
/// the offending clip/node in a diagnostic once 36 §3's `Subject` taxonomy
/// lands; until then this interim carrier is what [`LoadReport`] exposes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnknownSite {
    Clip {
        sequence: SequenceId,
        track: TrackId,
        clip: ClipId,
    },
    Effect {
        clip: ClipId,
        index: usize,
    },
    Transition {
        clip: ClipId,
    },
    GradeOp {
        clip: ClipId,
        op: GradeOpId,
    },
    GraphNode {
        graph: GraphId,
        node: GraphNodeId,
    },
    AudioFx {
        /// `None` for a master-bus fx unit; `Some` for a track fx unit.
        track: Option<TrackId>,
        index: usize,
    },
}

/// One deduped unknown-variant finding (39 §2.2 rule 3: once per document load,
/// never per frame). Dedup key is `(enum_name, tag)`; `count` accumulates every
/// occurrence and `first_site` records the first in traversal order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownVariant {
    /// The Rust enum whose unknown variant this is: `"EffectKind"`, `"GraphOp"`,
    /// `"TransitionKind"`, `"GradeOpKind"`, `"AudioFxKind"`, `"ClipSource"`.
    pub enum_name: &'static str,
    /// The verbatim serialized tag this build did not recognize.
    pub tag: String,
    /// Number of occurrences of this exact `(enum_name, tag)` in the document.
    pub count: usize,
    /// Where the first occurrence was found (traversal order).
    pub first_site: UnknownSite,
}

/// A non-fatal load-time finding: a document was migrated or repaired in a way
/// the caller may want to surface. Distinct from [`LoadError`] (fatal) and from
/// the structured [`unknown_variants`](LoadReport::unknown_variants) /
/// [`dissolved_groups`](LoadReport::dissolved_groups) findings — a `LoadNotice`
/// carries a machine-readable [`code`](LoadNotice::code), a human `message`, and
/// the optional clip it concerns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadNotice {
    /// Machine-readable classifier so a caller can act on the notice kind
    /// without parsing `message`.
    pub code: LoadNoticeCode,
    /// Human-readable one-line description of what was migrated/repaired.
    pub message: String,
    /// The clip the notice concerns, when it names one.
    pub subject: Option<NoticeSubject>,
}

/// The machine-readable classifier for a [`LoadNotice`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadNoticeCode {
    /// A `transition_out` sitting at a hard cut was dropped during load (01 §5):
    /// at a cut the incoming clip's `transition_in` owns the blend, so the
    /// outgoing fade was invalid and is migrated away so the document loads.
    TransitionOutAtCutDropped,
}

/// The clip a [`LoadNotice`] concerns (mirrors [`UnknownSite::Clip`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoticeSubject {
    pub sequence: SequenceId,
    pub track: TrackId,
    pub clip: ClipId,
}

/// The side-band produced by [`finalize_load`]. Empty for a document with no
/// unknown variants.
///
/// This is the interim carrier for 39 §2.2's `Project::UnknownVariantPreserved`
/// warning; wiring it to the real diagnostic taxonomy is a 36 §3 follow-up.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoadReport {
    pub unknown_variants: Vec<UnknownVariant>,
    /// Degenerate groups (empty / singleton-`Normal`) dissolved during load
    /// (35 §3), reported rather than silently swallowed. Each pair names the
    /// sequence the group lived in and the dissolved [`GroupId`].
    pub dissolved_groups: Vec<(SequenceId, GroupId)>,
    /// Markers whose `category` referenced a [`MarkerCategory`] absent from the
    /// project (35 §1.3): flagged for the panel, never remapped.
    ///
    /// [`MarkerCategory`]: super::sequence::MarkerCategory
    pub dangling_categories: Vec<(MarkerScope, MarkerId, MarkerCategoryId)>,
    /// Non-fatal migrations/repairs applied during load (e.g. a `transition_out`
    /// dropped at a cut, 01 §5). Empty for a document that needed none.
    pub notices: Vec<LoadNotice>,
}

/// Accumulates unknown-variant findings, deduping by `(enum_name, tag)` while
/// preserving first-seen order and per-key occurrence counts.
struct UnknownAccum {
    found: Vec<UnknownVariant>,
}

impl UnknownAccum {
    fn new() -> Self {
        UnknownAccum { found: Vec::new() }
    }

    /// Record one occurrence of `tag` for `enum_name` at `site`. The first
    /// occurrence stores `site`; later occurrences only bump `count`.
    fn record(&mut self, enum_name: &'static str, tag: &str, site: UnknownSite) {
        if let Some(v) = self
            .found
            .iter_mut()
            .find(|v| v.enum_name == enum_name && v.tag == tag)
        {
            v.count += 1;
        } else {
            self.found.push(UnknownVariant {
                enum_name,
                tag: tag.to_owned(),
                count: 1,
                first_site: site,
            });
        }
    }
}

/// Scan a finalized project for preserved unknown enum variants (39 §2.2). The
/// result is deduped per `(enum_name, tag)` so a document with the same unknown
/// effect on 50 clips yields ONE finding with `count == 50` — the "diagnose
/// once per load" rule, not once per occurrence.
///
/// Traversal mirrors [`flag_orphaned_property_tracks`] (sequences → video+audio
/// tracks → clips → source / effects / transitions / grade ops, plus track and
/// master-bus fx chains) but ALSO walks `project.graphs`, which the orphan pass
/// skips: `GraphOp::Unknown` is one of the payload-carrying unknown cases and
/// must be diagnosed. Sequences and graphs are visited in id-sorted order so a
/// document with unknowns in more than one sequence reports deterministically.
///
/// A grade op is represented once, under `"GradeOpKind"`: an unknown grade op
/// carries the same tag in both its `kind` and its `params.base`, so scanning
/// the `kind` covers it without double-counting.
pub fn collect_unknown_variants(project: &TimelineProject) -> Vec<UnknownVariant> {
    let mut acc = UnknownAccum::new();

    let mut seq_ids: Vec<SequenceId> = project.sequences.keys().copied().collect();
    seq_ids.sort();
    for seq_id in seq_ids {
        let seq = &project.sequences[&seq_id];
        for track in seq.video_tracks.iter().chain(seq.audio_tracks.iter()) {
            if let Some(audio) = track.audio.as_ref() {
                for (index, unit) in audio.fx_chain.iter().enumerate() {
                    if let Some(t) = unit.kind.unknown_tag() {
                        acc.record(
                            "AudioFxKind",
                            t.as_str(),
                            UnknownSite::AudioFx {
                                track: Some(track.id),
                                index,
                            },
                        );
                    }
                }
            }
            for clip in &track.clips {
                if let Some(tag) = clip.source.unknown_tag() {
                    acc.record(
                        "ClipSource",
                        tag,
                        UnknownSite::Clip {
                            sequence: seq_id,
                            track: track.id,
                            clip: clip.id,
                        },
                    );
                }
                for (index, effect) in clip.effects.iter().enumerate() {
                    if let Some(t) = effect.kind.unknown_tag() {
                        acc.record(
                            "EffectKind",
                            t.as_str(),
                            UnknownSite::Effect {
                                clip: clip.id,
                                index,
                            },
                        );
                    }
                }
                for transition in [clip.transition_in.as_ref(), clip.transition_out.as_ref()]
                    .into_iter()
                    .flatten()
                {
                    if let Some(t) = transition.kind.unknown_tag() {
                        acc.record(
                            "TransitionKind",
                            t.as_str(),
                            UnknownSite::Transition { clip: clip.id },
                        );
                    }
                }
                if let Some(grade) = clip.grade.as_ref() {
                    for op in &grade.ops {
                        if let Some(t) = op.kind.unknown_tag() {
                            acc.record(
                                "GradeOpKind",
                                t.as_str(),
                                UnknownSite::GradeOp {
                                    clip: clip.id,
                                    op: op.id,
                                },
                            );
                        }
                    }
                }
            }
        }
        for (index, unit) in seq.audio_master.fx_chain.iter().enumerate() {
            if let Some(t) = unit.kind.unknown_tag() {
                acc.record(
                    "AudioFxKind",
                    t.as_str(),
                    UnknownSite::AudioFx { track: None, index },
                );
            }
        }
    }

    let mut graph_ids: Vec<GraphId> = project.graphs.keys().copied().collect();
    graph_ids.sort();
    for graph_id in graph_ids {
        let graph = &project.graphs[&graph_id];
        let mut node_ids: Vec<GraphNodeId> = graph.nodes.keys().copied().collect();
        node_ids.sort();
        for node_id in node_ids {
            if let Some(tag) = graph.nodes[&node_id].op.unknown_tag() {
                acc.record(
                    "GraphOp",
                    tag,
                    UnknownSite::GraphNode {
                        graph: graph_id,
                        node: node_id,
                    },
                );
            }
        }
    }

    acc.found
}

#[cfg(test)]
mod unknown_scan_tests {
    use super::*;
    use crate::timeline::{
        Clip, ClipEffect, ClipSource, EffectKind, FrameRate, GraphNode, GraphOp, NodeGraph,
        Sequence, Tick, TimelineProject, Track, TrackKind, UnknownTag,
    };

    /// Three clips sharing ONE unknown effect tag plus one unknown graph op must
    /// yield exactly two findings — counts 3 and 1 — proving "diagnose once per
    /// load" (39 §2.2 rule 3), not once per occurrence, and that `first_site`
    /// points at the first clip in traversal order.
    #[test]
    fn collect_dedups_by_tag_and_accumulates_counts() {
        let mut project = TimelineProject::new();
        let mut seq = Sequence::new("Seq 1", FrameRate::FPS_30, 1920, 1080);
        let mut track = Track::new(TrackKind::Video, "V1");

        let film = EffectKind::Unknown(UnknownTag::intern("film_look"));
        let mut first_clip_id = None;
        for i in 0..3 {
            let mut clip = Clip::new(ClipSource::Adjustment, Tick(i * 1000), Tick(500));
            clip.effects.push(ClipEffect::new(film));
            if first_clip_id.is_none() {
                first_clip_id = Some(clip.id);
            }
            track.clips.push(clip);
        }
        seq.video_tracks.push(track);
        project.insert_sequence(seq);

        let mut graph = NodeGraph::new_project_graph("G1");
        let mut op_obj = serde_json::Map::new();
        op_obj.insert(
            "op".to_string(),
            serde_json::Value::String("caustics".to_string()),
        );
        let node = GraphNode::new(GraphOp::Unknown(op_obj));
        let node_id = node.id;
        graph.nodes.insert(node.id, node);
        project.graphs.insert(graph.id, graph);

        let found = collect_unknown_variants(&project);
        assert_eq!(
            found.len(),
            2,
            "one finding per (enum, tag), not per occurrence"
        );

        let effect = found
            .iter()
            .find(|u| u.enum_name == "EffectKind")
            .expect("the shared unknown effect must be reported");
        assert_eq!(effect.tag, "film_look");
        assert_eq!(
            effect.count, 3,
            "three clips share one unknown effect → count 3"
        );
        assert_eq!(
            effect.first_site,
            UnknownSite::Effect {
                clip: first_clip_id.unwrap(),
                index: 0,
            },
            "first_site names the first clip in traversal order"
        );

        let graph_op = found
            .iter()
            .find(|u| u.enum_name == "GraphOp")
            .expect("the unknown graph op must be reported");
        assert_eq!(graph_op.tag, "caustics");
        assert_eq!(graph_op.count, 1);
        assert!(matches!(
            graph_op.first_site,
            UnknownSite::GraphNode { node, .. } if node == node_id
        ));
    }

    /// A single-clip video sequence whose only clip carries `source`.
    fn project_with_source(source: ClipSource) -> TimelineProject {
        let mut project = TimelineProject::new();
        let mut seq = Sequence::new("Seq 1", FrameRate::FPS_30, 1920, 1080);
        let mut track = Track::new(TrackKind::Video, "V1");
        track.clips.push(Clip::new(source, Tick(0), Tick(500)));
        seq.video_tracks.push(track);
        project.insert_sequence(seq);
        project
    }

    /// 39 §2.2 rule 4: a KNOWN `source` tag whose payload is malformed is
    /// swallowed into `ClipSource::Unknown` by serde's greedy untagged fallback
    /// (retaining the known tag) — `finalize_load` must REJECT it, not load it
    /// inert, so corrupt data never masquerades as a forward-compat variant.
    #[test]
    fn finalize_load_rejects_a_malformed_known_clip_source() {
        let bad: ClipSource =
            serde_json::from_value(serde_json::json!({"source":"solid_color","color":"NOPE"}))
                .expect("greedy untagged fallback swallows the malformed known variant");
        assert!(bad.is_unknown() && bad.unknown_tag() == Some("solid_color"));

        let mut project = project_with_source(bad);
        let err = finalize_load(&mut project)
            .expect_err("a corrupt known clip source must be rejected at load");
        assert_eq!(
            err,
            LoadError::CorruptKnownVariant {
                enum_name: "ClipSource",
                tag: "solid_color".to_string(),
            }
        );
        assert!(err.to_string().contains("solid_color"));
    }

    /// The counterpart: a GENUINE unknown tag (one this build does not know) is
    /// preserved and diagnosed, never rejected.
    #[test]
    fn finalize_load_accepts_a_genuine_unknown_clip_source() {
        let src: ClipSource =
            serde_json::from_value(serde_json::json!({"source":"holo_gen","seed":7})).unwrap();
        let mut project = project_with_source(src);
        let report = finalize_load(&mut project).expect("a genuine unknown source loads");
        assert_eq!(report.unknown_variants.len(), 1);
        assert_eq!(report.unknown_variants[0].enum_name, "ClipSource");
        assert_eq!(report.unknown_variants[0].tag, "holo_gen");
    }

    /// The same greedy-swallow hazard on `GradeOpParams` (an internally-tagged
    /// enum reached through `clip.grade`): a malformed KNOWN grade kind is
    /// rejected.
    #[test]
    fn finalize_load_rejects_a_malformed_known_grade_param() {
        use crate::timeline::{Grade, GradeOp, GradeOpKind, GradeOpParams};
        let bad: GradeOpParams =
            serde_json::from_value(serde_json::json!({"kind":"exposure","stops":"NOPE"}))
                .expect("malformed known grade param degrades to Unknown");
        assert!(bad.is_unknown() && bad.unknown_tag() == Some("exposure"));

        let mut project = project_with_source(ClipSource::Adjustment);
        let clip = &mut project.sequences.values_mut().next().unwrap().video_tracks[0].clips[0];
        let mut op = GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 0.0 },
        );
        op.params.base = bad;
        clip.grade = Some(Grade {
            ops: vec![op],
            bypass: false,
        });

        let err = finalize_load(&mut project).expect_err("corrupt known grade param must reject");
        assert_eq!(
            err,
            LoadError::CorruptKnownVariant {
                enum_name: "GradeOpParams",
                tag: "exposure".to_string(),
            }
        );
    }

    /// The known-tag constants must stay in lockstep with the enums, otherwise
    /// the guard silently stops catching a corrupted variant. Rather than trust a
    /// hand-typed snake_case list, this serializes REAL known variants and
    /// asserts serde's emitted tag is present in the constant — catching the
    /// exact renaming mistakes (e.g. `transform2_d`) the list is prone to.
    #[test]
    fn known_tag_constants_track_serde_output() {
        use crate::timeline::GradeOpParams;
        use crate::Color;

        // Every fieldless GraphOp variant serialized here must appear in the
        // constant; the field-carrying variants are covered by construction.
        let graph_ops = [
            GraphOp::Output,
            GraphOp::ClipIn,
            GraphOp::SolidColor,
            GraphOp::Transform2D,
            GraphOp::Crop,
            GraphOp::Blur,
            GraphOp::Sharpen,
            GraphOp::Glow,
            GraphOp::ChromaKey,
            GraphOp::LumaKey,
            GraphOp::MaskFromMatte,
            GraphOp::Invert,
            GraphOp::ChannelSplit,
            GraphOp::ChannelCombine,
            GraphOp::Switch,
        ];
        for op in graph_ops {
            let tag = serde_json::to_value(&op).unwrap()["op"]
                .as_str()
                .unwrap()
                .to_string();
            assert!(
                KNOWN_GRAPH_OP_TAGS.contains(&tag.as_str()),
                "GraphOp tag {tag:?} missing from KNOWN_GRAPH_OP_TAGS"
            );
        }

        // ClipSource: a couple of representative variants (one fieldless, one
        // with a payload) to pin the snake_case rename against serde.
        for src in [
            ClipSource::Adjustment,
            ClipSource::SolidColor {
                color: Color {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                },
            },
        ] {
            let tag = serde_json::to_value(&src).unwrap()["source"]
                .as_str()
                .unwrap()
                .to_string();
            assert!(
                KNOWN_CLIP_SOURCE_TAGS.contains(&tag.as_str()),
                "missing {tag:?}"
            );
        }

        // GradeOpParams likewise.
        let tag = serde_json::to_value(GradeOpParams::Exposure { stops: 0.0 }).unwrap()["kind"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(KNOWN_GRADE_PARAM_TAGS.contains(&tag.as_str()));

        // Genuine unknown tags are deliberately absent from every constant, so a
        // real forward-compat variant is preserved rather than rejected.
        assert!(!KNOWN_CLIP_SOURCE_TAGS.contains(&"holo_gen"));
        assert!(!KNOWN_GRAPH_OP_TAGS.contains(&"caustics"));
        assert!(!KNOWN_GRADE_PARAM_TAGS.contains(&"bloom"));
    }
}

#[cfg(test)]
mod load_repair_tests {
    use super::*;
    use crate::timeline::anim::{PropPath, PropertyTrack};
    use crate::timeline::{
        AssetKind, Clip, ClipEffect, ClipId, ClipSource, EffectKind, FrameRate, GroupKind,
        GroupNode, Marker, MarkerCategoryId, MediaAsset, Sequence, Tick, TimelineProject, Track,
        TrackKind,
    };

    /// A one-video-track sequence holding a single adjustment clip; returns the
    /// sequence and that clip's id.
    fn seq_with_one_clip() -> (Sequence, ClipId) {
        let mut t = Track::new(TrackKind::Video, "V1");
        let c = Clip::new(ClipSource::Adjustment, Tick(0), Tick(100));
        let cid = c.id;
        t.clips.push(c);
        let mut s = Sequence::new("seq", FrameRate::FPS_30, 1920, 1080);
        s.video_tracks.push(t);
        (s, cid)
    }

    /// An effect carrying one lane whose path cannot resolve for any scope.
    fn blur_with_unknown_lane(path: &str) -> ClipEffect {
        let mut eff = ClipEffect::new(EffectKind::Blur);
        eff.params
            .tracks
            .push(PropertyTrack::new(PropPath::new(path)));
        eff
    }

    // ── Task 1: orphan flagging reaches the new scopes (35 §2) ────────────

    #[test]
    fn catalog_effect_animation_remains_live_after_load() {
        let kind = super::super::effect_kind::EffectKind::Unknown(
            super::super::unknown::UnknownTag::intern("blur.motion"),
        );
        let mut tracks = vec![PropertyTrack::new(super::super::anim::PropPath::new(
            "params.distance",
        ))];
        flag_tracks(&mut tracks, PropTargetKind::Effect(kind));
        assert!(
            !tracks[0].orphaned,
            "known catalog animation must survive loading"
        );
    }

    #[test]
    fn orphaned_paths_are_flagged_at_track_master_and_asset_scope() {
        let mut project = TimelineProject::new();
        let (mut seq, _) = seq_with_one_clip();
        seq.video_tracks[0]
            .effects
            .push(blur_with_unknown_lane("params.nope_track"));
        seq.master_effects
            .push(blur_with_unknown_lane("params.nope_master"));
        project.insert_sequence(seq);

        let mut asset = MediaAsset::from_file(AssetKind::Video, "/tmp/a.mp4");
        asset
            .effects
            .push(blur_with_unknown_lane("params.nope_asset"));
        project.media.insert(asset);

        flag_orphaned_property_tracks(&mut project);

        let seq = project.sequences.values().next().unwrap();
        assert!(
            seq.video_tracks[0].effects[0].params.tracks[0].orphaned,
            "track-scope effect lane must be flagged orphaned"
        );
        assert!(
            seq.master_effects[0].params.tracks[0].orphaned,
            "master-scope effect lane must be flagged orphaned"
        );
        let asset = project.media.assets.values().next().unwrap();
        assert!(
            asset.effects[0].params.tracks[0].orphaned,
            "asset-scope effect lane must be flagged orphaned"
        );
    }

    // ── Task 3: load-time group repair + dangling-category flagging (35 §3/§1.3) ─

    #[test]
    fn load_dissolves_an_empty_group_and_reports_it() {
        let (mut seq, _) = seq_with_one_clip();
        let g = GroupNode::new(GroupKind::Normal);
        let gid = g.id;
        let sid = seq.id;
        seq.groups.insert(gid, g); // no members, no children → degenerate
        let mut project = TimelineProject::new();
        project.insert_sequence(seq);

        let report = finalize_load(&mut project).expect("an empty group is repaired, not rejected");
        assert!(
            report.dissolved_groups.contains(&(sid, gid)),
            "the dissolved empty group must be reported, not silently swallowed"
        );
        assert!(
            !project.sequences[&sid].groups.contains_key(&gid),
            "the degenerate group must be gone after load"
        );
    }

    #[test]
    fn load_rejects_a_group_cycle() {
        let (mut seq, cid) = seq_with_one_clip();
        // A second clip so neither group is a singleton `Normal` (which would be
        // dissolved instead of surfacing the cycle).
        let c2 = Clip::new(ClipSource::Adjustment, Tick(100), Tick(100));
        seq.video_tracks[0].clips.push(c2);
        let mut a = GroupNode::new(GroupKind::Normal);
        let mut b = GroupNode::new(GroupKind::Normal);
        let (aid, bid) = (a.id, b.id);
        a.parent = Some(bid);
        b.parent = Some(aid); // a → b → a
        seq.groups.insert(aid, a);
        seq.groups.insert(bid, b);
        for c in &mut seq.video_tracks[0].clips {
            c.group = if c.id == cid { Some(aid) } else { Some(bid) };
        }
        let mut project = TimelineProject::new();
        project.insert_sequence(seq);

        let err = finalize_load(&mut project).expect_err("a group cycle must be rejected at load");
        assert!(matches!(
            err,
            LoadError::Validation(ValidationError::GroupCycle { .. })
        ));
    }

    #[test]
    fn load_rejects_an_unknown_group_ref() {
        let (mut seq, cid) = seq_with_one_clip();
        seq.video_tracks[0]
            .clips
            .iter_mut()
            .find(|c| c.id == cid)
            .unwrap()
            .group = Some(GroupId::new()); // names a group absent from the sequence
        let mut project = TimelineProject::new();
        project.insert_sequence(seq);

        let err =
            finalize_load(&mut project).expect_err("an unknown group ref must be rejected at load");
        assert!(matches!(
            err,
            LoadError::Validation(ValidationError::UnknownGroup { .. })
        ));
    }

    #[test]
    fn load_flags_a_dangling_marker_category_without_remapping_it() {
        let (mut seq, _) = seq_with_one_clip();
        let missing = MarkerCategoryId::new();
        let mut m = Marker::new(Tick(10), "m");
        m.category = Some(missing);
        let mid = m.id;
        seq.markers.push(m);
        let sid = seq.id;
        let mut project = TimelineProject::new();
        // Deliberately leave `project.marker_categories` empty so `missing` dangles.
        project.insert_sequence(seq);

        let report =
            finalize_load(&mut project).expect("a dangling category is flagged, not fatal");
        assert!(
            report
                .dangling_categories
                .contains(&(MarkerScope::Sequence(sid), mid, missing)),
            "the dangling category must be reported at its marker"
        );
        // Never silently remapped or cleared (35 §1.3).
        assert_eq!(
            project.sequences[&sid].markers[0].category,
            Some(missing),
            "the marker's category must be left untouched"
        );
    }

    // ── Part 1: typed load notices + transition_out-at-a-cut migration ────────

    #[test]
    fn finalize_load_returns_empty_notices_for_clean_project() {
        let (seq, _) = seq_with_one_clip();
        let mut project = TimelineProject::new();
        project.insert_sequence(seq);

        let report = finalize_load(&mut project).expect("a clean project loads");
        assert!(
            report.notices.is_empty(),
            "a clean project must produce no load notices"
        );
    }

    #[test]
    fn finalize_load_migrates_transition_out_at_a_cut() {
        use crate::timeline::{FrameRate, Transition, TransitionKind};
        // Two adjacent clips — [0,100) then [100,100), no gap. The first carries
        // a `transition_out`, which is invalid at a hard cut: load must DROP it
        // and record a notice rather than reject the whole document.
        let mut t = Track::new(TrackKind::Video, "V1");
        let mut a = Clip::new(ClipSource::Adjustment, Tick(0), Tick(100));
        a.transition_out = Some(Transition::new(TransitionKind::CrossDissolve, Tick(10)));
        let aid = a.id;
        let tid = t.id;
        t.clips.push(a);
        t.clips
            .push(Clip::new(ClipSource::Adjustment, Tick(100), Tick(100)));
        let mut seq = Sequence::new("seq", FrameRate::FPS_30, 1920, 1080);
        let sid = seq.id;
        seq.video_tracks.push(t);
        let mut project = TimelineProject::new();
        project.insert_sequence(seq);

        let report = finalize_load(&mut project)
            .expect("a transition_out at a cut is migrated away, not rejected");

        // The offending transition is gone (the second clip is untouched)...
        let clip = &project.sequences[&sid].video_tracks[0].clips[0];
        assert_eq!(clip.id, aid);
        assert!(
            clip.transition_out.is_none(),
            "the transition_out at the cut must be dropped on load"
        );
        // ...and the drop is reported as a typed notice naming the clip.
        assert_eq!(report.notices.len(), 1, "exactly one drop must be reported");
        assert_eq!(
            report.notices[0].code,
            LoadNoticeCode::TransitionOutAtCutDropped
        );
        assert_eq!(
            report.notices[0].subject,
            Some(NoticeSubject {
                sequence: sid,
                track: tid,
                clip: aid,
            })
        );
    }
}
