//! Args for the video-domain MCP tool surface (10-mcp-tools.md §3, P2 slice:
//! timeline-EDIT tools only — sequence/track/clip/effects/keyframes/media).
//!
//! Engine-backed tools (probe/proxy/playback/render/export/captions/tts/grade
//! scopes) are P3+ and have no args here.
//!
//! Time-valued fields follow design rule 3 (10 §1.3): every timeline position
//! exposes `*_ticks`/`*_tc`/`*_seconds`, precedence ticks > tc > seconds,
//! resolved by the shared `resolve_tick` helper in `handlers/video.rs`.
//!
//! Clip/track-scoped tools address their target by id alone (`clip_id`,
//! `track_id`) — matching §3's tool tables and the rest of the MCP surface's
//! id-only addressing (`update_node(node_id)` needs no `layer_id`). The
//! handler resolves the owning sequence/track by scanning the project
//! (`handlers/video.rs::locate_clip`/`locate_track`) before calling the
//! `timeline/ops.rs` fn, which does take explicit ids.

use photonic_core::timeline::{
    AssetId, BinId, ChannelMap, ClipId, ClipTransform, CueId, FadeShape, FrameRate, GraphId,
    GraphNodeId, Interp, MarkerCategoryId, MarkerGlyph, MarkerId, PropValue, SequenceFormat,
    SequenceId, TrackId, TrackKind, TransitionKind, TransitionParams,
};
use serde::Deserialize;

// ─── Shared value types ─────────────────────────────────────────────────────

/// A clip source (01 §5 `ClipSource`), as supplied over the wire.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClipSourceArg {
    Asset {
        asset_id: AssetId,
    },
    Vector {
        asset_id: AssetId,
    },
    NestedSequence {
        sequence_id: SequenceId,
    },
    /// `#rrggbb` or `#rrggbbaa`.
    SolidColor {
        color: String,
    },
    Adjustment,
}

/// A generic keyframe/animation target (01 §6). P2 scope: clip transform and
/// clip-effect params only — grade/audio/graph-node targets land with their
/// respective domains (P3+).
#[derive(Debug, Deserialize)]
#[serde(tag = "target", rename_all = "snake_case")]
pub enum AnimTargetArg {
    ClipTransform {
        clip_id: ClipId,
    },
    ClipEffect {
        clip_id: ClipId,
        effect_index: usize,
    },
}

impl AnimTargetArg {
    pub fn clip_id(&self) -> ClipId {
        match self {
            AnimTargetArg::ClipTransform { clip_id }
            | AnimTargetArg::ClipEffect { clip_id, .. } => *clip_id,
        }
    }
}

/// Which mode `set_sequence_format` operates in.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FormatOpKind {
    Add,
    Update,
    Remove,
}

/// Which edge a trim op targets.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipEdge {
    In,
    Out,
}

/// `{num, den}` — exact rational speed (01 §5.1).
#[derive(Debug, Deserialize)]
pub struct RatioArg {
    pub num: i32,
    pub den: u32,
}

// ─── Sequence (10 §3.2) ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CreateSequenceArgs {
    pub name: String,
    /// `{"num": 30, "den": 1}` — see [`FrameRate`].
    pub frame_rate: FrameRate,
    /// At least one required (CAP-012).
    pub formats: Vec<SequenceFormat>,
}

#[derive(Debug, Deserialize)]
pub struct DeleteSequenceArgs {
    pub sequence_id: SequenceId,
}

#[derive(Debug, Deserialize, Default)]
pub struct ListSequencesArgs {}

#[derive(Debug, Deserialize)]
pub struct SetActiveSequenceArgs {
    /// `null`/omitted clears the active sequence.
    #[serde(default)]
    pub sequence_id: Option<SequenceId>,
}

#[derive(Debug, Deserialize)]
pub struct SetSequenceFormatArgs {
    pub sequence_id: SequenceId,
    pub op: FormatOpKind,
    /// Required for `add`/`update`.
    #[serde(default)]
    pub format: Option<SequenceFormat>,
    /// Required for `update`/`remove`.
    #[serde(default)]
    pub format_index: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct SetActiveFormatArgs {
    pub sequence_id: SequenceId,
    pub format_index: usize,
}

/// `null`/omitted `range` clears the sequence's work range.
#[derive(Debug, Deserialize)]
pub struct SetWorkRangeArgs {
    pub sequence_id: SequenceId,
    #[serde(default)]
    pub range: Option<WorkRangeArg>,
}

#[derive(Debug, Deserialize)]
pub struct WorkRangeArg {
    #[serde(default)]
    pub start_ticks: Option<i64>,
    #[serde(default)]
    pub start_tc: Option<String>,
    #[serde(default)]
    pub start_seconds: Option<f64>,
    #[serde(default)]
    pub end_ticks: Option<i64>,
    #[serde(default)]
    pub end_tc: Option<String>,
    #[serde(default)]
    pub end_seconds: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct AddMarkerArgs {
    pub sequence_id: SequenceId,
    #[serde(default)]
    pub at_ticks: Option<i64>,
    #[serde(default)]
    pub at_tc: Option<String>,
    #[serde(default)]
    pub at_seconds: Option<f64>,
    #[serde(default)]
    pub name: Option<String>,
    /// `#rrggbb` or `#rrggbbaa`.
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    /// K-A2: `> 0` makes this a RANGED marker (`duration` on the model). A
    /// ranged marker is what `export_per_marker` (K-F2) fans out over, so
    /// omitting all three leaves a point marker that multi-export skips.
    #[serde(default)]
    pub duration_ticks: Option<i64>,
    #[serde(default)]
    pub duration_seconds: Option<f64>,
    /// End position, as an alternative to a duration (`duration = end - at`).
    #[serde(default)]
    pub end_tc: Option<String>,
    /// K-A2: a `MarkerCategory` id from `list_marker_categories`.
    #[serde(default)]
    pub category_id: Option<MarkerCategoryId>,
}

#[derive(Debug, Deserialize)]
pub struct RemoveMarkerArgs {
    pub marker_id: MarkerId,
}

#[derive(Debug, Deserialize)]
pub struct ListMarkersArgs {
    pub sequence_id: SequenceId,
}

// ─── Marker depth: editing, categories, clip scope (26 K-A2) ────────────────

/// Universal marker editor. Only supplied fields change; everything else keeps
/// its current value. `clip_id` selects the scope: omitted = a sequence marker,
/// present = that clip's own marker list.
#[derive(Debug, Deserialize)]
pub struct SetMarkerArgs {
    pub marker_id: MarkerId,
    /// When set, `marker_id` is looked up on this clip instead of a sequence.
    #[serde(default)]
    pub clip_id: Option<ClipId>,
    #[serde(default)]
    pub at_ticks: Option<i64>,
    #[serde(default)]
    pub at_tc: Option<String>,
    #[serde(default)]
    pub at_seconds: Option<f64>,
    #[serde(default)]
    pub duration_ticks: Option<i64>,
    #[serde(default)]
    pub duration_seconds: Option<f64>,
    #[serde(default)]
    pub end_tc: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    /// `#rrggbb` / `#rrggbbaa`, or `""` to clear the per-marker override and
    /// fall back to the category colour.
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub category_id: Option<MarkerCategoryId>,
    /// Set true to clear the category (`category_id` is then ignored).
    #[serde(default)]
    pub clear_category: bool,
}

#[derive(Debug, Deserialize)]
pub struct AddClipMarkerArgs {
    pub clip_id: ClipId,
    /// CLIP-RELATIVE position (0 = the clip's first frame), not a sequence tick.
    #[serde(default)]
    pub at_ticks: Option<i64>,
    #[serde(default)]
    pub at_seconds: Option<f64>,
    #[serde(default)]
    pub at_tc: Option<String>,
    #[serde(default)]
    pub duration_ticks: Option<i64>,
    #[serde(default)]
    pub duration_seconds: Option<f64>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub category_id: Option<MarkerCategoryId>,
}

#[derive(Debug, Deserialize)]
pub struct RemoveClipMarkerArgs {
    pub clip_id: ClipId,
    pub marker_id: MarkerId,
}

#[derive(Debug, Deserialize)]
pub struct ListClipMarkersArgs {
    pub clip_id: ClipId,
}

#[derive(Debug, Deserialize)]
pub struct ListMarkerCategoriesArgs {}

#[derive(Debug, Deserialize)]
pub struct AddMarkerCategoryArgs {
    pub name: String,
    /// `#rrggbb` or `#rrggbbaa`.
    pub color: String,
    #[serde(default)]
    pub glyph: Option<MarkerGlyph>,
}

#[derive(Debug, Deserialize)]
pub struct SeedMarkerCategoriesArgs {}

#[derive(Debug, Deserialize)]
pub struct UpdateMarkerCategoryArgs {
    pub category_id: MarkerCategoryId,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub glyph: Option<MarkerGlyph>,
}

#[derive(Debug, Deserialize)]
pub struct RemoveMarkerCategoryArgs {
    pub category_id: MarkerCategoryId,
    /// Where markers that referenced the deleted category go. Omitted = their
    /// category is cleared. Either way the move is recorded per marker and is
    /// undone with the delete — a marker is never left pointing at a category
    /// that no longer exists, and is never silently remapped (35 §1.3).
    #[serde(default)]
    pub reassign_to: Option<MarkerCategoryId>,
}

// ─── Track (10 §3.3) ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct AddTrackArgs {
    pub sequence_id: SequenceId,
    pub kind: TrackKind,
    #[serde(default)]
    pub name: Option<String>,
    /// Insertion index within the track's lane; defaults to the end.
    #[serde(default)]
    pub index: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct RemoveTrackArgs {
    pub track_id: TrackId,
}

/// Universal track setter (mirrors `TrackSettings`); every field optional,
/// only supplied fields change.
#[derive(Debug, Deserialize)]
pub struct SetTrackPropArgs {
    pub track_id: TrackId,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub locked: Option<bool>,
    #[serde(default)]
    pub height_px: Option<f32>,
}

/// Args shape matches `ops::reorder_track` (single track → target index
/// within its lane), not the spec table's illustrative full
/// `old_order`/`new_order` permutation — `ops.rs` has no permutation-setter
/// fn; this is functionally equivalent (design rule 1: use the existing op
/// as-is).
#[derive(Debug, Deserialize)]
pub struct ReorderTrackArgs {
    pub track_id: TrackId,
    pub new_index: usize,
}

// ─── Clip edit ops (10 §3.4) ────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct InsertClipArgs {
    pub track_id: TrackId,
    #[serde(default)]
    pub name: Option<String>,
    /// Position in the sequence. Precedence: at_ticks > at_tc > at_seconds (§1 rule 3).
    #[serde(default)]
    pub start_ticks: Option<i64>,
    #[serde(default)]
    pub start_tc: Option<String>,
    #[serde(default)]
    pub start_seconds: Option<f64>,
    pub source: ClipSourceArg,
    #[serde(default)]
    pub source_in_ticks: Option<i64>,
    #[serde(default)]
    pub source_in_tc: Option<String>,
    #[serde(default)]
    pub source_in_seconds: Option<f64>,
    /// Duration always exact ticks — no dual-unit ambiguity for a length
    /// agents typically compute from probe data (§5).
    pub duration_ticks: i64,
}

#[derive(Debug, Deserialize)]
pub struct MoveClipArgs {
    pub clip_id: ClipId,
    #[serde(default)]
    pub new_start_ticks: Option<i64>,
    #[serde(default)]
    pub new_start_tc: Option<String>,
    #[serde(default)]
    pub new_start_seconds: Option<f64>,
    /// Cross-track move (destination must be the same `TrackKind`, routes
    /// through `ops::move_clip_to_track`). Omit for a same-track move.
    #[serde(default)]
    pub new_track_id: Option<TrackId>,
}

/// Args for `ripple_edit` — trims `edge` to `current_edge_position +
/// delta_ticks` and ripples every later clip on the track to close/open the
/// gap (`ops::ripple_trim`). `edge` uses the same in/out vocabulary as
/// `trim_clip` (in = clip's in-point/`ClipEdge::Start`, out = clip's
/// out-point/`ClipEdge::End`).
/// Args for `move_clips` — shift several clips by one shared time delta and
/// optional same-kind track offset in a single undo step (04 §2.6 multi-select,
/// 210 §5). `delta_ticks` preserves relative spacing; `track_delta` is a lane
/// index offset within the sequence's video or audio list (default 0).
#[derive(Debug, Deserialize)]
pub struct MoveClipsArgs {
    pub clip_ids: Vec<ClipId>,
    pub delta_ticks: i64,
    /// Same-kind lane-index offset applied to every clip (0 = stay on track).
    #[serde(default)]
    pub track_delta: i32,
}

#[derive(Debug, Deserialize)]
pub struct RippleEditArgs {
    pub clip_id: ClipId,
    pub edge: ClipEdge,
    pub delta_ticks: i64,
}

#[derive(Debug, Deserialize)]
pub struct TrimClipArgs {
    pub clip_id: ClipId,
    pub edge: ClipEdge,
    #[serde(default)]
    pub new_ticks: Option<i64>,
    #[serde(default)]
    pub new_tc: Option<String>,
    #[serde(default)]
    pub new_seconds: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct SplitClipArgs {
    pub clip_id: ClipId,
    #[serde(default)]
    pub at_ticks: Option<i64>,
    #[serde(default)]
    pub at_tc: Option<String>,
    #[serde(default)]
    pub at_seconds: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct RemoveClipArgs {
    pub clip_id: ClipId,
    /// Shift every later clip on the track left by the removed clip's
    /// duration (`ops::ripple_delete`).
    #[serde(default)]
    pub ripple: bool,
}

#[derive(Debug, Deserialize)]
pub struct RollEditArgs {
    pub clip_id_a: ClipId,
    pub clip_id_b: ClipId,
    /// Delta in ticks applied to the shared edge (positive = later).
    pub delta_ticks: i64,
}

#[derive(Debug, Deserialize)]
pub struct SlipClipArgs {
    pub clip_id: ClipId,
    /// Delta in ticks applied to `source_in` (position on the timeline is
    /// unchanged).
    pub delta_ticks: i64,
}

#[derive(Debug, Deserialize)]
pub struct SlideClipArgs {
    pub clip_id: ClipId,
    pub delta_ticks: i64,
}

// ─── 3/4-point editing (16 §2, CAP-019 MCP parity) ─────────────────────────

/// Args for `insert_edit` (16 §2, Premiere `,`): open a gap of `source`'s
/// duration at `at` on `track_id` — splitting any clip straddling `at` and
/// rippling everything at/after `at` on that track right — then place
/// `source` in the gap. Same source/time-resolution shape as `insert_clip`
/// (`at_*` plays the role `start_*` plays there).
#[derive(Debug, Deserialize)]
pub struct InsertEditArgs {
    pub track_id: TrackId,
    #[serde(default)]
    pub name: Option<String>,
    /// Insertion point. Precedence: at_ticks > at_tc > at_seconds (§1 rule 3).
    #[serde(default)]
    pub at_ticks: Option<i64>,
    #[serde(default)]
    pub at_tc: Option<String>,
    #[serde(default)]
    pub at_seconds: Option<f64>,
    pub source: ClipSourceArg,
    #[serde(default)]
    pub source_in_ticks: Option<i64>,
    #[serde(default)]
    pub source_in_tc: Option<String>,
    #[serde(default)]
    pub source_in_seconds: Option<f64>,
    /// Duration always exact ticks — no dual-unit ambiguity (mirrors
    /// `insert_clip`).
    pub duration_ticks: i64,
}

/// Args for `overwrite_edit` (16 §2, Premiere `.`) — identical shape to
/// [`InsertEditArgs`], but `source` replaces whatever it covers on
/// `track_id` at `at` with NO ripple (timeline duration unchanged unless
/// `source` extends past the old end).
#[derive(Debug, Deserialize)]
pub struct OverwriteEditArgs {
    pub track_id: TrackId,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub at_ticks: Option<i64>,
    #[serde(default)]
    pub at_tc: Option<String>,
    #[serde(default)]
    pub at_seconds: Option<f64>,
    pub source: ClipSourceArg,
    #[serde(default)]
    pub source_in_ticks: Option<i64>,
    #[serde(default)]
    pub source_in_tc: Option<String>,
    #[serde(default)]
    pub source_in_seconds: Option<f64>,
    pub duration_ticks: i64,
}

/// Args for `lift_edit` (16 §2, Premiere `;`): remove clip content in
/// `range` on `track_id`, leaving a gap (no ripple). `range` reuses
/// [`WorkRangeArg`]'s shape (`start_*`/`end_*`, ticks > tc > seconds
/// precedence per bound); both bounds are required in practice — a missing
/// one surfaces as `resolve_tick`'s standard "missing time value" error.
#[derive(Debug, Deserialize)]
pub struct LiftEditArgs {
    pub track_id: TrackId,
    pub range: WorkRangeArg,
}

/// Args for `extract_edit` (16 §2, Premiere `'`) — same shape as
/// [`LiftEditArgs`], but everything after `range` on `track_id` ripples left
/// to close the gap (generalizes `remove_clip`'s `ripple` flag to an
/// arbitrary range instead of one clip).
#[derive(Debug, Deserialize)]
pub struct ExtractEditArgs {
    pub track_id: TrackId,
    pub range: WorkRangeArg,
}

// ─── NLE parity round-2 (17-nle-parity-round2.md, G21 CAP-019 MCP parity) ──

/// Args for `replace_clip_source` (G-5, Premiere "Replace With Clip"): swap
/// `clip_id`'s source in place — `start`/`duration`/effects/transitions/grade
/// untouched (`ops::replace_clip_source`). A shorter new source is held to the
/// slot (sampled from `new_source_in` for the slot's length by the engine).
#[derive(Debug, Deserialize)]
pub struct ReplaceClipSourceArgs {
    pub clip_id: ClipId,
    pub new_source: ClipSourceArg,
    /// Offset into the new source to sample from. Omit to keep the clip's
    /// existing `source_in`. Precedence: ticks > tc > seconds (§1 rule 3).
    #[serde(default)]
    pub new_source_in_ticks: Option<i64>,
    #[serde(default)]
    pub new_source_in_tc: Option<String>,
    #[serde(default)]
    pub new_source_in_seconds: Option<f64>,
}

/// Args for `add_edit_all_tracks` (G-1, Premiere Ctrl+Shift+K): split every
/// unlocked track's clip that `at` sits strictly inside, across the whole
/// sequence, as ONE undo step.
#[derive(Debug, Deserialize)]
pub struct AddEditAllTracksArgs {
    pub sequence_id: SequenceId,
    #[serde(default)]
    pub at_ticks: Option<i64>,
    #[serde(default)]
    pub at_tc: Option<String>,
    #[serde(default)]
    pub at_seconds: Option<f64>,
}

/// Args for `close_gap` (G-1): close the gap containing `at` — on just
/// `track_id` when supplied, or on every unlocked track in the sequence (one
/// undo step either way) when omitted.
#[derive(Debug, Deserialize)]
pub struct CloseGapArgs {
    pub sequence_id: SequenceId,
    /// Restrict to one track. Omit to close the gap at `at` on every
    /// unlocked track in the sequence.
    #[serde(default)]
    pub track_id: Option<TrackId>,
    #[serde(default)]
    pub at_ticks: Option<i64>,
    #[serde(default)]
    pub at_tc: Option<String>,
    #[serde(default)]
    pub at_seconds: Option<f64>,
}

/// K-A3 Insert Space: open `amount` of empty timeline at `at` across every
/// unlocked track (shifts later clips right). One undo step.
#[derive(Debug, Deserialize)]
pub struct InsertSpaceArgs {
    pub sequence_id: SequenceId,
    #[serde(default)]
    pub at_ticks: Option<i64>,
    #[serde(default)]
    pub at_tc: Option<String>,
    #[serde(default)]
    pub at_seconds: Option<f64>,
    /// Gap width. Prefer `amount_ticks`; else `amount_seconds` (default 1.0).
    #[serde(default)]
    pub amount_ticks: Option<i64>,
    #[serde(default)]
    pub amount_seconds: Option<f64>,
}

/// K-A3 Remove Space: close up to `amount` of pure gap at `at` across unlocked
/// tracks. Refuses when a clip covers `at` or the shared free gap is shorter.
#[derive(Debug, Deserialize)]
pub struct RemoveSpaceArgs {
    pub sequence_id: SequenceId,
    #[serde(default)]
    pub at_ticks: Option<i64>,
    #[serde(default)]
    pub at_tc: Option<String>,
    #[serde(default)]
    pub at_seconds: Option<f64>,
    #[serde(default)]
    pub amount_ticks: Option<i64>,
    #[serde(default)]
    pub amount_seconds: Option<f64>,
}

/// K-A3 Remove All Spaces After / Remove All Clips After: sequence + point.
#[derive(Debug, Deserialize)]
pub struct SpaceAfterArgs {
    pub sequence_id: SequenceId,
    #[serde(default)]
    pub at_ticks: Option<i64>,
    #[serde(default)]
    pub at_tc: Option<String>,
    #[serde(default)]
    pub at_seconds: Option<f64>,
}

/// Args for `match_frame` (G-3, Premiere F): from `clip_id`, compute the
/// source-media tick that lines up with timeline position `at` (which must
/// fall within the clip's span). Read-only — does not mutate the project or
/// arm anything; the caller feeds the returned tick into
/// `replace_clip_source`/`insert_edit`/`overwrite_edit`.
#[derive(Debug, Deserialize)]
pub struct MatchFrameArgs {
    pub clip_id: ClipId,
    #[serde(default)]
    pub at_ticks: Option<i64>,
    #[serde(default)]
    pub at_tc: Option<String>,
    #[serde(default)]
    pub at_seconds: Option<f64>,
}

/// Args for `insert_adjustment_clip` (G-7): create a `ClipSource::Adjustment`
/// clip spanning `[start, start+duration)` on `track_id` — no media, its
/// effect stack/grade composites over every lower track beneath its span
/// (engine side; `ops::add_adjustment_clip`).
#[derive(Debug, Deserialize)]
pub struct InsertAdjustmentClipArgs {
    pub track_id: TrackId,
    #[serde(default)]
    pub start_ticks: Option<i64>,
    #[serde(default)]
    pub start_tc: Option<String>,
    #[serde(default)]
    pub start_seconds: Option<f64>,
    pub duration_ticks: i64,
}

/// Args for `insert_text_clip` (G-12): create a `ClipSource::Text` title clip
/// spanning `[start, start+duration)` on `track_id` (`ops::add_text_clip`).
/// `style` is a partial [`CaptionStyleArg`] patch over `CaptionStyle::default()`
/// — reuses the caption styling vocabulary (font/fill/position/etc.).
#[derive(Debug, Deserialize)]
pub struct InsertTextClipArgs {
    pub track_id: TrackId,
    pub text: String,
    #[serde(default)]
    pub style: Option<CaptionStyleArg>,
    #[serde(default)]
    pub start_ticks: Option<i64>,
    #[serde(default)]
    pub start_tc: Option<String>,
    #[serde(default)]
    pub start_seconds: Option<f64>,
    pub duration_ticks: i64,
}

// ─── Clip properties (10 §3.5) ──────────────────────────────────────────────

/// Universal clip setter — every field optional, only supplied fields
/// change (mirrors `update_node`'s shape, utility.rs:12). `speed` and
/// `transition_*` have dedicated tools (`set_clip_speed`/`set_transition`) —
/// not repeated here.
#[derive(Debug, Deserialize)]
pub struct SetClipPropArgs {
    pub clip_id: ClipId,
    #[serde(default)]
    pub name: Option<String>,
    /// Full base transform replace (pos/scale/rotation/anchor/opacity). Omitted
    /// `anchor_space` defaults to the v4 `center_offset` convention.
    #[serde(default)]
    pub transform: Option<ClipTransform>,
    /// Per-`SequenceFormat` static override.
    #[serde(default)]
    pub reframe: Option<ReframeArg>,
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Organizational color label — a swatch-palette index (14 §M-1,
    /// CAP-019 MCP parity). Presence of the field is the change signal:
    /// `{"color_label": null}` clears the label, `{"color_label": 3}` sets
    /// it, and omitting the field entirely leaves it unchanged. A plain
    /// `Option<Option<u8>>` with `#[serde(default)]` can't make that
    /// distinction on its own — `Option<T>`'s null-handling swallows the
    /// JSON `null` before the outer `Option` learns the field was even
    /// present — so this field routes through [`deserialize_present`]
    /// (`reframe` above sidesteps the same trap differently, by nesting the
    /// clear signal one level down inside a required object).
    #[serde(default, deserialize_with = "deserialize_present")]
    pub color_label: Option<Option<u8>>,
}

#[derive(Debug, Deserialize)]
pub struct ReframeArg {
    pub format_index: usize,
    /// `null` clears the override for this format index.
    pub transform: Option<ClipTransform>,
}

/// Deserializes into `Some(_)` whenever the field is present in the JSON
/// (including an explicit `null`, which resolves to `Some(None)`), and is
/// simply never invoked when the field is absent — `#[serde(default)]`
/// supplies `None` for that case instead. Use as
/// `#[serde(default, deserialize_with = "deserialize_present")]` on an
/// `Option<Option<T>>` field that needs to distinguish "omitted" from
/// "explicitly cleared".
fn deserialize_present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    T::deserialize(deserializer).map(Some)
}

// ─── Clip organization: color label & linking (14 §M-1/M-2, CAP-019 MCP
// parity) ────────────────────────────────────────────────────────────────

/// Args for `link_clips` (14 §M-2): link two clips (e.g. a split A/V pair)
/// into the same link group, reusing whichever clip's group already exists
/// or minting a fresh one (`ops::link_clips`). One undo step.
#[derive(Debug, Deserialize)]
pub struct LinkClipsArgs {
    pub clip_id_a: ClipId,
    pub clip_id_b: ClipId,
}

/// Args for `unlink_clips`: remove `clip_id` from its link group
/// (`ops::unlink_clip`) — a no-op edit if it wasn't linked. Only the named
/// clip leaves the group; its former partners stay linked to each other.
#[derive(Debug, Deserialize)]
pub struct UnlinkClipsArgs {
    pub clip_id: ClipId,
}

/// Set a clip's speed to either a constant ratio or a keyframed ramp (G-11) —
/// supply exactly one of `ratio`/`keys`. `keys` mirrors `SpeedMap::Keyframed`
/// (clip.rs): control points at clip-relative timeline ticks, piecewise-constant
/// between them (each key's ratio holds until the next).
#[derive(Debug, Deserialize)]
pub struct SetClipSpeedArgs {
    pub clip_id: ClipId,
    /// Constant-speed ratio. Mutually exclusive with `keys`.
    #[serde(default)]
    pub ratio: Option<RatioArg>,
    /// Keyframed variable-speed ramp control points, in clip-relative-tick
    /// order (not required to be pre-sorted — `ops::set_clip_prop` doesn't
    /// care, but `SpeedMap::source_delta`'s integration assumes ascending
    /// `at`). Mutually exclusive with `ratio`.
    #[serde(default)]
    pub keys: Option<Vec<SpeedKeyArg>>,
}

/// K-B14 Freeze frame: hold the source frame at clip-relative `at_*` for the
/// clip's whole duration (zero-rate `SpeedMap`). `at_*` uses the same
/// precedence as other time args (ticks > tc > seconds); omit for the
/// clip's first frame.
#[derive(Debug, Deserialize)]
pub struct FreezeFrameArgs {
    pub clip_id: ClipId,
    #[serde(default)]
    pub at_ticks: Option<i64>,
    #[serde(default)]
    pub at_tc: Option<String>,
    #[serde(default)]
    pub at_seconds: Option<f64>,
}

/// One control point of a [`SetClipSpeedArgs::keys`] ramp — clip-relative
/// position (`at_*`, §1 rule 3 precedence) + the exact-rational ratio that
/// takes effect there. `interp` controls the segment leaving this key and
/// defaults to `hold` for backwards-compatible stepped ramps.
#[derive(Debug, Deserialize)]
pub struct SpeedKeyArg {
    #[serde(default)]
    pub at_ticks: Option<i64>,
    #[serde(default)]
    pub at_tc: Option<String>,
    #[serde(default)]
    pub at_seconds: Option<f64>,
    pub ratio: RatioArg,
    #[serde(default)]
    pub interp: Option<Interp>,
}

#[derive(Debug, Deserialize)]
pub struct SetTransitionArgs {
    pub clip_id: ClipId,
    pub edge: ClipEdge,
    /// `null` removes the transition on that edge.
    #[serde(default)]
    pub transition: Option<TransitionArg>,
}

#[derive(Debug, Deserialize)]
pub struct TransitionArg {
    pub kind: TransitionKind,
    pub duration_ticks: i64,
    #[serde(default)]
    pub params: TransitionParams,
}

#[derive(Debug, Deserialize, Default)]
pub struct ListClipsArgs {
    #[serde(default)]
    pub sequence_id: Option<SequenceId>,
    #[serde(default)]
    pub track_id: Option<TrackId>,
    /// Optional `[start_ticks, end_ticks)` filter.
    #[serde(default)]
    pub range_start_ticks: Option<i64>,
    #[serde(default)]
    pub range_end_ticks: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct GetClipArgs {
    pub clip_id: ClipId,
}

// ─── Effects (10 §3.6) ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct AddEffectArgs {
    pub clip_id: ClipId,
    pub kind: photonic_core::timeline::EffectKind,
    #[serde(default)]
    pub index: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct RemoveEffectArgs {
    pub clip_id: ClipId,
    pub effect_index: usize,
}

#[derive(Debug, Deserialize)]
pub struct ReorderEffectsArgs {
    pub clip_id: ClipId,
    pub new_order: Vec<usize>,
}

/// Sets one `PropPath` under `effects[i].params` (static base value, not a
/// keyframe — use `set_keyframe` for animated params); `path == "enabled"`
/// toggles the effect itself instead of a param.
#[derive(Debug, Deserialize)]
pub struct SetEffectParamArgs {
    pub clip_id: ClipId,
    pub effect_index: usize,
    pub path: String,
    pub value: PropValue,
}

/// K-C2: set free-form tags on an asset (resolved into the TagId registry).
#[derive(Debug, Deserialize)]
pub struct SetAssetTagsArgs {
    pub asset_id: AssetId,
    /// Replacement tag names (deduped). Empty clears.
    #[serde(default)]
    pub tags: Vec<String>,
}

/// K-A8: create a subclip (zone-bounded pool entry) of a parent media asset.
#[derive(Debug, Deserialize)]
pub struct CreateSubclipArgs {
    pub parent_asset_id: AssetId,
    /// Source-range start (ticks on parent media). Prefer ticks over seconds.
    #[serde(default)]
    pub in_ticks: Option<i64>,
    #[serde(default)]
    pub out_ticks: Option<i64>,
    #[serde(default)]
    pub in_seconds: Option<f64>,
    #[serde(default)]
    pub out_seconds: Option<f64>,
    #[serde(default)]
    pub name: Option<String>,
}

/// K-B3: set/clear an effect zone (clip-relative half-open range). Omit both
/// start and end (or pass null zone) to clear. Supply both start_* and end_*
/// for a zone; end must be > start.
#[derive(Debug, Deserialize)]
pub struct SetEffectZoneArgs {
    pub clip_id: ClipId,
    pub effect_index: usize,
    /// Clear the zone when true (effect applies to whole clip).
    #[serde(default)]
    pub clear: bool,
    #[serde(default)]
    pub start_ticks: Option<i64>,
    #[serde(default)]
    pub end_ticks: Option<i64>,
}

/// Which of the four video effect stacks an `effect_stack` call addresses
/// (26 §10 K-B1/K-B2, 35 §2). Deliberately the same vocabulary as
/// `photonic_core::timeline::commands::VfxOwner`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectScopeArg {
    Clip,
    Track,
    Master,
    Asset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectStackOp {
    List,
    Add,
    Remove,
    Reorder,
    SetParam,
    SetGrade,
}

/// One verb for every scope of the video effect stack. `clip` is included so a
/// single tool covers all four scopes; the older clip-only `add_effect` /
/// `remove_effect` / `reorder_effects` / `set_effect_param` tools remain as the
/// unchanged clip-shaped shorthand.
#[derive(Debug, Deserialize)]
pub struct EffectStackArgs {
    pub scope: EffectScopeArg,
    pub op: EffectStackOp,
    /// `scope=clip`.
    #[serde(default)]
    pub clip_id: Option<ClipId>,
    /// `scope=track`.
    #[serde(default)]
    pub track_id: Option<TrackId>,
    /// `scope=master`; defaults to the active sequence.
    #[serde(default)]
    pub sequence_id: Option<SequenceId>,
    /// `scope=asset`.
    #[serde(default)]
    pub asset_id: Option<AssetId>,
    /// `op=add`: stable manifest id (preferred, see `list_effect_kinds`).
    #[serde(default)]
    pub effect_id: Option<String>,
    /// `op=add`: legacy `EffectKind` tag, used when `effect_id` is absent.
    #[serde(default)]
    pub kind: Option<photonic_core::timeline::EffectKind>,
    /// `op=add` insert position (default: append); `op=remove`/`set_param` index.
    #[serde(default)]
    pub index: Option<usize>,
    /// `op=reorder`: a permutation of `0..len`.
    #[serde(default)]
    pub new_order: Option<Vec<usize>>,
    /// `op=set_param`: a registry `PropPath` (e.g. `"params.radius"`), or the
    /// literal `"enabled"` to toggle the effect itself.
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub value: Option<PropValue>,
    /// `op=set_grade`: a `Grade` object, or `null` to clear it.
    #[serde(default)]
    pub grade: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ListEffectKindsArgs {}

// ─── Effect presets, custom stacks and favourites (26 §10 K-B4) ─────────────
//
// A preset library is USER state (`<config>/Photonic/effect_presets.json`),
// not document state — see `photonic_core::timeline::effect_preset`'s module
// docs. Only `effect_preset_apply` edits the document (and is exactly one undo
// unit); every other verb here manages that config file and produces no
// history entry at all.
//
// The four owner-addressing fields (`scope` + `clip_id`/`track_id`/
// `sequence_id`/`asset_id`) are deliberately spelled exactly as `effect_stack`
// spells them, and are resolved by the same `resolve_owner_fields` helper, so
// "which of the four stacks" cannot come to mean two different things on two
// tools.

#[derive(Debug, Deserialize, Default)]
pub struct EffectPresetListArgs {}

/// Capture the named scope's current effect stack **and** its grade as a saved
/// preset. Config-file write only: no document mutation, no undo step.
#[derive(Debug, Deserialize)]
pub struct EffectPresetSaveArgs {
    /// The preset name. A built-in name is refused (`NotSupportedV1`); an
    /// existing user name is overwritten in place, keeping its position.
    pub name: String,
    pub scope: EffectScopeArg,
    /// `scope=clip`.
    #[serde(default)]
    pub clip_id: Option<ClipId>,
    /// `scope=track`.
    #[serde(default)]
    pub track_id: Option<TrackId>,
    /// `scope=master`; defaults to the active sequence.
    #[serde(default)]
    pub sequence_id: Option<SequenceId>,
    /// `scope=asset`.
    #[serde(default)]
    pub asset_id: Option<AssetId>,
}

/// Apply a preset (built-in or user) to one scope — or, for `scope=clip`, to
/// several clips at once. Always exactly ONE undo unit.
#[derive(Debug, Deserialize)]
pub struct EffectPresetApplyArgs {
    /// Resolved across built-ins first, then the user's own presets.
    pub name: String,
    pub scope: EffectScopeArg,
    /// `scope=clip`, single target.
    #[serde(default)]
    pub clip_id: Option<ClipId>,
    /// `scope=clip`, many targets — applied as one batch, so one undo reverts
    /// the whole apply. May be given instead of `clip_id`.
    #[serde(default)]
    pub clip_ids: Option<Vec<ClipId>>,
    /// `scope=track`.
    #[serde(default)]
    pub track_id: Option<TrackId>,
    /// `scope=master`; defaults to the active sequence.
    #[serde(default)]
    pub sequence_id: Option<SequenceId>,
    /// `scope=asset`.
    #[serde(default)]
    pub asset_id: Option<AssetId>,
}

/// Delete a user preset. Built-ins are read-only (`NotSupportedV1`).
#[derive(Debug, Deserialize)]
pub struct EffectPresetDeleteArgs {
    pub name: String,
}

/// Rename a user preset, keeping its position in the user's ordering. Refuses
/// if either side names a built-in (`NotSupportedV1`).
#[derive(Debug, Deserialize)]
pub struct EffectPresetRenameArgs {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Deserialize, Default)]
pub struct EffectFavouriteListArgs {}

/// Star / unstar one effect id. Idempotent: setting the state it already has
/// succeeds and rewrites nothing.
#[derive(Debug, Deserialize)]
pub struct EffectFavouriteSetArgs {
    /// A stable effect id from `list_effect_kinds`, e.g. `"blur.gaussian"`.
    pub id: String,
    pub favourite: bool,
}

/// One attribute family a `paste_attributes` call may transfer (26 §10 K-B15).
/// Deliberately the same four names as
/// `photonic_core::timeline::ops::AttrSelector`'s flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipAttributeArg {
    Effects,
    Grade,
    Transform,
    Audio,
}

/// **Paste Attributes** (26 §10 K-B15): copy one clip's effect stack / grade /
/// transform / audio onto other, already-existing clips. Timing and source are
/// never touched — this is not the clip clipboard.
#[derive(Debug, Deserialize)]
pub struct PasteAttributesArgs {
    /// The clip whose look is copied.
    pub source_clip_id: ClipId,
    /// The clips it is stamped onto. Must be non-empty.
    pub target_clip_ids: Vec<ClipId>,
    /// Families to transfer; omit for all four. An empty array is refused (it
    /// could only ever be a no-op, and silence there reads as success).
    #[serde(default)]
    pub attributes: Option<Vec<ClipAttributeArg>>,
}

// ─── Keyframes (10 §3.7) ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SetKeyframeArgs {
    #[serde(flatten)]
    pub target: AnimTargetArg,
    pub path: String,
    #[serde(default)]
    pub at_ticks: Option<i64>,
    #[serde(default)]
    pub at_tc: Option<String>,
    #[serde(default)]
    pub at_seconds: Option<f64>,
    pub value: PropValue,
    pub interp: Interp,
}

#[derive(Debug, Deserialize)]
pub struct RemoveKeyframeArgs {
    #[serde(flatten)]
    pub target: AnimTargetArg,
    pub path: String,
    #[serde(default)]
    pub at_ticks: Option<i64>,
    #[serde(default)]
    pub at_tc: Option<String>,
    #[serde(default)]
    pub at_seconds: Option<f64>,
}

/// One entry of `batch_set_keyframes`.
#[derive(Debug, Deserialize)]
pub struct KeyframeOpArg {
    #[serde(flatten)]
    pub target: AnimTargetArg,
    pub path: String,
    #[serde(default)]
    pub at_ticks: Option<i64>,
    #[serde(default)]
    pub at_tc: Option<String>,
    #[serde(default)]
    pub at_seconds: Option<f64>,
    pub value: PropValue,
    pub interp: Interp,
}

/// N keyframes as one undo step (design rule 4).
#[derive(Debug, Deserialize)]
pub struct BatchSetKeyframesArgs {
    pub ops: Vec<KeyframeOpArg>,
}

#[derive(Debug, Deserialize)]
pub struct GetKeyframesArgs {
    #[serde(flatten)]
    pub target: AnimTargetArg,
}

/// K-B11: copy keyframe tracks off a target (returns a serializable payload).
#[derive(Debug, Deserialize)]
pub struct CopyKeyframesArgs {
    #[serde(flatten)]
    pub target: AnimTargetArg,
    /// Optional property-path filter; omit to copy every non-empty track.
    #[serde(default)]
    pub paths: Option<Vec<String>>,
}

/// One source→dest path remap for `paste_keyframes`.
#[derive(Debug, Deserialize)]
pub struct KeyframePathMapArg {
    pub from: String,
    pub to: String,
}

/// K-B11: paste a keyframe clipboard onto a target with optional mapping + offset.
#[derive(Debug, Deserialize)]
pub struct PasteKeyframesArgs {
    #[serde(flatten)]
    pub target: AnimTargetArg,
    /// Payload from `copy_keyframes` (or any `KeyframeClipboard` JSON).
    pub clipboard: photonic_core::timeline::KeyframeClipboard,
    /// Optional path remaps; unmapped sources keep their original path.
    #[serde(default)]
    pub mapping: Vec<KeyframePathMapArg>,
    /// Added to every keyframe time (clip-relative ticks). Ignored when
    /// `reanchor_ticks` is set.
    #[serde(default)]
    pub offset_ticks: i64,
    /// If set, shift so the clipboard anchor lands at this clip-relative tick.
    #[serde(default)]
    pub reanchor_ticks: Option<i64>,
}

// ─── Media (P2 subset: import/relink/list/remove) ───────────────────────────

/// Registers `MediaAsset`s with `probe: None` (ffprobe integration is P3, 02
/// §6) — the result data flags each asset `"probed": false`. Content-hashes
/// the file now (head+tail+len) so `relink_media`'s future by-hash matching
/// has an identity to match against. `bin` names a bin to file the imported
/// asset(s) under — looked up by exact name, created (top-level, no parent)
/// if it doesn't exist yet.
#[derive(Debug, Deserialize)]
pub struct ImportMediaArgs {
    pub paths: Vec<String>,
    #[serde(default)]
    pub bin: Option<String>,
}

/// `allow_hash_mismatch` is the consent gate for rebinding an asset to
/// different bytes (26 K-C6): without it a new file whose content hash differs
/// from the asset's recorded one is refused with `HashMismatch`, because a
/// relink to the wrong take is invisible until export.
#[derive(Debug, Deserialize)]
pub struct RelinkMediaArgs {
    pub asset_id: AssetId,
    pub new_path: String,
    #[serde(default)]
    pub allow_hash_mismatch: bool,
}

/// No arguments — the whole pool is inspected (26 K-C6).
#[derive(Debug, Deserialize, Default)]
pub struct FindOfflineMediaArgs {}

/// Batch relink (26 K-C6): scan `search_dir` and re-point every offline asset
/// it can account for, as ONE undo step.
///
/// * `asset_ids` — restrict to these assets; default is every offline asset.
/// * `recursive` — walk subdirectories (default true; depth- and count-capped).
/// * `dry_run` — report the plan and change nothing. The preview a user gets
///   before committing 200 rebinds.
/// * `allow_hash_mismatch` — commit entries whose bytes differ from the
///   recorded `content_hash` too. Off by default; those entries are otherwise
///   reported under `skipped_hash_mismatch` and left offline.
#[derive(Debug, Deserialize)]
pub struct RelinkMediaBatchArgs {
    pub search_dir: String,
    #[serde(default)]
    pub asset_ids: Option<Vec<AssetId>>,
    #[serde(default)]
    pub recursive: Option<bool>,
    #[serde(default)]
    pub dry_run: Option<bool>,
    #[serde(default)]
    pub allow_hash_mismatch: Option<bool>,
}

/// `bin` filters to assets filed under the bin with that exact name.
#[derive(Debug, Deserialize, Default)]
pub struct ListMediaArgs {
    #[serde(default)]
    pub bin: Option<String>,
}

/// Not in the 10-mcp-tools.md §3.1 catalog table — added to the P2 scope
/// explicitly by the work order (`ops::remove_asset` already exists).
#[derive(Debug, Deserialize)]
pub struct RemoveAssetArgs {
    pub asset_id: AssetId,
}

// ─── Media bins (not in the original §3.1 catalog table — added because the
// media gap-fix (photonic-core commit ab7557f) landed real `MediaAsset.bin`
// support; folded in as standard `create_/remove_/set_/list_` tools rather
// than an op-field mega-tool, since each maps 1:1 to a distinct
// `TimelineCmd` variant, matching design rule 1/2) ─────────────────────────

#[derive(Debug, Deserialize)]
pub struct CreateBinArgs {
    pub name: String,
    #[serde(default)]
    pub parent: Option<BinId>,
}

#[derive(Debug, Deserialize)]
pub struct RemoveBinArgs {
    pub bin_id: BinId,
}

/// `null`/omitted `bin_id` moves the asset to the pool root (unfiled).
#[derive(Debug, Deserialize)]
pub struct SetAssetBinArgs {
    pub asset_id: AssetId,
    #[serde(default)]
    pub bin_id: Option<BinId>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ListBinsArgs {}

// ─── Playback (10 §3.13 — P3 engine slice) ──────────────────────────────────

/// `sequence_id` optional on `play`/`pause`: omitted = the engine's current
/// active sequence (document `active_sequence` fallback, 02 §1).
#[derive(Debug, Deserialize, Default)]
pub struct PlayArgs {
    #[serde(default)]
    pub sequence_id: Option<SequenceId>,
}

#[derive(Debug, Deserialize, Default)]
pub struct PauseArgs {}

#[derive(Debug, Deserialize)]
pub struct SeekArgs {
    pub sequence_id: SequenceId,
    /// Precedence: at_ticks > at_tc > at_seconds (10 §1 rule 3).
    #[serde(default)]
    pub at_ticks: Option<i64>,
    #[serde(default)]
    pub at_tc: Option<String>,
    #[serde(default)]
    pub at_seconds: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct StepArgs {
    /// Signed frame count (CAP-004): +1 = next frame, -1 = previous.
    pub frames: i32,
}

/// `null`/omitted `range` clears the loop.
#[derive(Debug, Deserialize)]
pub struct SetLoopRangeArgs {
    pub sequence_id: SequenceId,
    #[serde(default)]
    pub range: Option<WorkRangeArg>,
}

#[derive(Debug, Deserialize)]
pub struct SetProxyModeArgs {
    /// `auto` | `force_proxy` | `force_original` (02 §6 — session state).
    pub mode: ProxyModeArg,
}

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum ProxyModeArg {
    Auto,
    ForceProxy,
    ForceOriginal,
}

#[derive(Debug, Deserialize, Default)]
pub struct GetEngineStatusArgs {
    /// Accepted for §3.13 parity; the session is a singleton (10 §2), so this
    /// is currently informational only.
    #[serde(default)]
    pub sequence_id: Option<SequenceId>,
}

// ─── Render (10 §3.14 / §4) ─────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RenderFrameAtArgs {
    pub sequence_id: SequenceId,
    /// Precedence: at_ticks > at_tc > at_seconds (10 §1 rule 3).
    #[serde(default)]
    pub at_ticks: Option<i64>,
    #[serde(default)]
    pub at_tc: Option<String>,
    #[serde(default)]
    pub at_seconds: Option<f64>,
    /// Which `SequenceFormat` (aspect variant); default = the active format.
    #[serde(default)]
    pub format_index: Option<usize>,
    /// `preview` (proxy-eligible) or `full` (originals). Required by 10 §4.
    pub quality: RenderQualityArg,
    /// 0 < scale <= 1 — CPU box-downscale of the output.
    #[serde(default)]
    pub scale: Option<f64>,
    /// `png` (default, 8-bit sRGB for display) or `raw_rgba16f` (linear
    /// premultiplied f16, base64 — the deterministic golden-frame basis).
    #[serde(default)]
    pub output_format: Option<RenderOutputFormatArg>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RenderQualityArg {
    Preview,
    Full,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RenderOutputFormatArg {
    #[default]
    Png,
    RawRgba16f,
}

// ─── Media engine ops (10 §3.1 — P3 slice) ──────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ProbeMediaArgs {
    pub asset_id: AssetId,
}

#[derive(Debug, Deserialize)]
pub struct GenerateProxiesArgs {
    pub asset_ids: Vec<AssetId>,
    #[serde(default)]
    pub force: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct RemoveProxyArgs {
    pub asset_ids: Vec<AssetId>,
}

/// Attach a user-owned proxy file to a video asset (G-15A).
#[derive(Debug, Deserialize)]
pub struct AttachProxyArgs {
    pub asset_id: AssetId,
    /// Absolute path to the proxy media file.
    pub path: String,
    /// When true, duration/frame-rate mismatches become warnings.
    #[serde(default)]
    pub allow_mismatch: Option<bool>,
}

/// Clear an asset's proxy ref without deleting user-owned attached files.
#[derive(Debug, Deserialize)]
pub struct DetachProxyArgs {
    pub asset_id: AssetId,
}

#[derive(Debug, Deserialize)]
pub struct TranscodeMediaArgs {
    pub asset_id: AssetId,
    /// `prores_proxy` | `prores_lt` | `dnxhr_lb` | `h264_high` — the fixed
    /// editing-intermediate menu (distinct from export presets, 10 §3.1).
    pub preset: TranscodePresetArg,
    /// Defaults to `<source stem>.<preset>.<ext>` next to the source file.
    #[serde(default)]
    pub out_path: Option<String>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TranscodePresetArg {
    ProresProxy,
    ProresLt,
    DnxhrLb,
    H264High,
}

// ─── Export (10 §3.15) ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ExportSequenceArgs {
    /// Reject export if the live document has changed since inspection.
    #[serde(default)]
    pub expected_revision: Option<u64>,
    pub sequence_id: SequenceId,
    pub out_path: String,
    /// Preset name (built-in or custom); default `"Web H.264"`.
    #[serde(default)]
    pub preset: Option<String>,
    /// Which `SequenceFormat` to export; default = the active format.
    #[serde(default)]
    pub format_index: Option<usize>,
    /// Defaults to the sequence work range, else `[0, content end)`.
    #[serde(default)]
    pub range: Option<WorkRangeArg>,
    #[serde(default)]
    pub overrides: Option<ExportOverridesArg>,
}

/// Inline overrides applied on top of the named preset (10 §3.15).
#[derive(Debug, Deserialize, Default)]
pub struct ExportOverridesArg {
    /// Explicit output resolution (both required together). Must not upscale
    /// beyond the format size in P3 (`NotSupportedV1`).
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    /// Explicit output frame rate; nearest-source-frame retiming (05 §6.2).
    #[serde(default)]
    pub frame_rate: Option<FrameRate>,
}

#[derive(Debug, Deserialize)]
pub struct GetJobStatusArgs {
    pub job_id: uuid::Uuid,
}

#[derive(Debug, Deserialize)]
pub struct CancelJobArgs {
    pub job_id: uuid::Uuid,
}

#[derive(Debug, Deserialize, Default)]
pub struct ListExportPresetsArgs {}

/// `preset` is a full `ExportPreset` object in its serde shape (see
/// `list_export_presets` output for examples); `name` overrides the object's
/// own name field.
#[derive(Debug, Deserialize)]
pub struct SaveExportPresetArgs {
    pub name: String,
    pub preset: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct DeleteExportPresetArgs {
    pub name: String,
}

// ═════════════════════════════════════════════════════════════════════════════
// P4+ slice: captions (§3.8), tts (§3.9), grade (§3.10), node graph (§3.11),
// audio (§3.12), title templates (05 §4b). Time-valued fields follow design
// rule 3 (ticks > tc > seconds), resolved against the owning sequence's frame
// rate by the shared `resolve_tick` helper in `handlers/video.rs`.
// ═════════════════════════════════════════════════════════════════════════════

// ─── Captions (10 §3.8) ─────────────────────────────────────────────────────

/// One word with its own timing (CAP-009/010). Times resolve against the
/// target track's owning sequence frame rate.
#[derive(Debug, Deserialize)]
pub struct CaptionWordArg {
    pub text: String,
    #[serde(default)]
    pub start_ticks: Option<i64>,
    #[serde(default)]
    pub start_tc: Option<String>,
    #[serde(default)]
    pub start_seconds: Option<f64>,
    #[serde(default)]
    pub end_ticks: Option<i64>,
    #[serde(default)]
    pub end_tc: Option<String>,
    #[serde(default)]
    pub end_seconds: Option<f64>,
}

/// Hosted transcription (D-04) → word-level cues (CAP-009). Async job (§6).
/// Supply exactly one of `sequence_id` / `clip_id`. `provider` defaults to the
/// configured hosted service; pass `"mock"` with `mock_transcript` for a
/// deterministic offline/CI run against the `MockTranscriptionProvider`.
#[derive(Debug, Deserialize)]
pub struct AutoCaptionArgs {
    #[serde(default)]
    pub sequence_id: Option<SequenceId>,
    #[serde(default)]
    pub clip_id: Option<ClipId>,
    /// Existing caption track to append to; omit to create a new one.
    #[serde(default)]
    pub track_id: Option<TrackId>,
    /// `"hosted"` (default) or `"mock"`.
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub language_hint: Option<String>,
    /// Offline/CI only: with `provider="mock"`, the deterministic transcript
    /// distributed proportionally across the target range.
    #[serde(default)]
    pub mock_transcript: Option<String>,
    /// Name for a newly-created caption track.
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AddCaptionTrackArgs {
    pub sequence_id: SequenceId,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RemoveCaptionTrackArgs {
    pub track_id: TrackId,
}

#[derive(Debug, Deserialize)]
pub struct GetCaptionTrackArgs {
    pub track_id: TrackId,
}

/// Text/timing/position for one cue; creates a new cue when `cue_id` is
/// omitted. `words` (explicit per-word timing) takes precedence over `text`
/// (distributed proportionally across `[start, end)`).
#[derive(Debug, Deserialize)]
pub struct SetCaptionCueArgs {
    pub track_id: TrackId,
    #[serde(default)]
    pub cue_id: Option<CueId>,
    #[serde(default)]
    pub start_ticks: Option<i64>,
    #[serde(default)]
    pub start_tc: Option<String>,
    #[serde(default)]
    pub start_seconds: Option<f64>,
    #[serde(default)]
    pub end_ticks: Option<i64>,
    #[serde(default)]
    pub end_tc: Option<String>,
    #[serde(default)]
    pub end_seconds: Option<f64>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub words: Option<Vec<CaptionWordArg>>,
    /// Normalized sequence coords `[x, y]`.
    #[serde(default)]
    pub position_override: Option<[f32; 2]>,
}

#[derive(Debug, Deserialize)]
pub struct SplitCaptionCueArgs {
    pub cue_id: CueId,
    #[serde(default)]
    pub at_ticks: Option<i64>,
    #[serde(default)]
    pub at_tc: Option<String>,
    #[serde(default)]
    pub at_seconds: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct MergeCaptionCuesArgs {
    pub cue_id_a: CueId,
    pub cue_id_b: CueId,
}

#[derive(Debug, Deserialize)]
pub struct SetCaptionWordArgs {
    pub cue_id: CueId,
    pub word_index: usize,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub start_ticks: Option<i64>,
    #[serde(default)]
    pub start_tc: Option<String>,
    #[serde(default)]
    pub start_seconds: Option<f64>,
    #[serde(default)]
    pub end_ticks: Option<i64>,
    #[serde(default)]
    pub end_tc: Option<String>,
    #[serde(default)]
    pub end_seconds: Option<f64>,
}

/// Partial caption style — only supplied fields change; the rest inherit the
/// current effective style at the chosen scope (01 §7 cascade word→cue→track).
#[derive(Debug, Deserialize, Default)]
pub struct CaptionStyleArg {
    #[serde(default)]
    pub font_family: Option<String>,
    #[serde(default)]
    pub font_size: Option<f32>,
    #[serde(default)]
    pub weight: Option<u16>,
    /// `#rrggbb` / `#rrggbbaa`.
    #[serde(default)]
    pub fill: Option<String>,
    /// Normalized position `[x, y]`.
    #[serde(default)]
    pub position: Option<[f32; 2]>,
    /// Normalized max width.
    #[serde(default)]
    pub max_width: Option<f32>,
}

/// Scope precedence: `word_index`+`cue_id` = word, `cue_id` = cue, else track.
#[derive(Debug, Deserialize)]
pub struct SetCaptionStyleArgs {
    #[serde(default)]
    pub track_id: Option<TrackId>,
    #[serde(default)]
    pub cue_id: Option<CueId>,
    #[serde(default)]
    pub word_index: Option<usize>,
    /// Clear the cue/word style override (no effect at track scope).
    #[serde(default)]
    pub clear: bool,
    #[serde(default)]
    pub style: Option<CaptionStyleArg>,
}

#[derive(Debug, Deserialize)]
pub struct ImportCaptionsArgs {
    pub track_id: TrackId,
    pub path: String,
    /// `srt` | `vtt` | `ass`; inferred from the file extension when omitted.
    #[serde(default)]
    pub format: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ExportCaptionsArgs {
    pub track_id: TrackId,
    pub path: String,
    /// `srt` | `vtt` | `ass`; inferred from the file extension when omitted.
    #[serde(default)]
    pub format: Option<String>,
}

// ─── TTS (10 §3.9) ───────────────────────────────────────────────────────────

/// Submit text to the configured TTS provider (D-04); on completion inserts an
/// audio clip sized to the returned audio (CAP-011). Async job (§6).
#[derive(Debug, Deserialize)]
pub struct GenerateVoiceoverArgs {
    pub text: String,
    pub track_id: TrackId,
    #[serde(default)]
    pub start_ticks: Option<i64>,
    #[serde(default)]
    pub start_tc: Option<String>,
    #[serde(default)]
    pub start_seconds: Option<f64>,
    #[serde(default)]
    pub voice: Option<String>,
    /// `"hosted"` (default) or `"mock"`.
    #[serde(default)]
    pub provider: Option<String>,
    /// Also add word-level captions from the provider's alignment (06 §6).
    #[serde(default)]
    pub also_caption: bool,
    /// Existing caption track for `also_caption`; a new one is created when
    /// omitted.
    #[serde(default)]
    pub caption_track_id: Option<TrackId>,
}

// ─── Grade (10 §3.10) ────────────────────────────────────────────────────────

/// Full `Grade` replace/patch; `grade` omitted or `null` clears the grade.
#[derive(Debug, Deserialize)]
pub struct SetGradeArgs {
    pub clip_id: ClipId,
    #[serde(default)]
    pub grade: Option<serde_json::Value>,
}

/// Attach (or, with `lut_path` omitted/`null`, remove) a 3D LUT as part of the
/// clip's grade stack.
#[derive(Debug, Deserialize)]
pub struct ApplyLutArgs {
    pub clip_id: ClipId,
    #[serde(default)]
    pub lut_path: Option<String>,
    /// 0..1 blend, default 1.0.
    #[serde(default)]
    pub intensity: Option<f32>,
}

#[derive(Debug, Deserialize)]
pub struct CopyGradeArgs {
    pub source_clip_id: ClipId,
    pub target_clip_ids: Vec<ClipId>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GradePresetOp {
    Save,
    Apply,
    List,
}

#[derive(Debug, Deserialize)]
pub struct GradePresetArgs {
    pub op: GradePresetOp,
    #[serde(default)]
    pub clip_id: Option<ClipId>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GetScopesArgs {
    pub clip_id: ClipId,
    #[serde(default)]
    pub at_ticks: Option<i64>,
    #[serde(default)]
    pub at_tc: Option<String>,
    #[serde(default)]
    pub at_seconds: Option<f64>,
    #[serde(default)]
    pub format_index: Option<usize>,
    /// K-E2 readback point (03 §3.6 / 07 §5). Defaults to
    /// [`ScopeTap::Clip`] — the clip's own texture after its `Grade`, before the
    /// track fold and `CaptionOverlay`. `program` scopes the folded sequence
    /// instead (still pre-`CaptionOverlay`).
    #[serde(default)]
    pub tap: ScopeTap,
}

/// Which texture `get_scopes` measures (K-E2).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeTap {
    #[default]
    Clip,
    Program,
}

// ─── Node graph (10 §3.11) ───────────────────────────────────────────────────

/// Instantiate a per-clip composition (D-06). `detach` reverts the clip to its
/// plain source; an explicit `graph_id` pastes a deep-clone of that graph;
/// otherwise a fresh `ClipIn → Output` composition is created.
#[derive(Debug, Deserialize)]
pub struct CreateClipCompositionArgs {
    pub clip_id: ClipId,
    #[serde(default)]
    pub graph_id: Option<GraphId>,
    #[serde(default)]
    pub detach: bool,
}

/// `op` is a `GraphOp` in its serde shape, e.g. `{"op":"blur"}`,
/// `{"op":"solid_color"}`, `{"op":"merge","mode":"normal"}` (08 §2).
#[derive(Debug, Deserialize)]
pub struct AddGraphNodeArgs {
    pub graph_id: GraphId,
    pub op: serde_json::Value,
    /// Editor position `[x, y]`; default `[0, 0]`.
    #[serde(default)]
    pub pos: Option<[f32; 2]>,
}

#[derive(Debug, Deserialize)]
pub struct RemoveGraphNodeArgs {
    pub graph_id: GraphId,
    pub node_id: GraphNodeId,
}

#[derive(Debug, Deserialize)]
pub struct GraphPortArg {
    pub node_id: GraphNodeId,
    /// Port index; default 0 (primary).
    #[serde(default)]
    pub port: Option<u16>,
}

#[derive(Debug, Deserialize)]
pub struct AddGraphEdgeArgs {
    pub graph_id: GraphId,
    pub from: GraphPortArg,
    pub to: GraphPortArg,
}

#[derive(Debug, Deserialize)]
pub struct RemoveGraphEdgeArgs {
    pub graph_id: GraphId,
    pub edge_index: usize,
}

#[derive(Debug, Deserialize)]
pub struct SetGraphNodeParamArgs {
    pub graph_id: GraphId,
    pub node_id: GraphNodeId,
    pub path: String,
    pub value: PropValue,
}

/// `graph_id` sets the project graph to an existing arena graph; `clear`
/// removes it; omit both to create a fresh empty project graph.
#[derive(Debug, Deserialize)]
pub struct SetProjectGraphArgs {
    #[serde(default)]
    pub graph_id: Option<GraphId>,
    #[serde(default)]
    pub clear: bool,
}

#[derive(Debug, Deserialize)]
pub struct GetGraphArgs {
    pub graph_id: GraphId,
}

// ─── Audio (10 §3.12) ────────────────────────────────────────────────────────

/// Per-clip audio (01 §5 `ClipAudio`). A `fade_*_ticks` of `0` clears that
/// fade; a positive value sets it.
#[derive(Debug, Deserialize)]
pub struct SetClipAudioArgs {
    pub clip_id: ClipId,
    #[serde(default)]
    pub gain_db: Option<f64>,
    #[serde(default)]
    pub fade_in_ticks: Option<i64>,
    #[serde(default)]
    pub fade_out_ticks: Option<i64>,
    #[serde(default)]
    pub fade_shape: Option<FadeShape>,
    #[serde(default)]
    pub channel_map: Option<ChannelMap>,
}

#[derive(Debug, Deserialize)]
pub struct SetTrackAudioArgs {
    pub track_id: TrackId,
    #[serde(default)]
    pub volume_db: Option<f64>,
    #[serde(default)]
    pub pan: Option<f64>,
    #[serde(default)]
    pub muted: Option<bool>,
    #[serde(default)]
    pub solo: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioFxOp {
    Add,
    Remove,
    Reorder,
}

/// Add/remove/reorder an EQ/compressor/limiter/gate unit in a track's pre-fader
/// fx chain (09 §4).
#[derive(Debug, Deserialize)]
pub struct AudioFxArgs {
    pub track_id: TrackId,
    pub op: AudioFxOp,
    /// Required for `add`: `eq` | `compressor` | `limiter` | `gate`.
    #[serde(default)]
    pub kind: Option<photonic_core::timeline::AudioFxKind>,
    /// Insertion (add) or removal index; default = end for add.
    #[serde(default)]
    pub index: Option<usize>,
    /// Required for `reorder`: the new chain order as source indices.
    #[serde(default)]
    pub new_order: Option<Vec<usize>>,
}

/// Master bus level/loudness (09 §4). `loudness`: `streaming` | `broadcast` |
/// `none`.
#[derive(Debug, Deserialize)]
pub struct SetMasterBusArgs {
    pub sequence_id: SequenceId,
    #[serde(default)]
    pub volume_db: Option<f64>,
    #[serde(default)]
    pub loudness: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GetAudioMetersArgs {
    pub sequence_id: SequenceId,
}

/// Decoded waveform peak-pyramid summary for an asset or clip's asset (09 §8,
/// sidecar-cached per 01 §9). Supply exactly one of `asset_id` / `clip_id`.
#[derive(Debug, Deserialize)]
pub struct GetWaveformArgs {
    #[serde(default)]
    pub asset_id: Option<AssetId>,
    #[serde(default)]
    pub clip_id: Option<ClipId>,
    /// Target number of peak buckets in the response, default 512.
    #[serde(default)]
    pub resolution: Option<usize>,
}

// ─── Title templates (05 §4b) ────────────────────────────────────────────────

#[derive(Debug, Deserialize, Default)]
pub struct ListTitleTemplatesArgs {}

#[derive(Debug, Deserialize)]
pub struct InsertTitleTemplateArgs {
    pub template: String,
    pub track_id: TrackId,
    #[serde(default)]
    pub start_ticks: Option<i64>,
    #[serde(default)]
    pub start_tc: Option<String>,
    #[serde(default)]
    pub start_seconds: Option<f64>,
    #[serde(default)]
    pub text_overrides: Option<std::collections::HashMap<String, String>>,
}

// ── D-12 gyro stabilization (22 §6.5) ───────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ImportMotionMetadataArgs {
    pub clip_id: ClipId,
    /// Path to a `.gcsv` or Photonic gyro JSON sidecar.
    pub path: String,
    /// Optional lens-profile JSON. Without one, only rotation is corrected.
    #[serde(default)]
    pub lens_profile_path: Option<String>,
    #[serde(default)]
    pub smoothness: Option<f32>,
    #[serde(default)]
    pub horizon_lock: Option<f32>,
}

#[derive(Debug, Deserialize)]
pub struct SetStabilizationArgs {
    pub clip_id: ClipId,
    #[serde(default)]
    pub smoothness: Option<f32>,
    #[serde(default)]
    pub horizon_lock: Option<f32>,
    #[serde(default)]
    pub max_zoom: Option<f32>,
    /// `static_safe` | `dynamic` | `transparent_edges`.
    #[serde(default)]
    pub crop_mode: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AnalyzeStabilizationArgs {
    pub clip_id: ClipId,
}

#[derive(Debug, Deserialize)]
pub struct GetStabilizationStatusArgs {
    pub clip_id: ClipId,
}
