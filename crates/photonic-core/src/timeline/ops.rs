//! Pure timeline edit ops (01 §10): `fn …(…) -> Result<TimelineCmd, EditError>`.
//!
//! Every op reads the current project to capture the old state and construct the
//! command that performs the edit — it never mutates. The GUI and MCP both call
//! these (CAP-019 parity), then hand the command to the history for apply/undo.
//! Invariants (non-overlap, `duration > 0`, sorted, cycle-freedom, the
//! composition-on-`Adjustment` rejection) are enforced here, before any command
//! exists — an invalid edit returns `Err` and produces no document change.
//!
//! A handful of ops perform two arena mutations atomically (create/paste a
//! composition, set the project graph); those return `Vec<TimelineCmd>` for the
//! caller to wrap in a single `Command::Batch`, mirroring the existing
//! `GroupNodes` batching idiom.

use super::anim::{AnimProps, Interp, Keyframe, KeyframeClipboard, PropPath, PropertyTrack};
use super::audio::ClipAudio;
use super::clip::{
    Clip, ClipEffect, ClipSource, ClipTransform, LinkGroupId, MulticamAngle, MulticamGroup, Ratio,
    SpeedKey, SpeedMap, TextClipContent,
};
use super::commands::{
    AnimTarget, AudioCmd, ClipTiming, FormatOp, FxOwner, TimelineCmd, TrackSettings, VfxOwner,
};
use super::grade::{Grade, GradeOpParams};
use super::graph::{GraphOp, NodeGraph};
use super::ids::*;
use super::media::MediaBin;
use super::sequence::{
    Marker, MarkerCategory, MarkerGlyph, MarkerRef, MarkerRetarget, Sequence, SequenceFormat,
    TimelineProject, Track, TrackKind,
};
use super::time::{FrameRate, Tick};
use std::path::PathBuf;

/// A rejected timeline edit — no command is produced and the document is
/// unchanged.
#[derive(Debug, Clone, PartialEq)]
pub enum EditError {
    NoProject,
    NoSequence(SequenceId),
    NoTrack(TrackId),
    NoClip(ClipId),
    NoAsset(AssetId),
    NoGraph(GraphId),
    NoGradeOp(GradeOpId),
    /// The requested placement/trim would overlap another clip on the track.
    Overlap,
    /// A clip duration would be `<= 0`.
    NonPositiveDuration,
    /// A split point was not strictly inside the clip.
    InvalidSplit,
    /// An index was out of range for the target vector.
    IndexOutOfRange,
    /// A graph edge would create a cycle (01 §8).
    WouldCreateCycle,
    /// A composition was requested on a `ClipSource::Adjustment` clip (07 §6.6).
    CompositionOnAdjustment,
    /// A ducking/sidechain wiring would create a cycle (09 §6.3).
    SidechainCycle,
    /// A nested-sequence insertion would create a sequence cycle (CAP-005).
    SequenceCycle,
    /// No marker with this id in the addressed scope (35 §1).
    NoMarker(MarkerId),
    /// No marker category with this id on the project (35 §1.3). Also returned
    /// when a delete is asked to reassign its markers to the very category
    /// being deleted — after the delete that target would not exist.
    NoMarkerCategory(MarkerCategoryId),
    /// Manifest `Applicability` forbids attaching this effect to the chosen
    /// scope (clip / track / master / asset). K-B1 residual.
    ApplicabilityDenied,
    /// A speed map has invalid key timing, ratios, or Bezier handles.
    InvalidSpeedMap,
    /// The addressed track is edit-locked.
    TrackLocked,
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for EditError {}

// ── Read helpers ────────────────────────────────────────────────────────────

fn seq(p: &TimelineProject, id: SequenceId) -> Result<&Sequence, EditError> {
    p.sequences.get(&id).ok_or(EditError::NoSequence(id))
}

fn track(s: &Sequence, id: TrackId) -> Result<&Track, EditError> {
    s.track(id).ok_or(EditError::NoTrack(id))
}

fn clip(t: &Track, id: ClipId) -> Result<&Clip, EditError> {
    t.clips
        .iter()
        .find(|c| c.id == id)
        .ok_or(EditError::NoClip(id))
}

/// True if `[start, end)` would overlap any clip on `t` other than `ignore`.
fn overlaps_other(t: &Track, start: Tick, end: Tick, ignore: Option<ClipId>) -> bool {
    t.clips
        .iter()
        .filter(|c| Some(c.id) != ignore)
        .any(|c| c.start < end && start < c.end())
}

// ── Project / media ─────────────────────────────────────────────────────────

/// Create the timeline project (first video-mode action, 01 §2).
pub fn create_project() -> TimelineCmd {
    TimelineCmd::CreateProject {
        project: Box::new(TimelineProject::new()),
    }
}

pub fn add_asset(asset: super::media::MediaAsset) -> TimelineCmd {
    TimelineCmd::AddAsset {
        asset: Box::new(asset),
    }
}

pub fn remove_asset(p: &TimelineProject, asset: AssetId) -> Result<TimelineCmd, EditError> {
    let a = p
        .media
        .assets
        .get(&asset)
        .ok_or(EditError::NoAsset(asset))?;
    Ok(TimelineCmd::RemoveAsset {
        asset: Box::new(a.clone()),
    })
}

/// K-A8: create a subclip pool entry — a zone-bounded view of `parent` that
/// shares `content_hash` / proxy / probe / source path so caches are not
/// duplicated. `range` is half-open source ticks `[in, out)` on the parent
/// media. Optional `name` labels the bin entry (default `Parent · in–out`).
///
/// Returns `AddAsset` for the new subclip. Refuses a missing parent, a range
/// with `out <= in`, or nesting a subclip of a subclip (resolve to the root
/// parent instead is not done automatically — call with the root).
pub fn create_subclip(
    p: &TimelineProject,
    parent: AssetId,
    range: (Tick, Tick),
    name: Option<String>,
) -> Result<(TimelineCmd, AssetId), EditError> {
    let (rin, rout) = range;
    if rout.0 <= rin.0 {
        return Err(EditError::NonPositiveDuration);
    }
    let parent_asset = p
        .media
        .assets
        .get(&parent)
        .ok_or(EditError::NoAsset(parent))?;
    if parent_asset.is_subclip() {
        // Nested subclips would require range composition; refuse rather than
        // silently mis-offset. Callers should use the root parent.
        return Err(EditError::Overlap);
    }
    let mut child = parent_asset.clone();
    child.id = AssetId::new();
    child.parent = Some(parent);
    child.subclip_range = Some((rin, rout));
    // Display name rides on tags for now (MediaAsset has no name field —
    // pool UI uses path basename). Put a human label in tags[0] when supplied.
    if let Some(n) = name {
        if !n.is_empty() {
            child.tags.insert(0, format!("subclip:{n}"));
        }
    } else {
        child
            .tags
            .insert(0, format!("subclip:{}-{}", rin.0, rout.0));
    }
    let id = child.id;
    Ok((add_asset(child), id))
}

/// Default timeline placement for a subclip asset: `source_in` = range start,
/// duration = range width. Pure helper for GUI/MCP insert.
pub fn subclip_default_timing(asset: &super::media::MediaAsset) -> Option<(Tick, Tick)> {
    let (a, b) = asset.subclip_range?;
    Some((a, Tick(b.0 - a.0)))
}

/// K-C2: set or clear a 1–5 star rating (`None` / out-of-range clears).
pub fn set_asset_rating(
    p: &TimelineProject,
    asset: AssetId,
    rating: Option<u8>,
) -> Result<TimelineCmd, EditError> {
    let a = p
        .media
        .assets
        .get(&asset)
        .ok_or(EditError::NoAsset(asset))?;
    let new = rating.filter(|r| (1..=5).contains(r));
    Ok(TimelineCmd::SetAssetRating {
        asset,
        old: a.rating,
        new,
    })
}

/// K-C2: replace free-form tags (deduped, trimmed, non-empty).
///
/// Prefer [`set_asset_tags_resolved`] when you want the registry + `tag_ids`
/// list updated in the same batch.
pub fn set_asset_tags(
    p: &TimelineProject,
    asset: AssetId,
    tags: Vec<String>,
) -> Result<TimelineCmd, EditError> {
    let a = p
        .media
        .assets
        .get(&asset)
        .ok_or(EditError::NoAsset(asset))?;
    let mut new: Vec<String> = tags
        .into_iter()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    new.sort();
    new.dedup();
    Ok(TimelineCmd::SetAssetTags {
        asset,
        old: a.tags.clone(),
        new,
    })
}

/// K-C2: ensure each name exists in the media-tag registry, assign `tag_ids`,
/// and refresh free-form names. Returns a batch of commands (registry adds +
/// asset updates). One call → one undo unit when wrapped in `Command::Batch`.
pub fn set_asset_tags_resolved(
    p: &TimelineProject,
    asset: AssetId,
    tags: Vec<String>,
) -> Result<Vec<TimelineCmd>, EditError> {
    use super::media::MediaTag;

    let a = p
        .media
        .assets
        .get(&asset)
        .ok_or(EditError::NoAsset(asset))?;
    let mut names: Vec<String> = tags
        .into_iter()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    names.sort();
    names.dedup();

    let mut cmds = Vec::new();
    let mut ids = Vec::new();
    // Work on a virtual registry so multiple new names in one call don't collide.
    let mut virtual_tags = p.media_tags.clone();
    for name in &names {
        if let Some(existing) = virtual_tags
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(name))
        {
            ids.push(existing.id);
            continue;
        }
        let tag = MediaTag::new(name.clone());
        ids.push(tag.id);
        let index = virtual_tags.len();
        virtual_tags.push(tag.clone());
        cmds.push(TimelineCmd::AddMediaTag { tag, index });
    }
    ids.sort_by_key(|id| id.0);
    ids.dedup();

    if names != a.tags {
        cmds.push(TimelineCmd::SetAssetTags {
            asset,
            old: a.tags.clone(),
            new: names,
        });
    }
    if ids != a.tag_ids {
        cmds.push(TimelineCmd::SetAssetTagIds {
            asset,
            old: a.tag_ids.clone(),
            new: ids,
        });
    }
    Ok(cmds)
}

/// K-C2: add a named tag to the project registry (no-op if name already exists).
pub fn add_media_tag(
    p: &TimelineProject,
    name: impl Into<String>,
) -> Result<Option<TimelineCmd>, EditError> {
    use super::media::MediaTag;
    let name = name.into().trim().to_string();
    if name.is_empty() {
        return Ok(None);
    }
    if p.media_tag_by_name(&name).is_some() {
        return Ok(None);
    }
    let tag = MediaTag::new(name);
    Ok(Some(TimelineCmd::AddMediaTag {
        tag,
        index: p.media_tags.len(),
    }))
}

/// K-C2: replace an asset's tag-id list (deduped). Unknown ids are kept — the
/// registry may reintroduce them later (same orphan rule as marker categories).
pub fn set_asset_tag_ids(
    p: &TimelineProject,
    asset: AssetId,
    tag_ids: Vec<super::ids::TagId>,
) -> Result<TimelineCmd, EditError> {
    let a = p
        .media
        .assets
        .get(&asset)
        .ok_or(EditError::NoAsset(asset))?;
    let mut new = tag_ids;
    new.sort_by_key(|id| id.0);
    new.dedup();
    Ok(TimelineCmd::SetAssetTagIds {
        asset,
        old: a.tag_ids.clone(),
        new,
    })
}

/// Every asset a grade stack names. Currently only `Lut3d` (07 §4), but this is
/// the one place to extend when a grade op gains another asset-backed operand.
fn collect_grade_asset_refs(g: &Grade, used: &mut std::collections::HashSet<AssetId>) {
    // Counted regardless of `op.enabled` / `g.bypass`: a bypassed op still holds
    // the reference, and re-enabling it must not find the LUT deleted.
    for op in &g.ops {
        if let GradeOpParams::Lut3d { asset, .. } = &op.params.base {
            used.insert(*asset);
        }
    }
}

/// K-C5: assets with zero project references (candidates for remove-unused).
///
/// A clip source is **not** the only way to reference an asset, and treating it
/// as such is a data-loss bug: an `AssetKind::Lut3d` is referenced solely by a
/// `GradeOp::Lut3d`, never by a clip, so a clip-only scan reports every LUT in
/// the project as unused. This walks all four reference classes — clip sources,
/// grade stacks at each of the four scopes (35 §2), embedded graph grades, and
/// the `MediaIn`/`Lut` graph ops.
///
/// The result is sorted so the removal batch is deterministic; the asset arena
/// is a `HashMap` and would otherwise yield an arbitrary order per run.
pub fn unused_assets(p: &TimelineProject) -> Vec<AssetId> {
    let mut used = std::collections::HashSet::new();

    for seq in p.sequences.values() {
        // Master scope (35 §2).
        if let Some(g) = &seq.master_grade {
            collect_grade_asset_refs(g, &mut used);
        }
        for track in seq.tracks() {
            // Track scope.
            if let Some(g) = &track.grade {
                collect_grade_asset_refs(g, &mut used);
            }
            for clip in &track.clips {
                if let Some(id) = clip.source.asset() {
                    used.insert(id);
                }
                // Clip scope.
                if let Some(g) = &clip.grade {
                    collect_grade_asset_refs(g, &mut used);
                }
            }
        }
    }

    // Asset scope: a grade bound beneath every clip referencing that asset.
    // Note this can keep a LUT alive for an otherwise-unused asset; the LUT then
    // falls out on the next pass, once that asset is gone.
    for asset in p.media.assets.values() {
        if let Some(g) = &asset.grade {
            collect_grade_asset_refs(g, &mut used);
        }
    }

    // Node graphs are one arena (01 §8) — per-clip compositions and the project
    // graph alike, so walking `p.graphs` covers both without resolving owners.
    for graph in p.graphs.values() {
        for node in graph.nodes.values() {
            match &node.op {
                GraphOp::MediaIn { asset, .. } | GraphOp::Lut { asset } => {
                    used.insert(*asset);
                }
                GraphOp::Grade { grade } => collect_grade_asset_refs(grade, &mut used),
                _ => {}
            }
        }
    }

    let mut unused: Vec<AssetId> = p
        .media
        .assets
        .keys()
        .copied()
        .filter(|id| !used.contains(id))
        .collect();
    unused.sort();
    unused
}

/// K-C5: batch-remove every unused asset as one undoable step (caller wraps
/// in `Command::Batch` if multiple). Returns one `RemoveAsset` per id.
pub fn remove_unused_assets(p: &TimelineProject) -> Vec<TimelineCmd> {
    unused_assets(p)
        .into_iter()
        .filter_map(|id| remove_asset(p, id).ok())
        .collect()
}

/// Repoint one asset at a new file (26 K-C6 single case).
///
/// # `rel_path` (read this before changing the command shape)
///
/// [`AssetSource::File`](super::media::AssetSource::File) carries
/// `{ path, rel_path }`, and `media.rs`'s doc comment specifies the load ladder
/// as *`rel_path` first, then `path`, then relink-by-hash* — the mechanism that
/// makes a **moved project** reopen with no relink at all. `RelinkAsset` rewrites
/// `path` only and deliberately leaves `rel_path` alone, which is correct **only
/// because `rel_path` is currently vestigial**: nothing in the workspace ever
/// writes a `Some(..)` (grep `rel_path` — every construction site is `None`) and
/// no loader reads it. The day the ladder is implemented, a relink that leaves a
/// stale `rel_path` in place would be silently overridden by it on the next open
/// — so `TimelineCmd::RelinkAsset` must grow `old_rel_path`/`new_rel_path` (kept
/// undoable) *in the same change* that starts populating the field. Filed as the
/// K-C6 follow-up; not fixable from `ops.rs` alone since the command shape lives
/// in `commands.rs`.
pub fn relink_asset(
    p: &TimelineProject,
    asset: AssetId,
    new_path: PathBuf,
) -> Result<TimelineCmd, EditError> {
    let a = p
        .media
        .assets
        .get(&asset)
        .ok_or(EditError::NoAsset(asset))?;
    let old_path = match &a.source {
        super::media::AssetSource::File { path, .. } => path.clone(),
        _ => PathBuf::new(),
    };
    Ok(TimelineCmd::RelinkAsset {
        asset,
        old_path,
        new_path,
    })
}

// ── K-C6: batch relink of offline media ─────────────────────────────────────
//
// The single-asset relink above has existed since P2. What a user who moved a
// folder actually needs is the *batch*: 200 offline clips, one directory
// rewrite, one undo step. The planner below is deliberately pure — it takes the
// scan result as data and returns a plan; the filesystem walk and the hashing
// live in the callers (MCP handler / media-pool panel), which keeps
// `photonic-core` free of I/O and makes every matching rule unit-testable
// without touching a disk.

/// One file found by the caller's scan of a relink search root.
#[derive(Clone, Debug, PartialEq)]
pub struct RelinkCandidate {
    pub path: PathBuf,
    /// Content hash of this file, in the same shape as
    /// [`MediaAsset::content_hash`](super::media::MediaAsset::content_hash).
    /// `None` when the caller chose not to hash the scan (large folders); the
    /// by-hash *discovery* rule then simply does not fire, while per-entry
    /// verification still can (it hashes only the chosen file).
    pub content_hash: Option<String>,
}

/// Which rule bound a candidate to an offline asset, strongest first.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RelinkMatchKind {
    /// Same bytes (`content_hash` equality) — the relink identity per
    /// `media.rs`'s module docs. Survives a rename.
    ContentHash,
    /// Same file name, byte-for-byte.
    ExactName,
    /// Same file name ignoring ASCII case (a case-insensitive volume, or media
    /// copied through one, routinely changes case).
    CaseInsensitiveName,
}

/// Does the file we are about to bind actually hold the asset's bytes?
///
/// `Unknown` is a first-class outcome and is **not** an error: the asset may
/// never have been hashed, the caller may not be able to recompute the stored
/// hash's algorithm, or the file may be unreadable. Never report `Mismatch`
/// unless two hashes were genuinely compared — a false mismatch would train
/// users to click past the one guard that catches a wrong-take relink.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RelinkHashCheck {
    Match,
    Mismatch,
    Unknown,
}

impl RelinkHashCheck {
    pub fn as_str(self) -> &'static str {
        match self {
            RelinkHashCheck::Match => "match",
            RelinkHashCheck::Mismatch => "mismatch",
            RelinkHashCheck::Unknown => "unknown",
        }
    }
}

impl RelinkMatchKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RelinkMatchKind::ContentHash => "content_hash",
            RelinkMatchKind::ExactName => "exact_name",
            RelinkMatchKind::CaseInsensitiveName => "case_insensitive_name",
        }
    }
}

/// One proposed relink: what would change, how it was found, and whether the
/// bytes agree. This is the preview row — nothing is committed until
/// [`relink_plan_commands`] turns accepted entries into commands.
#[derive(Clone, Debug, PartialEq)]
pub struct RelinkPlanEntry {
    pub asset: AssetId,
    pub old_path: PathBuf,
    pub new_path: PathBuf,
    pub matched_by: RelinkMatchKind,
    pub hash: RelinkHashCheck,
    /// Hash of `new_path` as computed by the caller's hasher, when it could be
    /// computed. Used to re-identify the asset once a byte change is accepted.
    pub new_hash: Option<String>,
    /// More than one scanned file matched by the same rule; the plan picked the
    /// lexicographically smallest path so the result is deterministic, but the
    /// choice is a guess and the UI must say so.
    pub ambiguous: bool,
}

/// The result of planning a batch relink.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RelinkPlan {
    pub entries: Vec<RelinkPlanEntry>,
    /// Offline assets no scanned file matched, sorted.
    pub unmatched: Vec<AssetId>,
}

impl RelinkPlan {
    /// Entries whose bytes are known to differ from the asset's recorded hash —
    /// the ones a caller must not commit without explicit consent.
    pub fn mismatched(&self) -> impl Iterator<Item = &RelinkPlanEntry> {
        self.entries
            .iter()
            .filter(|e| e.hash == RelinkHashCheck::Mismatch)
    }
}

/// Every file-backed asset whose file is not reachable, sorted for determinism.
///
/// `exists` is injected rather than calling `Path::exists` so `photonic-core`
/// stays I/O-free and the predicate can be faked in tests. `EmbeddedVector`
/// assets are never offline (they live in the document).
pub fn offline_assets(
    p: &TimelineProject,
    mut exists: impl FnMut(&std::path::Path) -> bool,
) -> Vec<AssetId> {
    let mut out: Vec<AssetId> = p
        .media
        .assets
        .values()
        .filter(|a| match &a.source {
            super::media::AssetSource::File { path, .. } => !exists(path),
            super::media::AssetSource::EmbeddedVector { .. } => false,
        })
        .map(|a| a.id)
        .collect();
    out.sort();
    out
}

fn file_name_of(p: &std::path::Path) -> Option<String> {
    p.file_name().map(|n| n.to_string_lossy().into_owned())
}

/// Plan a batch relink of `offline` against the files in `candidates`.
///
/// Rule order per asset — strongest identity first, which is the order
/// `media.rs` specifies (`content_hash` first, then filename):
///
/// 1. **`content_hash` equality.** Survives a rename, and is the only rule that
///    can be wrong solely through a hash collision.
/// 2. **Exact file name.**
/// 3. **Case-insensitive file name.**
///
/// Whichever rule fires, the entry then records a *verification*: the chosen
/// file is hashed via `hash_like(stored_hash, candidate_path)` and compared to
/// the asset's stored hash. `hash_like` is passed the stored hash (`None` when
/// the asset has never been hashed) precisely so a caller can hash with **the
/// same algorithm that produced it** and return `None` when it cannot — a
/// cross-algorithm comparison would manufacture a mismatch on every asset (the
/// P2 `siphash64:` stopgap vs the engine's xxh3).
///
/// Ties are broken on the lexicographically smallest path and flagged
/// `ambiguous`, so the same scan always produces the same plan.
///
/// An asset whose best match is the path it *already* points at appears in
/// neither list: there is nothing to change, and emitting it would spend an undo
/// step on a no-op. (Reachable only when a caller passes online assets
/// explicitly — the offline set never contains one.)
pub fn plan_relink(
    p: &TimelineProject,
    offline: &[AssetId],
    candidates: &[RelinkCandidate],
    mut hash_like: impl FnMut(Option<&str>, &std::path::Path) -> Option<String>,
) -> RelinkPlan {
    let mut plan = RelinkPlan::default();
    let mut ids: Vec<AssetId> = offline.to_vec();
    ids.sort();
    ids.dedup();

    for id in ids {
        let Some(asset) = p.media.assets.get(&id) else {
            continue;
        };
        let super::media::AssetSource::File { path: old_path, .. } = &asset.source else {
            continue; // embedded vectors have no file to relink
        };
        let want_name = file_name_of(old_path);

        // Collect the matches for each rule, then take the strongest non-empty.
        let by_hash: Vec<&RelinkCandidate> = match asset.content_hash.as_deref() {
            Some(h) => candidates
                .iter()
                .filter(|c| c.content_hash.as_deref() == Some(h))
                .collect(),
            None => Vec::new(),
        };
        let by_exact: Vec<&RelinkCandidate> = match &want_name {
            Some(name) => candidates
                .iter()
                .filter(|c| file_name_of(&c.path).as_deref() == Some(name.as_str()))
                .collect(),
            None => Vec::new(),
        };
        let by_ci: Vec<&RelinkCandidate> = match &want_name {
            Some(name) => candidates
                .iter()
                .filter(|c| {
                    file_name_of(&c.path)
                        .map(|n| n.eq_ignore_ascii_case(name))
                        .unwrap_or(false)
                })
                .collect(),
            None => Vec::new(),
        };

        let (matched_by, mut matches) = if !by_hash.is_empty() {
            (RelinkMatchKind::ContentHash, by_hash)
        } else if !by_exact.is_empty() {
            (RelinkMatchKind::ExactName, by_exact)
        } else if !by_ci.is_empty() {
            (RelinkMatchKind::CaseInsensitiveName, by_ci)
        } else {
            plan.unmatched.push(id);
            continue;
        };
        matches.sort_by(|a, b| a.path.cmp(&b.path));
        let ambiguous = matches.len() > 1;
        let chosen = matches[0];
        if &chosen.path == old_path {
            // The file the asset already points at (a caller that scanned a
            // still-online asset's own directory). Relinking it would be a
            // no-op undo step.
            continue;
        }

        // Verify the chosen file's bytes against the asset's identity.
        let (check, new_hash) = match asset.content_hash.as_deref() {
            None => (RelinkHashCheck::Unknown, hash_like(None, &chosen.path)),
            Some(stored) => match hash_like(Some(stored), &chosen.path) {
                None => (RelinkHashCheck::Unknown, None),
                Some(actual) => {
                    let check = if actual == stored {
                        RelinkHashCheck::Match
                    } else {
                        RelinkHashCheck::Mismatch
                    };
                    (check, Some(actual))
                }
            },
        };

        plan.entries.push(RelinkPlanEntry {
            asset: id,
            old_path: old_path.clone(),
            new_path: chosen.path.clone(),
            matched_by,
            hash: check,
            new_hash,
            ambiguous,
        });
    }
    plan.unmatched.sort();
    plan
}

/// Turn accepted plan entries into commands — the caller wraps the whole `Vec`
/// in ONE `Command::Batch` so a 200-clip relink is a single undo unit (DoD 4).
///
/// Two data-integrity rules are enforced here rather than left to each caller:
///
/// * A [`RelinkHashCheck::Mismatch`] entry is **skipped** unless
///   `accept_mismatch`. Binding a clip to the wrong take is a failure the user
///   would not notice until export.
/// * When a mismatch *is* accepted, the asset is re-identified in the same
///   batch: `content_hash` becomes the new file's hash and `probe` is cleared,
///   because a probe describes the bytes that were probed — keeping the old
///   duration/resolution around for a different file is exactly the silent lie
///   the hash check exists to prevent. Re-probe (`probe_media`) refills it.
///   An asset that had *no* hash gets the new one recorded, keeping its probe.
pub fn relink_plan_commands(
    p: &TimelineProject,
    entries: &[RelinkPlanEntry],
    accept_mismatch: bool,
) -> Vec<TimelineCmd> {
    let mut cmds = Vec::new();
    for e in entries {
        if e.hash == RelinkHashCheck::Mismatch && !accept_mismatch {
            continue;
        }
        if e.new_path == e.old_path {
            continue;
        }
        let Ok(relink) = relink_asset(p, e.asset, e.new_path.clone()) else {
            continue;
        };
        cmds.push(relink);
        let Some(asset) = p.media.assets.get(&e.asset) else {
            continue;
        };
        match e.hash {
            RelinkHashCheck::Mismatch => {
                if let Ok(meta) = set_asset_meta(p, e.asset, None, e.new_hash.clone()) {
                    cmds.push(meta);
                }
            }
            RelinkHashCheck::Unknown if asset.content_hash.is_none() && e.new_hash.is_some() => {
                if let Ok(meta) =
                    set_asset_meta(p, e.asset, asset.probe.clone(), e.new_hash.clone())
                {
                    cmds.push(meta);
                }
            }
            _ => {}
        }
    }
    cmds
}

/// Set or clear an asset's proxy attachment while preserving a lossless undo
/// snapshot. Used by background proxy generation when a job moves through
/// Pending → Ready/Failed.
pub fn set_asset_proxy(
    p: &TimelineProject,
    asset: AssetId,
    new: Option<super::media::ProxyRef>,
) -> Result<TimelineCmd, EditError> {
    let old = p
        .media
        .assets
        .get(&asset)
        .ok_or(EditError::NoAsset(asset))?
        .proxy
        .clone();
    Ok(TimelineCmd::SetAssetProxy { asset, old, new })
}

/// Update probe + content hash after L0 pool registration (24-preview-media-load
/// L1/L2). Row already exists via [`add_asset`]; this fills metadata without
/// removing/re-adding the asset id.
pub fn set_asset_meta(
    p: &TimelineProject,
    asset: AssetId,
    new_probe: Option<super::media::MediaProbe>,
    new_hash: Option<String>,
) -> Result<TimelineCmd, EditError> {
    let a = p
        .media
        .assets
        .get(&asset)
        .ok_or(EditError::NoAsset(asset))?;
    Ok(TimelineCmd::SetAssetMeta {
        asset,
        old_probe: a.probe.clone(),
        new_probe,
        old_hash: a.content_hash.clone(),
        new_hash,
    })
}

/// Set project policy for auto proxy generation on import (G-15C / 24 L7).
pub fn set_generate_proxies_on_import(p: &TimelineProject, new: bool) -> TimelineCmd {
    TimelineCmd::SetGenerateProxiesOnImport {
        old: p.settings.generate_proxies,
        new,
    }
}

// ── Sequences / formats / tracks ────────────────────────────────────────────

pub fn add_sequence(s: Sequence) -> TimelineCmd {
    TimelineCmd::AddSequence {
        sequence: Box::new(s),
    }
}

pub fn remove_sequence(p: &TimelineProject, id: SequenceId) -> Result<TimelineCmd, EditError> {
    let s = seq(p, id)?;
    let order_index = p.sequence_order.iter().position(|x| *x == id).unwrap_or(0);
    Ok(TimelineCmd::RemoveSequence {
        sequence: Box::new(s.clone()),
        order_index,
        was_active: p.active_sequence == Some(id),
    })
}

pub fn set_active_sequence(p: &TimelineProject, new: Option<SequenceId>) -> TimelineCmd {
    TimelineCmd::SetActiveSequence {
        old: p.active_sequence,
        new,
    }
}

/// Create a new (empty) sequence and return the command that adds it (17
/// §G-17). A thin convenience over [`add_sequence`] for the sequence-tab UI:
/// builds a [`Sequence`] with one `width`×`height` format and appends it,
/// activating it if the project had none. Undoable via `AddSequence`'s inverse.
pub fn create_sequence(
    name: impl Into<String>,
    frame_rate: FrameRate,
    width: u32,
    height: u32,
) -> TimelineCmd {
    add_sequence(Sequence::new(name, frame_rate, width, height))
}

/// Duplicate a sequence (17 §G-17). Deep-clones it with fresh structural ids
/// (see [`Sequence::duplicate_with_fresh_ids`]) under a `"<name> copy"` name,
/// then returns the `AddSequence` command that inserts the copy. Undoable.
pub fn duplicate_sequence(p: &TimelineProject, id: SequenceId) -> Result<TimelineCmd, EditError> {
    let s = seq(p, id)?;
    let mut dup = s.duplicate_with_fresh_ids();
    dup.name = format!("{} copy", s.name);
    Ok(add_sequence(dup))
}

/// Rename a sequence (17 §G-17 tab rename). Undoable via the `RenameSequence`
/// command (old/new names swapped on inverse).
pub fn rename_sequence(
    p: &TimelineProject,
    id: SequenceId,
    new_name: impl Into<String>,
) -> Result<TimelineCmd, EditError> {
    let s = seq(p, id)?;
    Ok(TimelineCmd::RenameSequence {
        seq: id,
        old: s.name.clone(),
        new: new_name.into(),
    })
}

pub fn set_active_format(
    p: &TimelineProject,
    id: SequenceId,
    new: usize,
) -> Result<TimelineCmd, EditError> {
    let s = seq(p, id)?;
    if new >= s.formats.len() {
        return Err(EditError::IndexOutOfRange);
    }
    Ok(TimelineCmd::SetActiveFormat {
        seq: id,
        old: s.active_format,
        new,
    })
}

pub fn set_sequence_format(id: SequenceId, op: FormatOp) -> TimelineCmd {
    TimelineCmd::SetSequenceFormat { seq: id, op }
}

pub fn add_format(id: SequenceId, format: SequenceFormat) -> TimelineCmd {
    TimelineCmd::SetSequenceFormat {
        seq: id,
        op: FormatOp::Add { format },
    }
}

pub fn add_track(
    p: &TimelineProject,
    id: SequenceId,
    t: Track,
    index: Option<usize>,
) -> Result<TimelineCmd, EditError> {
    let s = seq(p, id)?;
    let kind = t.kind;
    let len = s.tracks_for(kind).len();
    Ok(TimelineCmd::AddTrack {
        seq: id,
        kind,
        index: index.unwrap_or(len).min(len),
        track: Box::new(t),
    })
}

pub fn remove_track(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
) -> Result<TimelineCmd, EditError> {
    let s = seq(p, id)?;
    for (kind, v) in [
        (TrackKind::Video, &s.video_tracks),
        (TrackKind::Audio, &s.audio_tracks),
    ] {
        if let Some(index) = v.iter().position(|t| t.id == track_id) {
            return Ok(TimelineCmd::RemoveTrack {
                seq: id,
                kind,
                index,
                track: Box::new(v[index].clone()),
            });
        }
    }
    Err(EditError::NoTrack(track_id))
}

/// Reorder a track within its lane (remove+add batched — the caller wraps the
/// two commands in a single `Command::Batch`).
pub fn reorder_track(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    new_index: usize,
) -> Result<Vec<TimelineCmd>, EditError> {
    let remove = remove_track(p, id, track_id)?;
    let (kind, t) = match &remove {
        TimelineCmd::RemoveTrack { kind, track, .. } => (*kind, (**track).clone()),
        _ => unreachable!(),
    };
    let add = TimelineCmd::AddTrack {
        seq: id,
        kind,
        index: new_index,
        track: Box::new(t),
    };
    Ok(vec![remove, add])
}

pub fn set_track_prop(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    new: TrackSettings,
) -> Result<TimelineCmd, EditError> {
    let s = seq(p, id)?;
    let t = track(s, track_id)?;
    Ok(TimelineCmd::SetTrackProp {
        seq: id,
        track: track_id,
        old: Box::new(TrackSettings::of(t)),
        new: Box::new(new),
    })
}

/// Toggle a track's sync-lock (14 §M-9). Data + toggle only — the
/// ripple-propagation across sync-locked tracks is a later GUI concern. Reuses
/// [`set_track_prop`] (a whole-[`TrackSettings`] diff) so undo/redo rides the
/// existing `SetTrackProp` path.
pub fn toggle_sync_lock(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
) -> Result<TimelineCmd, EditError> {
    let s = seq(p, id)?;
    let t = track(s, track_id)?;
    let mut new = TrackSettings::of(t);
    new.sync_lock = !new.sync_lock;
    set_track_prop(p, id, track_id, new)
}

// ── Clips ───────────────────────────────────────────────────────────────────

pub fn insert_clip(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    c: Clip,
) -> Result<TimelineCmd, EditError> {
    let s = seq(p, id)?;
    let t = track(s, track_id)?;
    if c.duration.0 <= 0 {
        return Err(EditError::NonPositiveDuration);
    }
    if overlaps_other(t, c.start, c.end(), None) {
        return Err(EditError::Overlap);
    }
    if let ClipSource::NestedSequence { sequence } = &c.source {
        if *sequence == id || nests_into(p, *sequence, id) {
            return Err(EditError::SequenceCycle);
        }
    }
    Ok(TimelineCmd::InsertClip {
        seq: id,
        track: track_id,
        clip: Box::new(c),
    })
}

/// Does sequence `outer` (transitively) contain a clip nesting `target`?
fn nests_into(p: &TimelineProject, outer: SequenceId, target: SequenceId) -> bool {
    let Some(s) = p.sequences.get(&outer) else {
        return false;
    };
    for t in s.video_tracks.iter().chain(s.audio_tracks.iter()) {
        for c in &t.clips {
            if let ClipSource::NestedSequence { sequence } = &c.source {
                if *sequence == target || nests_into(p, *sequence, target) {
                    return true;
                }
            }
        }
    }
    false
}

/// Create and insert a `ClipSource::Adjustment` clip spanning
/// `[start, start+duration)` on `track_id` (G-7 data half): an adjustment layer
/// whose effect stack / grade applies to the composite of every lower track
/// beneath its span. The clip carries no media — the create/insert is the model
/// op; the engine composites it (a separate lane). Undoable via the existing
/// [`insert_clip`] path (duration `> 0`, non-overlap and track existence
/// validated there).
pub fn add_adjustment_clip(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    start: Tick,
    duration: Tick,
) -> Result<TimelineCmd, EditError> {
    insert_clip(
        p,
        id,
        track_id,
        Clip::new(ClipSource::Adjustment, start, duration),
    )
}

/// Create and insert a `ClipSource::Text` title/graphics clip spanning
/// `[start, start+duration)` on `track_id` (G-12 data half): styled text on a
/// video track, rendered by the engine's text path (no render here). The clip's
/// `name` defaults to the text so the timeline shows a friendly label. Undoable
/// via the existing [`insert_clip`] path.
pub fn add_text_clip(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    start: Tick,
    duration: Tick,
    content: TextClipContent,
) -> Result<TimelineCmd, EditError> {
    let mut clip = Clip::new(ClipSource::Text { content }, start, duration);
    if let ClipSource::Text { content } = &clip.source {
        clip.name = content.text.clone();
    }
    insert_clip(p, id, track_id, clip)
}

pub fn remove_clip(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
) -> Result<TimelineCmd, EditError> {
    let s = seq(p, id)?;
    let t = track(s, track_id)?;
    let c = clip(t, clip_id)?;
    Ok(TimelineCmd::RemoveClip {
        seq: id,
        track: track_id,
        clip: Box::new(c.clone()),
    })
}

/// Shift several clips by one shared time `delta` and optional vertical
/// `track_delta` (same relative lane offset for every member of matching kind)
/// (04 §2.6 multi-select, 210 §5).
///
/// # Why this is a plural op and not a loop over [`move_clip`]
///
/// Two reasons, both load-bearing.
///
/// 1. **Collision.** `move_clip` validates against the *current* project, where
///    every other selected clip is still at its old position. Moving a
///    contiguous run right by 50 would have each clip refuse on the neighbour
///    it is about to vacate. Overlap here is therefore tested against
///    non-moving clips only — the moving set is displaced as a body.
/// 2. **Ordering.** `TimelineCmd::apply` debug-asserts `Sequence::validate()`
///    after *every* command and `Command::Batch` applies members one at a time,
///    so a fan-out whose intermediate states overlap panics in debug (the
///    hazard 194 K-A5 records). Commands are emitted rightmost-first when
///    moving later and leftmost-first when moving earlier, so each clip lands
///    in space its neighbour has already left and no intermediate state is
///    ever invalid. Vertical moves use the same rule on lane index: higher
///    source lanes first when `track_delta > 0`, lower first when negative.
///
/// `track_delta` is an index offset within the sequence's same-kind track list
/// (`Sequence::tracks_for`). It is applied **per kind independently**: a kind
/// whose members all have a valid destination takes the offset; a kind that
/// cannot (e.g. a single audio track when link partners ride along with a
/// video vertical drag) keeps every member of that kind on its own track and
/// applies **time only** — the same contract as single-clip
/// [`move_clip_cross_track`] / `expand_link_group_move` (partners never change
/// track). Relative lane spacing within a kind is never half-applied.
///
/// Returns commands in that safe application order. Empty when nothing would
/// change (zero time delta and no successful track reassignment). Rejects the
/// whole move on overlap / before-zero rather than applying part of it.
pub fn move_clips(
    p: &TimelineProject,
    id: SequenceId,
    moving: &[(TrackId, ClipId)],
    delta: Tick,
    track_delta: i32,
) -> Result<Vec<TimelineCmd>, EditError> {
    if moving.is_empty() || (delta.0 == 0 && track_delta == 0) {
        return Ok(Vec::new());
    }
    let s = seq(p, id)?;

    // Resolve geometry first (kind + lane index), then decide per-kind whether
    // track_delta is applicable so a linked A/V set does not fail the video
    // half when the audio lane list has no room for a vertical step.
    struct ResolvedClip {
        track_id: TrackId,
        clip_id: ClipId,
        old_start: Tick,
        duration: Tick,
        new_start: Tick,
        kind: TrackKind,
        src_idx: usize,
    }
    let mut resolved: Vec<ResolvedClip> = Vec::with_capacity(moving.len());
    for (track_id, clip_id) in moving {
        let t = track(s, *track_id)?;
        let c = clip(t, *clip_id)?;
        if t.locked {
            return Err(EditError::TrackLocked);
        }
        let new_start = Tick(c.start.0.checked_add(delta.0).ok_or(EditError::Overlap)?);
        if new_start.0 < 0 {
            return Err(EditError::Overlap);
        }
        let lanes = s.tracks_for(t.kind);
        let src_idx = lanes
            .iter()
            .position(|lane| lane.id == *track_id)
            .ok_or(EditError::NoTrack(*track_id))?;
        resolved.push(ResolvedClip {
            track_id: *track_id,
            clip_id: *clip_id,
            old_start: c.start,
            duration: c.duration,
            new_start,
            kind: t.kind,
            src_idx,
        });
    }

    // Per kind: apply track_delta only when every member of that kind has a
    // valid same-kind destination. Otherwise that kind is time-only.
    let mut kind_takes_vertical: std::collections::HashMap<TrackKind, bool> =
        std::collections::HashMap::new();
    if track_delta != 0 {
        for r in &resolved {
            let lanes = s.tracks_for(r.kind);
            let dest_idx = r.src_idx as i32 + track_delta;
            let ok = dest_idx >= 0
                && (dest_idx as usize) < lanes.len()
                && lanes[dest_idx as usize].kind == r.kind;
            kind_takes_vertical
                .entry(r.kind)
                .and_modify(|v| *v = *v && ok)
                .or_insert(ok);
        }
    }

    // (src_track, clip, old_start, duration, new_start, src_lane_idx, new_track, applies_vertical)
    let mut planned: Vec<(
        TrackId,
        ClipId,
        Tick,
        Tick,
        Tick,
        usize,
        Option<TrackId>,
        bool,
    )> = Vec::with_capacity(resolved.len());
    for r in &resolved {
        let apply_v =
            track_delta != 0 && kind_takes_vertical.get(&r.kind).copied().unwrap_or(false);
        let new_track = if apply_v {
            let lanes = s.tracks_for(r.kind);
            let dest = &lanes[(r.src_idx as i32 + track_delta) as usize];
            if dest.id == r.track_id {
                None
            } else {
                Some(dest.id)
            }
        } else {
            None
        };
        // Skip no-ops (zero time and same track) so a pure-vertical OOR kind
        // does not emit empty MoveClips.
        if delta.0 == 0 && new_track.is_none() {
            continue;
        }
        planned.push((
            r.track_id,
            r.clip_id,
            r.old_start,
            r.duration,
            r.new_start,
            r.src_idx,
            new_track,
            apply_v,
        ));
    }

    if planned.is_empty() {
        return Ok(Vec::new());
    }

    // Destination must be clear of every clip that is NOT part of the move.
    for (track_id, _, _, duration, new_start, _, new_track, _) in &planned {
        let dest_id = new_track.unwrap_or(*track_id);
        let t = track(s, dest_id)?;
        if t.locked {
            return Err(EditError::TrackLocked);
        }
        let end = Tick(
            new_start
                .0
                .checked_add(duration.0)
                .ok_or(EditError::Overlap)?,
        );
        let hits_stationary = t
            .clips
            .iter()
            .filter(|c| !moving.iter().any(|(_, m)| *m == c.id))
            .any(|c| c.start < end && *new_start < c.end());
        if hits_stationary {
            return Err(EditError::Overlap);
        }
    }

    // Safe application order: vacate lanes/times before peers land there.
    // Primary key is lane when any member moves vertically, else time.
    let any_vertical = planned.iter().any(|p| p.7);
    planned.sort_by(|a, b| {
        use std::cmp::Ordering;
        let track_ord = if any_vertical && track_delta > 0 {
            b.5.cmp(&a.5) // higher source lane first
        } else if any_vertical && track_delta < 0 {
            a.5.cmp(&b.5) // lower source lane first
        } else {
            Ordering::Equal
        };
        if track_ord != Ordering::Equal {
            return track_ord;
        }
        if delta.0 > 0 {
            b.2.cmp(&a.2) // trailing edge first
        } else if delta.0 < 0 {
            a.2.cmp(&b.2) // leading edge first
        } else {
            Ordering::Equal
        }
    });

    Ok(planned
        .into_iter()
        .map(
            |(track_id, clip_id, old_start, _, new_start, _, new_track, _)| TimelineCmd::MoveClip {
                seq: id,
                track: track_id,
                clip: clip_id,
                old_start,
                new_start,
                new_track,
            },
        )
        .collect())
}

/// Resolve selected clips and their linked partners once, refusing locked
/// participants. GUI and MCP share this policy so a blocked partner cannot
/// silently detach from its picture or sound.
pub fn linked_moving_set(
    p: &TimelineProject,
    id: SequenceId,
    selected: &[(TrackId, ClipId)],
) -> Result<Vec<(TrackId, ClipId)>, EditError> {
    let sequence = seq(p, id)?;
    let mut moving = Vec::new();
    for &(track_id, clip_id) in selected {
        let source = clip(track(sequence, track_id)?, clip_id)?;
        if !moving.contains(&(track_id, clip_id)) {
            moving.push((track_id, clip_id));
        }
        if let Some(group) = source.link_group {
            for lane in sequence.tracks() {
                for partner in lane.clips.iter().filter(|c| c.link_group == Some(group)) {
                    if !moving.contains(&(lane.id, partner.id)) {
                        moving.push((lane.id, partner.id));
                    }
                }
            }
        }
    }
    if moving
        .iter()
        .any(|(id, _)| sequence.track(*id).is_some_and(|track| track.locked))
    {
        return Err(EditError::TrackLocked);
    }
    Ok(moving)
}

/// Move one linked unit. Only the explicitly addressed clip changes lanes;
/// all linked partners receive the same time delta on their current lanes.
/// Cross-lane edits are staged to guarantee valid intermediate command order.
pub fn move_linked_clip(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    new_start: Tick,
    new_track: Option<TrackId>,
) -> Result<Vec<TimelineCmd>, EditError> {
    let sequence = seq(p, id)?;
    let source = clip(track(sequence, track_id)?, clip_id)?;
    let delta = Tick(
        new_start
            .0
            .checked_sub(source.start.0)
            .ok_or(EditError::Overlap)?,
    );
    let moving = linked_moving_set(p, id, &[(track_id, clip_id)])?;
    let Some(destination) = new_track.filter(|id| *id != track_id) else {
        return move_clips(p, id, &moving, delta, 0);
    };
    let destination_track = track(sequence, destination)?;
    if destination_track.locked {
        return Err(EditError::TrackLocked);
    }
    if destination_track.kind != track(sequence, track_id)?.kind {
        return Err(EditError::NoTrack(destination));
    }
    // Validate final destinations against both moving and stationary clips.
    let mut staged = p.clone();
    let mut pending = Vec::new();
    for &(lane, item) in &moving {
        let original = clip(track(sequence, lane)?, item)?;
        let start = Tick(
            original
                .start
                .0
                .checked_add(delta.0)
                .ok_or(EditError::Overlap)?,
        );
        if start.0 < 0 {
            return Err(EditError::Overlap);
        }
        pending.push((
            lane,
            item,
            start,
            if item == clip_id {
                Some(destination)
            } else {
                None
            },
        ));
    }
    // A valid order always vacates a destination before its next occupant
    // lands. If no move can advance, reject the entire unit.
    let mut commands = Vec::with_capacity(pending.len());
    while !pending.is_empty() {
        let candidate =
            pending
                .iter()
                .enumerate()
                .find_map(|(index, &(lane, item, start, dest))| {
                    move_clip_to_track(&staged, id, lane, item, start, dest)
                        .ok()
                        .map(|command| (index, command))
                });
        let Some((index, command)) = candidate else {
            return Err(EditError::Overlap);
        };
        let (lane, item, start, dest) = pending.remove(index);
        let sequence = staged.sequences.get_mut(&id).expect("validated sequence");
        let source = sequence.track_mut(lane).expect("validated track");
        let position = source
            .clips
            .iter()
            .position(|c| c.id == item)
            .expect("validated clip");
        let mut moved = source.clips.remove(position);
        moved.start = start;
        let target = sequence
            .track_mut(dest.unwrap_or(lane))
            .expect("validated destination");
        target.clips.push(moved);
        target.clips.sort_by_key(|clip| clip.start);
        commands.push(command);
    }
    Ok(commands)
}

/// Move a clip within its track. Signature preserved for existing callers;
/// cross-track moves use [`move_clip_to_track`].
pub fn move_clip(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    new_start: Tick,
) -> Result<TimelineCmd, EditError> {
    move_clip_to_track(p, id, track_id, clip_id, new_start, None)
}

/// Move a clip, optionally to a different track (`new_track = Some(dest)`).
/// The destination must be the same [`TrackKind`] and have room at `new_start`
/// (non-overlap enforced here). Inverse returns the clip to its original
/// track + position (lossless).
pub fn move_clip_to_track(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    new_start: Tick,
    new_track: Option<TrackId>,
) -> Result<TimelineCmd, EditError> {
    if new_start.0 < 0 {
        return Err(EditError::Overlap);
    }
    let s = seq(p, id)?;
    let src = track(s, track_id)?;
    let c = clip(src, clip_id)?;

    // Normalize: a `Some(same)` destination is really a same-track move.
    let dest_id = match new_track {
        Some(t) if t != track_id => Some(t),
        _ => None,
    };

    match dest_id {
        None => {
            if overlaps_other(src, new_start, new_start + c.duration, Some(clip_id)) {
                return Err(EditError::Overlap);
            }
        }
        Some(dest) => {
            let dst = track(s, dest)?;
            if dst.kind != src.kind {
                return Err(EditError::NoTrack(dest));
            }
            // On the destination the clip is new — nothing to ignore.
            if overlaps_other(dst, new_start, new_start + c.duration, None) {
                return Err(EditError::Overlap);
            }
        }
    }

    Ok(TimelineCmd::MoveClip {
        seq: id,
        track: track_id,
        clip: clip_id,
        old_start: c.start,
        new_start,
        new_track: dest_id,
    })
}

pub fn trim_clip(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    new: ClipTiming,
) -> Result<TimelineCmd, EditError> {
    if new.duration.0 <= 0 {
        return Err(EditError::NonPositiveDuration);
    }
    let s = seq(p, id)?;
    let t = track(s, track_id)?;
    let c = clip(t, clip_id)?;
    if overlaps_other(t, new.start, new.start + new.duration, Some(clip_id)) {
        return Err(EditError::Overlap);
    }
    Ok(TimelineCmd::TrimClip {
        seq: id,
        track: track_id,
        clip: clip_id,
        old: ClipTiming::of(c),
        new,
    })
}

/// K-A6 frame-accurate Edit Duration planner.
///
/// Applies a desired `start` / `duration` / `source_in` to one clip in a single
/// undo batch. When `ripple` is true and the timeline **start is unchanged**
/// but the **end moves**, later clips (and sync-locked siblings) shift via
/// [`ripple_trim`]; otherwise a non-ripple [`trim_clip`] (or pure
/// [`move_clip`] / [`slip_clip`] when only those fields change) is used.
///
/// Returns an empty `Ok(vec![])` when nothing would change.
pub fn edit_clip_timing(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    new: ClipTiming,
    ripple: bool,
) -> Result<Vec<TimelineCmd>, EditError> {
    if new.duration.0 <= 0 {
        return Err(EditError::NonPositiveDuration);
    }
    if new.start.0 < 0 || new.source_in.0 < 0 {
        return Err(EditError::Overlap);
    }
    let s = seq(p, id)?;
    let t = track(s, track_id)?;
    let c = clip(t, clip_id)?;
    let old = ClipTiming::of(c);
    if old == new {
        return Ok(Vec::new());
    }

    // Pure slip: timeline placement unchanged, only source_in.
    if new.start == old.start && new.duration == old.duration && new.source_in != old.source_in {
        return Ok(vec![slip_clip(p, id, track_id, clip_id, new.source_in)?]);
    }

    // Pure move: only timeline start changes.
    if new.start != old.start && new.duration == old.duration && new.source_in == old.source_in {
        return Ok(vec![move_clip(p, id, track_id, clip_id, new.start)?]);
    }

    // Ripple duration (end-edge) when the start is held fixed — the dialog's
    // "ripple" checkbox. Start-edge / position changes never ride this path.
    if ripple && new.start == old.start && new.duration != old.duration {
        let mut cmds = ripple_trim(
            p,
            id,
            track_id,
            clip_id,
            ClipEdge::End,
            new.start + new.duration,
        )?;
        if new.source_in != old.source_in {
            // Slip against the post-trim project is not available here (pure
            // planner). Emit SlipClip with the desired value — apply order is
            // ripple first then slip, and Slip only touches source_in.
            cmds.push(TimelineCmd::SlipClip {
                seq: id,
                track: track_id,
                clip: clip_id,
                old_source_in: old.source_in,
                new_source_in: new.source_in,
            });
        }
        return Ok(cmds);
    }

    // Non-ripple full timing write (position + duration + source_in).
    Ok(vec![trim_clip(p, id, track_id, clip_id, new)?])
}

pub fn split_clip(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    at: Tick,
) -> Result<TimelineCmd, EditError> {
    let s = seq(p, id)?;
    let t = track(s, track_id)?;
    let c = clip(t, clip_id)?;
    if at <= c.start || at >= c.end() {
        return Err(EditError::InvalidSplit);
    }
    Ok(TimelineCmd::SplitClip {
        seq: id,
        track: track_id,
        clip: clip_id,
        at,
        new_clip_id: ClipId::new(),
    })
}

/// Slip a clip's source in/out without moving it on the timeline.
pub fn slip_clip(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    new_source_in: Tick,
) -> Result<TimelineCmd, EditError> {
    let s = seq(p, id)?;
    let t = track(s, track_id)?;
    let c = clip(t, clip_id)?;
    Ok(TimelineCmd::SlipClip {
        seq: id,
        track: track_id,
        clip: clip_id,
        old_source_in: c.source_in,
        new_source_in,
    })
}

/// Ripple-delete a clip: remove it and shift every later clip on the track left
/// by its duration. Returns `[RemoveClip, RippleEdit, …]` for the caller to
/// batch — including one `RippleEdit` per sync-locked sibling track (14 §M-9).
pub fn ripple_delete(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
) -> Result<Vec<TimelineCmd>, EditError> {
    let s = seq(p, id)?;
    let t = track(s, track_id)?;
    let c = clip(t, clip_id)?;
    let shift = c.duration;
    let start = c.start;
    let mut changes = Vec::new();
    for other in &t.clips {
        if other.start >= start && other.id != clip_id {
            let old = ClipTiming::of(other);
            let new = ClipTiming {
                start: other.start - shift,
                ..old
            };
            changes.push((other.id, old, new));
        }
    }
    let mut cmds = vec![
        TimelineCmd::RemoveClip {
            seq: id,
            track: track_id,
            clip: Box::new(c.clone()),
        },
        TimelineCmd::RippleEdit {
            seq: id,
            track: track_id,
            changes,
        },
    ];
    // Sync-locked siblings ripple left by the same duration in the SAME batch
    // (14 §M-9). Point is the deleted clip's start; delta is negative duration.
    cmds.extend(expand_sync_lock_ripple(
        p,
        id,
        track_id,
        start,
        Tick(0) - shift,
    ));
    Ok(cmds)
}

/// Expand a ripple edit on `edited_track` to every OTHER sync-locked,
/// non-edit-locked track in the sequence (14 §M-9): each qualifying track gets
/// its own `RippleEdit` shifting every clip with `start >= point` by the
/// identical `delta`. A clip whose shifted start would go negative is dropped
/// from that track's batch (not the whole batch), mirroring the per-clip guard
/// the ripple ops apply to their own track. Returns one `TimelineCmd::RippleEdit`
/// per affected track, skipping any that would end up empty.
///
/// This is the single shared core of sync-lock propagation (14 §M-9) so every
/// caller — GUI `ops_bridge`, MCP handlers, future scripting — rides the same
/// implementation. The edited track itself always ripples regardless of its own
/// `sync_lock` bit (that is just what a ripple edit *is*); `sync_lock` only
/// governs whether *other* tracks tag along, and `locked` always wins over it.
pub fn expand_sync_lock_ripple(
    p: &TimelineProject,
    id: SequenceId,
    edited_track: TrackId,
    point: Tick,
    delta: Tick,
) -> Vec<TimelineCmd> {
    if delta.0 == 0 {
        return Vec::new();
    }
    let Some(s) = p.sequences.get(&id) else {
        return Vec::new();
    };
    s.tracks()
        .filter(|t| t.id != edited_track && t.sync_lock && !t.locked)
        .filter_map(|t| {
            let mut changes = Vec::new();
            for c in &t.clips {
                if c.start >= point {
                    let old = ClipTiming::of(c);
                    let shifted = ClipTiming {
                        start: c.start + delta,
                        ..old
                    };
                    if shifted.start.0 >= 0 {
                        changes.push((c.id, old, shifted));
                    }
                }
            }
            if changes.is_empty() {
                None
            } else {
                Some(TimelineCmd::RippleEdit {
                    seq: id,
                    track: t.id,
                    changes,
                })
            }
        })
        .collect()
}

/// Which edge of a clip a trim targets.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ClipEdge {
    Start,
    End,
}

/// Ripple-trim one edge of a clip to `new_boundary` (a timeline tick) and shift
/// every later clip on the same track by the resulting delta, closing the gap
/// (04 §2.4 "Shift + edge" ripple; distinct from [`ripple_delete`]).
///
/// - **End edge**: the clip's out-point moves to `new_boundary` (duration
///   `= new_boundary - start`); later clips shift by `new_boundary - old_end`.
/// - **Start edge**: the clip's in-point moves; the clip keeps its timeline
///   `start`, its `source_in` advances by the delta (`speed`-scaled) and its
///   duration shrinks/grows by the delta; later clips shift by `-delta` so the
///   left gap closes. `new_boundary` is the new in-point position on the timeline.
///
/// Returns a batch: the edited track's `RippleEdit` (trimmed clip + shifted
/// siblings) plus one `RippleEdit` per sync-locked sibling track (14 §M-9).
/// Invariant-safe by construction; inverted by the existing `RippleEdit` logic.
pub fn ripple_trim(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    edge: ClipEdge,
    new_boundary: Tick,
) -> Result<Vec<TimelineCmd>, EditError> {
    let s = seq(p, id)?;
    let t = track(s, track_id)?;
    let c = clip(t, clip_id)?;
    let old_start = c.start;
    let old_end = c.end();

    let (trimmed, shift) = match edge {
        ClipEdge::End => {
            if new_boundary <= old_start {
                return Err(EditError::NonPositiveDuration);
            }
            let new_dur = new_boundary - old_start;
            let delta = new_boundary - old_end;
            (
                ClipTiming {
                    start: old_start,
                    duration: new_dur,
                    source_in: c.source_in,
                },
                delta,
            )
        }
        ClipEdge::Start => {
            let delta = new_boundary - old_start;
            let new_dur = c.duration - delta;
            if new_dur.0 <= 0 {
                return Err(EditError::NonPositiveDuration);
            }
            let new_source_in = c.source_in + c.speed.source_delta(delta);
            if new_source_in.0 < 0 {
                // Cannot pull the in-point before the start of the source media.
                return Err(EditError::InvalidSplit);
            }
            (
                ClipTiming {
                    start: old_start,
                    duration: new_dur,
                    source_in: new_source_in,
                },
                // Later clips move opposite the in-point drag to close the gap.
                Tick(-delta.0),
            )
        }
    };

    let mut changes = vec![(clip_id, ClipTiming::of(c), trimmed)];
    for other in &t.clips {
        if other.id != clip_id && other.start >= old_end {
            let old = ClipTiming::of(other);
            changes.push((
                other.id,
                old,
                ClipTiming {
                    start: other.start + shift,
                    ..old
                },
            ));
        }
    }

    let mut cmds = vec![TimelineCmd::RippleEdit {
        seq: id,
        track: track_id,
        changes,
    }];
    // Downstream content on this track moves from `old_end`; sync-locked
    // siblings share that same point and delta (14 §M-9).
    cmds.extend(expand_sync_lock_ripple(p, id, track_id, old_end, shift));
    Ok(cmds)
}

/// Roll the shared edit point between two adjacent clips.
pub fn roll_edit(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    left: ClipId,
    right: ClipId,
    delta: Tick,
) -> Result<TimelineCmd, EditError> {
    let s = seq(p, id)?;
    let t = track(s, track_id)?;
    if t.locked {
        return Err(EditError::TrackLocked);
    }
    let l = clip(t, left)?;
    let r = clip(t, right)?;
    let left_end = l
        .start
        .0
        .checked_add(l.duration.0)
        .ok_or(EditError::InvalidSplit)?;
    if left == right || left_end != r.start.0 {
        return Err(EditError::InvalidSplit);
    }
    let l_old = ClipTiming::of(l);
    let r_old = ClipTiming::of(r);
    let l_new = ClipTiming {
        duration: Tick(
            l.duration
                .0
                .checked_add(delta.0)
                .ok_or(EditError::InvalidSplit)?,
        ),
        ..l_old
    };
    let r_new = ClipTiming {
        start: Tick(
            r.start
                .0
                .checked_add(delta.0)
                .ok_or(EditError::InvalidSplit)?,
        ),
        duration: Tick(
            r.duration
                .0
                .checked_sub(delta.0)
                .ok_or(EditError::InvalidSplit)?,
        ),
        source_in: Tick(
            r.source_in
                .0
                .checked_add(checked_source_delta(&r.speed, delta)?.0)
                .ok_or(EditError::InvalidSplit)?,
        ),
    };
    if l_new.duration.0 <= 0 || r_new.duration.0 <= 0 {
        return Err(EditError::NonPositiveDuration);
    }
    validate_source_timing(p, l, l_new)?;
    validate_source_timing(p, r, r_new)?;
    Ok(TimelineCmd::RollEdit {
        seq: id,
        track: track_id,
        changes: vec![(left, l_old, l_new), (right, r_old, r_new)],
    })
}

/// Checked source mapping for precision edits; the legacy low-level mapper
/// returns an i64 and cannot report an out-of-range rational product.
pub(super) fn checked_source_delta(speed: &SpeedMap, delta: Tick) -> Result<Tick, EditError> {
    match speed {
        SpeedMap::Constant(ratio) => {
            if ratio.den == 0 {
                return Err(EditError::InvalidSpeedMap);
            }
            let mapped = delta.0 as i128 * ratio.num as i128 / ratio.den as i128;
            i64::try_from(mapped)
                .map(Tick)
                .map_err(|_| EditError::InvalidSplit)
        }
        SpeedMap::Keyframed { .. } => {
            let mapped = speed.source_delta_f64(delta);
            if !mapped.is_finite() || mapped < i64::MIN as f64 || mapped >= i64::MAX as f64 {
                return Err(EditError::InvalidSplit);
            }
            Ok(speed.source_delta(delta))
        }
    }
}

/// Validate available source bounds without requiring online media. Missing
/// probe information leaves only non-negative/finite mapping enforceable.
pub(super) fn validate_source_timing(
    project: &TimelineProject,
    clip: &Clip,
    timing: ClipTiming,
) -> Result<(), EditError> {
    if timing.start < Tick::ZERO || timing.source_in < Tick::ZERO || timing.duration <= Tick::ZERO {
        return Err(EditError::InvalidSplit);
    }
    timing
        .start
        .0
        .checked_add(timing.duration.0)
        .ok_or(EditError::InvalidSplit)?;
    clip.speed
        .validate_for_duration(clip.duration)
        .map_err(|_| EditError::InvalidSpeedMap)?;
    let (low, high) = clip.speed.source_delta_range(timing.duration);
    let lo = timing.source_in.0 as f64 + low;
    let hi = timing.source_in.0 as f64 + high;
    if !lo.is_finite() || !hi.is_finite() || lo < 0.0 || hi >= i64::MAX as f64 {
        return Err(EditError::InvalidSplit);
    }
    let bounds = match clip.source {
        ClipSource::Asset { asset } | ClipSource::Vector { asset } => {
            project.media.assets.get(&asset).and_then(|asset| {
                asset.subclip_range.or_else(|| {
                    asset
                        .probe
                        .as_ref()
                        .map(|probe| (Tick::ZERO, probe.duration))
                })
            })
        }
        ClipSource::NestedSequence { sequence } => project
            .sequences
            .get(&sequence)
            .map(|sequence| (Tick::ZERO, sequence.content_end())),
        _ => None,
    };
    if bounds.is_some_and(|(start, end)| lo < start.0 as f64 || hi > end.0 as f64) {
        return Err(EditError::InvalidSplit);
    }
    Ok(())
}

/// Slide a clip over its neighbors, keeping total span.
pub fn slide_clip(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    delta: Tick,
) -> Result<TimelineCmd, EditError> {
    let s = seq(p, id)?;
    let t = track(s, track_id)?;
    let pos = t
        .clips
        .iter()
        .position(|c| c.id == clip_id)
        .ok_or(EditError::NoClip(clip_id))?;
    let cur = &t.clips[pos];
    let mut changes = vec![(
        clip_id,
        ClipTiming::of(cur),
        ClipTiming {
            start: cur.start + delta,
            ..ClipTiming::of(cur)
        },
    )];
    if pos > 0 {
        let prev = &t.clips[pos - 1];
        let new = ClipTiming {
            duration: prev.duration + delta,
            ..ClipTiming::of(prev)
        };
        if new.duration.0 <= 0 {
            return Err(EditError::NonPositiveDuration);
        }
        changes.push((prev.id, ClipTiming::of(prev), new));
    }
    if pos + 1 < t.clips.len() {
        let next = &t.clips[pos + 1];
        let new = ClipTiming {
            start: next.start + delta,
            duration: next.duration - delta,
            source_in: next.source_in + delta,
        };
        if new.duration.0 <= 0 {
            return Err(EditError::NonPositiveDuration);
        }
        changes.push((next.id, ClipTiming::of(next), new));
    }
    Ok(TimelineCmd::SlideClip {
        seq: id,
        track: track_id,
        changes,
    })
}

/// Universal clip property change (mirrors `UpdateNode`).
pub fn set_clip_prop(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    mut new: Clip,
) -> Result<TimelineCmd, EditError> {
    let s = seq(p, id)?;
    let t = track(s, track_id)?;
    let old = clip(t, new.id)?.clone();
    if t.locked {
        return Err(EditError::TrackLocked);
    }
    if new.duration.0 <= 0 {
        return Err(EditError::NonPositiveDuration);
    }
    new.speed.normalize();
    new.speed
        .validate_for_duration(new.duration)
        .map_err(|_| EditError::InvalidSpeedMap)?;
    if overlaps_other(t, new.start, new.end(), Some(new.id)) {
        return Err(EditError::Overlap);
    }
    // A generated stabilization path is tied to the recipe and source range.
    // Any user-visible clip/source change must force a fresh generation rather
    // than leaving a stale analysis key attached to the edited clip.
    if old.source != new.source
        || old.source_in != new.source_in
        || old.duration != new.duration
        || old.speed != new.speed
        || old.stabilization != new.stabilization
    {
        if let Some(spec) = new.stabilization.as_mut() {
            spec.analysis_key = None;
        }
    }
    Ok(TimelineCmd::SetClipProp {
        seq: id,
        track: track_id,
        old: Box::new(old),
        new: Box::new(new),
    })
}

/// Replace one clip's complete speed map with validation and undo support.
pub fn set_speed_map(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    speed: SpeedMap,
) -> Result<TimelineCmd, EditError> {
    let mut new = clip(track(seq(p, id)?, track_id)?, clip_id)?.clone();
    new.speed = speed;
    set_clip_prop(p, id, track_id, new)
}

/// Insert or replace a speed key at its clip-relative tick.
pub fn upsert_speed_key(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    key: SpeedKey,
) -> Result<TimelineCmd, EditError> {
    let current = clip(track(seq(p, id)?, track_id)?, clip_id)?;
    let mut keys = match &current.speed {
        SpeedMap::Constant(ratio) => vec![SpeedKey::new(Tick::ZERO, *ratio)],
        SpeedMap::Keyframed { keys } => keys.clone(),
    };
    if let Some(existing) = keys.iter_mut().find(|existing| existing.at == key.at) {
        *existing = key;
    } else {
        keys.push(key);
    }
    keys.sort_by_key(|entry| entry.at.0);
    set_speed_map(p, id, track_id, clip_id, SpeedMap::Keyframed { keys })
}

/// Move an existing speed key to a unique tick and replace its ratio.
pub fn move_speed_key(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    old_at: Tick,
    new_at: Tick,
    ratio: Ratio,
) -> Result<TimelineCmd, EditError> {
    let current = clip(track(seq(p, id)?, track_id)?, clip_id)?;
    let SpeedMap::Keyframed { keys } = &current.speed else {
        return Err(EditError::InvalidSpeedMap);
    };
    let mut keys = keys.clone();
    let Some(index) = keys.iter().position(|key| key.at == old_at) else {
        return Err(EditError::InvalidSpeedMap);
    };
    if new_at != old_at && keys.iter().any(|key| key.at == new_at) {
        return Err(EditError::InvalidSpeedMap);
    }
    keys[index].at = new_at;
    keys[index].ratio = ratio;
    keys.sort_by_key(|entry| entry.at.0);
    set_speed_map(p, id, track_id, clip_id, SpeedMap::Keyframed { keys })
}

/// Remove a speed key, preserving a keyframed map so callers can explicitly
/// distinguish an empty curve from constant speed until they choose to clean it.
pub fn remove_speed_key(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    at: Tick,
) -> Result<TimelineCmd, EditError> {
    let current = clip(track(seq(p, id)?, track_id)?, clip_id)?;
    let SpeedMap::Keyframed { keys } = &current.speed else {
        return Err(EditError::InvalidSpeedMap);
    };
    let mut keys = keys.clone();
    let before = keys.len();
    keys.retain(|key| key.at != at);
    if keys.len() == before {
        return Err(EditError::InvalidSpeedMap);
    }
    set_speed_map(p, id, track_id, clip_id, SpeedMap::Keyframed { keys })
}

/// Change the interpolation leaving a speed key.
pub fn set_speed_key_interp(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    at: Tick,
    interp: Interp,
) -> Result<TimelineCmd, EditError> {
    let current = clip(track(seq(p, id)?, track_id)?, clip_id)?;
    let SpeedMap::Keyframed { keys } = &current.speed else {
        return Err(EditError::InvalidSpeedMap);
    };
    let mut keys = keys.clone();
    let Some(key) = keys.iter_mut().find(|key| key.at == at) else {
        return Err(EditError::InvalidSpeedMap);
    };
    key.interp = interp;
    set_speed_map(p, id, track_id, clip_id, SpeedMap::Keyframed { keys })
}

/// **Replace With Clip / Replace Edit** (G-5, Premiere): swap a clip's SOURCE
/// (and, optionally, its `source_in`) in place — keeping the clip's timeline
/// `start`, `duration`, `speed`, transform, effect stack, grade, transitions,
/// audio, reframe, color label and link group untouched. The shot changes;
/// everything the editor built around the slot stays. Undoable via the existing
/// [`set_clip_prop`] whole-clip diff (one undo step).
///
/// Rejects a nested-sequence source that would cycle (mirrors [`insert_clip`])
/// and a replacement into `Adjustment` on a clip that still carries a
/// composition (07 §6.6). Duration is unchanged, so a shorter new source is
/// held to the slot (Premiere trims-to-slot; the source is sampled from
/// `new_source_in` for the slot's length by the engine).
pub fn replace_clip_source(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    new_source: ClipSource,
    new_source_in: Option<Tick>,
) -> Result<TimelineCmd, EditError> {
    let s = seq(p, id)?;
    let t = track(s, track_id)?;
    let mut new = clip(t, clip_id)?.clone();

    if let ClipSource::NestedSequence { sequence } = &new_source {
        if *sequence == id || nests_into(p, *sequence, id) {
            return Err(EditError::SequenceCycle);
        }
    }
    if matches!(new_source, ClipSource::Adjustment) && new.composition.is_some() {
        return Err(EditError::CompositionOnAdjustment);
    }

    new.source = new_source;
    if let Some(si) = new_source_in {
        new.source_in = si;
    }
    set_clip_prop(p, id, track_id, new)
}

// ── Color labels & linking (14 §M-1/M-2, gaps #7/#8's data half) ───────────

/// Set (or clear, with `None`) a clip's organizational color label. Reuses
/// [`set_clip_prop`] — a whole-clip diff — since a label change never
/// affects timing/overlap; `EditError::Overlap`/`NonPositiveDuration` from
/// that path can't actually trigger here.
pub fn set_clip_color_label(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    label: Option<u8>,
) -> Result<TimelineCmd, EditError> {
    let s = seq(p, id)?;
    let t = track(s, track_id)?;
    let mut new = clip(t, clip_id)?.clone();
    new.color_label = label;
    set_clip_prop(p, id, track_id, new)
}

/// Link two clips (e.g. a split A/V pair) into the same link group so a
/// future move can carry them together (14 §M-2; the GUI drag-together
/// wiring is a later story — this just establishes the group). If either
/// clip already belongs to a group, that group is reused for both; otherwise
/// a fresh [`LinkGroupId`] is minted. Returns `[SetClipProp, SetClipProp]`
/// for the caller to wrap in one `Command::Batch` (one undo step).
pub fn link_clips(
    p: &TimelineProject,
    id: SequenceId,
    track_a: TrackId,
    clip_a: ClipId,
    track_b: TrackId,
    clip_b: ClipId,
) -> Result<Vec<TimelineCmd>, EditError> {
    let s = seq(p, id)?;
    let a = clip(track(s, track_a)?, clip_a)?.clone();
    let b = clip(track(s, track_b)?, clip_b)?.clone();
    let group = a
        .link_group
        .or(b.link_group)
        .unwrap_or_else(LinkGroupId::new);

    let mut new_a = a;
    new_a.link_group = Some(group);
    let mut new_b = b;
    new_b.link_group = Some(group);

    Ok(vec![
        set_clip_prop(p, id, track_a, new_a)?,
        set_clip_prop(p, id, track_b, new_b)?,
    ])
}

/// Remove `clip_id` from its link group (a no-op edit — still `Ok` — if it
/// wasn't linked, so callers don't need a special case).
pub fn unlink_clip(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
) -> Result<TimelineCmd, EditError> {
    let s = seq(p, id)?;
    let t = track(s, track_id)?;
    let mut new = clip(t, clip_id)?.clone();
    new.link_group = None;
    set_clip_prop(p, id, track_id, new)
}

/// K-A13: detach linked A/V partners of `clip_id` into independent clips
/// (clear every member's `link_group`). One undo unit when the caller wraps
/// the returned vec as a batch. Empty vec when the clip was not linked.
///
/// Does **not** move audio to another track — it only breaks the linkage so
/// subsequent trims/moves can diverge. That is the routine "Split Audio"
/// half of Kdenlive's verb (restore link is `link_clips`).
pub fn split_av_link(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
) -> Result<Vec<TimelineCmd>, EditError> {
    let s = seq(p, id)?;
    let t = track(s, track_id)?;
    let c = clip(t, clip_id)?;
    let Some(group) = c.link_group else {
        return Ok(Vec::new());
    };
    // Locate every member (video + audio tracks) and unlink each.
    let mut cmds = Vec::new();
    for track in s.video_tracks.iter().chain(s.audio_tracks.iter()) {
        for member in track.clips.iter().filter(|c| c.link_group == Some(group)) {
            cmds.push(unlink_clip(p, id, track.id, member.id)?);
        }
    }
    Ok(cmds)
}

/// K-A12: set the sequence start timecode offset (display label origin).
pub fn set_sequence_start_timecode(
    p: &TimelineProject,
    id: SequenceId,
    new: Tick,
) -> Result<TimelineCmd, EditError> {
    let s = seq(p, id)?;
    Ok(TimelineCmd::SetSequenceStartTimecode {
        seq: id,
        old: s.start_timecode,
        new,
    })
}

// ── K-A3 Spacer / space operations ──────────────────────────────────────────

/// Shift every clip with `start >= at` by `delta` on every **unlocked** track
/// in the sequence (K-A3). Positive `delta` opens space; negative closes it.
///
/// One `RippleEdit` per track that has at least one moved clip — the whole
/// vec is one undo unit when the caller wraps it as `Command::Batch`.
/// Locked tracks are skipped (they never move under a spacer). Empty result
/// when `delta == 0` or nothing sits at/after `at`.
///
/// Overlap is impossible when shifting right. When shifting left, a clip that
/// would land with `start < 0` is left out of that track's change list (same
/// guard as [`expand_sync_lock_ripple`]); callers that need a hard refuse
/// should pre-check via [`space_available_before`].
pub fn shift_after(
    p: &TimelineProject,
    id: SequenceId,
    at: Tick,
    delta: Tick,
) -> Result<Vec<TimelineCmd>, EditError> {
    if delta.0 == 0 {
        return Ok(Vec::new());
    }
    let s = seq(p, id)?;
    let mut cmds = Vec::new();
    for t in s.tracks().filter(|t| !t.locked) {
        let mut changes = Vec::new();
        for c in &t.clips {
            if c.start >= at {
                let old = ClipTiming::of(c);
                let shifted = ClipTiming {
                    start: c.start + delta,
                    ..old
                };
                if shifted.start.0 >= 0 {
                    changes.push((c.id, old, shifted));
                }
            }
        }
        if !changes.is_empty() {
            cmds.push(TimelineCmd::RippleEdit {
                seq: id,
                track: t.id,
                changes,
            });
        }
    }
    // Sequence markers at/after `at` ride with the space (content stays under
    // the cut). Clip-relative markers are untouched (they live on the clip).
    for m in &s.markers {
        if m.at >= at {
            let mut new_m = m.clone();
            let next = m.at + delta;
            if next.0 >= 0 {
                new_m.at = next;
                cmds.push(TimelineCmd::SetMarker {
                    seq: id,
                    id: m.id,
                    old: m.clone(),
                    new: new_m,
                });
            }
        }
    }
    Ok(cmds)
}

/// How much pure gap (no clip material) sits immediately after `at` on every
/// unlocked track that has content at or after `at`. `None` means unbounded
/// (no later clip on any unlocked track). Used by remove-space to refuse a
/// request larger than the shared gap.
pub fn space_available_after(p: &TimelineProject, id: SequenceId, at: Tick) -> Option<Tick> {
    let s = p.sequences.get(&id)?;
    let mut min_gap: Option<i64> = None;
    let mut saw_later = false;
    for t in s.tracks().filter(|t| !t.locked) {
        // First clip with start > at (strictly after the point).
        let next_start = t
            .clips
            .iter()
            .filter(|c| c.start > at)
            .map(|c| c.start.0)
            .min();
        // A clip covering `at` means zero available gap at that point.
        let covering = t.clips.iter().any(|c| c.start <= at && c.end() > at);
        if covering {
            return Some(Tick(0));
        }
        if let Some(ns) = next_start {
            saw_later = true;
            let gap = ns - at.0;
            min_gap = Some(min_gap.map_or(gap, |g| g.min(gap)));
        }
    }
    if saw_later {
        min_gap.map(Tick)
    } else {
        None // nothing after → unbounded
    }
}

/// K-A3 Insert Space: open `amount` of empty timeline at `at` on every
/// unlocked track (shifts every later clip right). `amount` must be > 0.
pub fn insert_space(
    p: &TimelineProject,
    id: SequenceId,
    at: Tick,
    amount: Tick,
) -> Result<Vec<TimelineCmd>, EditError> {
    if amount.0 <= 0 {
        return Err(EditError::NonPositiveDuration);
    }
    // validate sequence exists
    let _ = seq(p, id)?;
    shift_after(p, id, at, amount)
}

/// K-A3 Remove Space: close up to `amount` of empty gap at `at` across all
/// unlocked tracks (shifts later clips left). Refuses when a clip covers
/// `at`, or when the shared free gap is smaller than `amount`.
pub fn remove_space(
    p: &TimelineProject,
    id: SequenceId,
    at: Tick,
    amount: Tick,
) -> Result<Vec<TimelineCmd>, EditError> {
    if amount.0 <= 0 {
        return Err(EditError::NonPositiveDuration);
    }
    let _ = seq(p, id)?;
    match space_available_after(p, id, at) {
        // No free gap (clip covers `at`, or gap shorter than requested).
        Some(avail) if avail.0 < amount.0 => return Err(EditError::Overlap),
        // Nothing after the point → trailing void; removing it is a no-op.
        None => return Ok(Vec::new()),
        Some(_) => {}
    }
    shift_after(p, id, at + amount, Tick(0) - amount)
}

/// K-A3 Remove All Spaces After Cursor: pack every unlocked track so that
/// from `at` onward clips are contiguous (no internal gaps). Each track is
/// packed independently — different tracks may have different original gaps.
/// One undo unit via a batch of per-track `RippleEdit`s.
pub fn remove_all_spaces_after(
    p: &TimelineProject,
    id: SequenceId,
    at: Tick,
) -> Result<Vec<TimelineCmd>, EditError> {
    let s = seq(p, id)?;
    let mut cmds = Vec::new();
    for t in s.tracks().filter(|t| !t.locked) {
        // Clips that start at/after `at`, sorted by start.
        let mut later: Vec<&Clip> = t.clips.iter().filter(|c| c.start >= at).collect();
        if later.len() < 2 {
            // 0–1 clips: nothing to pack (a single clip can't have a gap).
            // Leading gap between `at` and the first clip: pull it left to `at`.
            if let Some(first) = later.first() {
                if first.start > at {
                    let old = ClipTiming::of(first);
                    let new = ClipTiming { start: at, ..old };
                    cmds.push(TimelineCmd::RippleEdit {
                        seq: id,
                        track: t.id,
                        changes: vec![(first.id, old, new)],
                    });
                }
            }
            continue;
        }
        later.sort_by_key(|c| c.start);
        let mut cursor = later[0].start.max(at);
        // If first clip starts after `at`, pull it to `at`.
        let mut changes = Vec::new();
        for (i, c) in later.iter().enumerate() {
            let target = if i == 0 { at.max(Tick(0)) } else { cursor };
            if c.start != target {
                let old = ClipTiming::of(c);
                changes.push((
                    c.id,
                    old,
                    ClipTiming {
                        start: target,
                        ..old
                    },
                ));
            }
            cursor = target + c.duration;
        }
        if !changes.is_empty() {
            cmds.push(TimelineCmd::RippleEdit {
                seq: id,
                track: t.id,
                changes,
            });
        }
    }
    Ok(cmds)
}

/// K-A3 Remove All Clips After Cursor: delete every clip on every unlocked
/// track whose `start >= at`. One undo unit (batch of `RemoveClip`s).
pub fn remove_clips_after(
    p: &TimelineProject,
    id: SequenceId,
    at: Tick,
) -> Result<Vec<TimelineCmd>, EditError> {
    let s = seq(p, id)?;
    let mut cmds = Vec::new();
    for t in s.tracks().filter(|t| !t.locked) {
        for c in t.clips.iter().filter(|c| c.start >= at) {
            cmds.push(TimelineCmd::RemoveClip {
                seq: id,
                track: t.id,
                clip: Box::new(c.clone()),
            });
        }
    }
    Ok(cmds)
}

/// K-B14 Freeze frame: hold a chosen source frame for the clip's entire
/// timeline duration.
///
/// Resolves the source tick visible at clip-relative `at` (clamped into
/// `[0, duration)` so an out-of-range playhead still freezes the nearest
/// edge frame), then writes:
///
/// - `source_in` ← that source tick
/// - `speed` ← `SpeedMap::Constant(Ratio { num: 0, den: 1 })`
///
/// Zero-rate integration already yields a zero source delta
/// ([`SpeedMap::source_delta`]), and the compile path treats `ratio.num == 0`
/// as an unbounded handle (a freeze never "runs out" of source). G-11's audio
/// contract maps zero speed to silence, not a held DC sample.
///
/// One undo unit via [`set_clip_prop`]. Returns `Ok(None)` when the clip is
/// already frozen at the same source frame (no history entry). Generators
/// (solid / text / adjustment) freeze the same way — their source is
/// time-invariant, but the speed flag still marks the clip as a freeze for
/// handle math and UI.
///
/// This is *not* a new effect kind: a zero-rate `SpeedMap` is the model
/// (26 §10 K-B14). Mid-clip freeze-then-resume is expressible as a keyframed
/// ramp with a zero-rate segment via `set_clip_speed`; this verb freezes the
/// whole slot.
pub fn freeze_frame(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    at: Tick,
) -> Result<Option<TimelineCmd>, EditError> {
    let s = seq(p, id)?;
    let t = track(s, track_id)?;
    let c = clip(t, clip_id)?;
    if c.duration.0 <= 0 {
        return Err(EditError::NonPositiveDuration);
    }
    // Clamp into the half-open clip range so freeze-at-playhead never fails
    // when the head sits on the out-point.
    let at_rel = Tick(at.0.clamp(0, (c.duration.0 - 1).max(0)));
    let freeze_source = c.source_in + c.speed.source_delta(at_rel);
    let already = matches!(
        &c.speed,
        SpeedMap::Constant(r) if r.num == 0 && r.den > 0
    ) && c.source_in == freeze_source;
    if already {
        return Ok(None);
    }
    let mut new = c.clone();
    new.source_in = freeze_source;
    new.speed = SpeedMap::Constant(Ratio::new(0, 1));
    Ok(Some(set_clip_prop(p, id, track_id, new)?))
}

/// All clip ids across the project sharing `group` (14 §M-2 helper — e.g. to
/// resolve an A/V pair to move/select as a unit). Pure read; empty when the
/// group has no members.
pub fn clips_in_link_group(p: &TimelineProject, group: LinkGroupId) -> Vec<ClipId> {
    p.sequences
        .values()
        .flat_map(|s| s.video_tracks.iter().chain(s.audio_tracks.iter()))
        .flat_map(|t| t.clips.iter())
        .filter(|c| c.link_group == Some(group))
        .map(|c| c.id)
        .collect()
}

// ── Auto-reframe (14 §9/CAP-012) ────────────────────────────────────────────

/// Compute a center-fill ("cover") reframe [`ClipTransform`] for retargeting
/// a clip authored against `content` onto `target`: scale isotropically (so
/// the aspect ratio doesn't distort) by the *larger* of the two per-axis
/// ratios, so `content`'s box fully covers `target`'s box, then leave
/// position/anchor/rotation at their centered defaults — mirrors an "auto
/// reframe: center-fill" preset in reference NLEs. `x`/`y`/`anchor_x`/
/// `anchor_y`/`rotation` are `0.0` and `opacity` is `1.0`
/// ([`ClipTransform::default`]); only `scale_x`/`scale_y` are set.
///
/// **Content-box assumption**: `Clip` doesn't track a source's intrinsic
/// pixel size (asset probing is out of this story's scope). The documented,
/// tested convention: `content` is the sequence's format at index 0 — the
/// sequence's original/native format, which is exactly what a clip's
/// un-reframed (identity-scale) `transform` already fills at authoring time,
/// so "the clip's transform baseline" and "the sequence's base format dims"
/// name the same box. `target` is the format being reframed into
/// (`sequence.formats[format_index]`); the result is stored via
/// [`set_clip_reframe`].
pub fn fit_clip_to_format(content: &SequenceFormat, target: &SequenceFormat) -> ClipTransform {
    let content_w = content.width.max(1) as f64;
    let content_h = content.height.max(1) as f64;
    let target_w = target.width.max(1) as f64;
    let target_h = target.height.max(1) as f64;
    // Cover fit: the larger of the two per-axis ratios so both target
    // dimensions are fully covered (may crop content; never letterboxes).
    let scale = (target_w / content_w).max(target_h / content_h);
    ClipTransform {
        scale_x: scale,
        scale_y: scale,
        ..ClipTransform::default()
    }
}

/// Set (or clear, with `transform = None`) a clip's per-`SequenceFormat`
/// static reframe override (CAP-012, `Clip.reframe[format_index]`). Mirrors
/// the existing GUI (`app/reframe.rs::commit_reframe`) and MCP
/// (`set_clip_prop`'s `reframe` arg) `reframe.insert`/`remove` pattern as a
/// first-class, independently testable op.
pub fn set_clip_reframe(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    format_index: usize,
    transform: Option<ClipTransform>,
) -> Result<TimelineCmd, EditError> {
    let s = seq(p, id)?;
    let t = track(s, track_id)?;
    let mut new = clip(t, clip_id)?.clone();
    match transform {
        Some(xf) => {
            new.reframe.insert(format_index, xf);
        }
        None => {
            new.reframe.remove(&format_index);
        }
    }
    set_clip_prop(p, id, track_id, new)
}

// ── Keyframes (generic over any AnimProps target) ───────────────────────────

fn existing_kf(
    p: &TimelineProject,
    target: &AnimTarget,
    path: &PropPath,
    at: Tick,
) -> Option<Keyframe> {
    read_tracks(p, target)?
        .iter()
        .find(|l| &l.property == path)
        .and_then(|l| l.keyframes.iter().find(|k| k.at == at).copied())
}

fn read_tracks<'a>(p: &'a TimelineProject, target: &AnimTarget) -> Option<&'a Vec<PropertyTrack>> {
    let find_clip = |cid: ClipId| -> Option<&Clip> {
        p.sequences
            .values()
            .flat_map(|s| s.video_tracks.iter().chain(s.audio_tracks.iter()))
            .flat_map(|t| t.clips.iter())
            .find(|c| c.id == cid)
    };
    let find_track = |tid: TrackId| -> Option<&Track> {
        p.sequences
            .values()
            .flat_map(|s| s.video_tracks.iter().chain(s.audio_tracks.iter()))
            .find(|t| t.id == tid)
    };
    match target {
        AnimTarget::ClipTransform { clip } => find_clip(*clip).map(|c| &c.transform.tracks),
        AnimTarget::ClipEffect { clip, effect_index } => find_clip(*clip)?
            .effects
            .get(*effect_index)
            .map(|e| &e.params.tracks),
        AnimTarget::GradeOp { clip, op } => find_clip(*clip)?
            .grade
            .as_ref()?
            .ops
            .iter()
            .find(|o| o.id == *op)
            .map(|o| &o.params.tracks),
        AnimTarget::ClipAudio { clip } => {
            find_clip(*clip)?.audio.as_ref().map(|a| &a.params.tracks)
        }
        AnimTarget::TrackAudio { track } => {
            find_track(*track)?.audio.as_ref().map(|a| &a.params.tracks)
        }
        AnimTarget::MasterBus { seq } => {
            p.sequences.get(seq).map(|s| &s.audio_master.params.tracks)
        }
        AnimTarget::AudioFx { owner, index } => match owner {
            FxOwner::Track(t) => find_track(*t)?
                .audio
                .as_ref()?
                .fx_chain
                .get(*index)
                .map(|u| &u.params.tracks),
            FxOwner::Master => {
                let sid = p.active_sequence?;
                p.sequences
                    .get(&sid)?
                    .audio_master
                    .fx_chain
                    .get(*index)
                    .map(|u| &u.params.tracks)
            }
        },
    }
}

pub fn set_keyframe(
    p: &TimelineProject,
    target: AnimTarget,
    path: PropPath,
    kf: Keyframe,
) -> TimelineCmd {
    let old = existing_kf(p, &target, &path, kf.at);
    TimelineCmd::SetKeyframe {
        target,
        path,
        old,
        new: kf,
    }
}

pub fn remove_keyframe(
    p: &TimelineProject,
    target: AnimTarget,
    path: PropPath,
    at: Tick,
) -> Result<TimelineCmd, EditError> {
    let kf = existing_kf(p, &target, &path, at).ok_or(EditError::IndexOutOfRange)?;
    Ok(TimelineCmd::RemoveKeyframe {
        target,
        path,
        keyframe: kf,
    })
}

pub fn set_keyframe_interp(
    p: &TimelineProject,
    target: AnimTarget,
    path: PropPath,
    at: Tick,
    new: Interp,
) -> Result<TimelineCmd, EditError> {
    let kf = existing_kf(p, &target, &path, at).ok_or(EditError::IndexOutOfRange)?;
    Ok(TimelineCmd::SetKeyframeInterp {
        target,
        path,
        at,
        old: kf.interp,
        new,
    })
}

// ── Keyframe interchange (26 §10 K-B11) ─────────────────────────────────────
//
// Copy a set of property tracks off any `AnimTarget`, paste them onto another
// (or the same) target with optional path remapping and a time offset. Paste
// returns one `SetKeyframe` per key so the caller wraps them in a single
// `Command::Batch` — one undo unit for the whole interchange.

/// Snapshot the keyframe lanes of `target`. When `paths` is `Some`, only those
/// properties are copied (unknown paths are skipped); `None` copies every
/// non-empty track. Empty result is still `Ok` — paste is then a no-op.
pub fn copy_keyframes(
    p: &TimelineProject,
    target: &AnimTarget,
    paths: Option<&[PropPath]>,
) -> Result<KeyframeClipboard, EditError> {
    let tracks = read_tracks(p, target).ok_or(EditError::IndexOutOfRange)?;
    let selected: Vec<PropertyTrack> = tracks
        .iter()
        .filter(|t| {
            if t.keyframes.is_empty() {
                return false;
            }
            match paths {
                None => true,
                Some(want) => want.iter().any(|p| p == &t.property),
            }
        })
        .cloned()
        .collect();
    Ok(KeyframeClipboard::from_tracks(selected))
}

/// Paste `clipboard` onto `target`.
///
/// * `mapping` — optional `(source_path, dest_path)` pairs. Unmapped source
///   tracks paste onto their original path (identity). A source path listed
///   with an empty dest string is skipped.
/// * `time_offset` — added to every keyframe `at` (clip-relative). Negative
///   offsets that would push a key before zero are clamped to `Tick::ZERO`.
///
/// Returns `Ok(vec![])` when nothing would change (empty clipboard / all
/// skipped). Each command is a single-key `SetKeyframe` (upsert).
pub fn paste_keyframes(
    p: &TimelineProject,
    target: AnimTarget,
    clipboard: &KeyframeClipboard,
    mapping: &[(PropPath, PropPath)],
    time_offset: Tick,
) -> Result<Vec<TimelineCmd>, EditError> {
    // Prove the target exists before building commands.
    let _ = read_tracks(p, &target).ok_or(EditError::IndexOutOfRange)?;

    let mut cmds = Vec::new();
    for src in &clipboard.tracks {
        let dest_path = match mapping
            .iter()
            .find(|(s, _)| s == &src.property)
            .map(|(_, d)| d.clone())
        {
            Some(d) if d.as_str().is_empty() => continue,
            Some(d) => d,
            None => src.property.clone(),
        };
        for kf in &src.keyframes {
            let at = Tick((kf.at.0 + time_offset.0).max(0));
            let new_kf = Keyframe::new(at, kf.value, kf.interp);
            cmds.push(set_keyframe(p, target.clone(), dest_path.clone(), new_kf));
        }
    }
    Ok(cmds)
}

/// Convenience: paste so `clipboard.anchor` lands at `dest_anchor`.
pub fn paste_keyframes_reanchored(
    p: &TimelineProject,
    target: AnimTarget,
    clipboard: &KeyframeClipboard,
    mapping: &[(PropPath, PropPath)],
    dest_anchor: Tick,
) -> Result<Vec<TimelineCmd>, EditError> {
    let offset = Tick(dest_anchor.0 - clipboard.anchor.0);
    paste_keyframes(p, target, clipboard, mapping, offset)
}

// ── Effects ─────────────────────────────────────────────────────────────────
//
// Four scopes (26 §10 K-B1/K-B2, 35 §2): clip, track, master, asset. The
// `*_scoped` ops are the general form; the four clip-shaped wrappers below are
// kept because every existing GUI/MCP call site is clip-shaped and their
// `(seq, track, clip)` triple additionally *validates* that the clip really
// lives on that track of that sequence — a check the id-addressed scoped form
// cannot make.
//
// Gated on manifest `Applicability` (30 §2.3) in `add_effect_scoped`: known
// manifests refuse owners outside `applies`. The catalogue currently uses
// `ALL_SCOPES` (K-B1-compatible); `CLIP_ONLY` remains for per-id curation.
// Unknown / unmanifested ids stay allowed (forward-compat, 39 §2.2).

/// Read the effect stack a [`VfxOwner`] names, or the owner-shaped `EditError`
/// when it does not resolve.
pub fn effect_stack(p: &TimelineProject, owner: VfxOwner) -> Result<&[ClipEffect], EditError> {
    match owner {
        VfxOwner::Clip(c) => Ok(&find_clip_anywhere(p, c)
            .ok_or(EditError::NoClip(c))?
            .effects),
        VfxOwner::Track(t) => Ok(&find_track_anywhere(p, t)
            .ok_or(EditError::NoTrack(t))?
            .effects),
        VfxOwner::Master(s) => Ok(&p
            .sequences
            .get(&s)
            .ok_or(EditError::NoSequence(s))?
            .master_effects),
        VfxOwner::Asset(a) => Ok(&p.media.assets.get(&a).ok_or(EditError::NoAsset(a))?.effects),
    }
}

/// Read the grade slot a [`VfxOwner`] names (35 §2).
pub fn scope_grade(p: &TimelineProject, owner: VfxOwner) -> Result<Option<&Grade>, EditError> {
    match owner {
        VfxOwner::Clip(c) => Ok(find_clip_anywhere(p, c)
            .ok_or(EditError::NoClip(c))?
            .grade
            .as_ref()),
        VfxOwner::Track(t) => Ok(find_track_anywhere(p, t)
            .ok_or(EditError::NoTrack(t))?
            .grade
            .as_ref()),
        VfxOwner::Master(s) => Ok(p
            .sequences
            .get(&s)
            .ok_or(EditError::NoSequence(s))?
            .master_grade
            .as_ref()),
        VfxOwner::Asset(a) => Ok(p
            .media
            .assets
            .get(&a)
            .ok_or(EditError::NoAsset(a))?
            .grade
            .as_ref()),
    }
}

fn find_clip_anywhere(p: &TimelineProject, id: ClipId) -> Option<&Clip> {
    p.sequences.values().find_map(|s| {
        s.video_tracks
            .iter()
            .chain(s.audio_tracks.iter())
            .find_map(|t| t.clips.iter().find(|c| c.id == id))
    })
}

fn find_track_anywhere(p: &TimelineProject, id: TrackId) -> Option<&Track> {
    p.sequences.values().find_map(|s| {
        s.video_tracks
            .iter()
            .chain(s.audio_tracks.iter())
            .find(|t| t.id == id)
    })
}

/// Insert an effect into any scope's stack. One call → one command → one undo
/// unit, whose inverse is the position-preserving `RemoveEffect` at the same
/// index (so undo restores exact stack order, not just membership).
pub fn add_effect_scoped(
    p: &TimelineProject,
    owner: VfxOwner,
    effect: ClipEffect,
    index: Option<usize>,
) -> Result<TimelineCmd, EditError> {
    // Manifest Applicability gate (K-B residual): refuse scopes the effect
    // does not declare. Unknown / unmanifested ids stay allowed (forward-compat).
    let mid = if effect.id.is_empty() {
        effect.kind.effect_id()
    } else {
        effect.id.clone()
    };
    if let Some(m) = super::effect_manifest::manifest(mid) {
        if !m.applies.allows(owner) {
            return Err(EditError::ApplicabilityDenied);
        }
    }
    let len = effect_stack(p, owner)?.len();
    let idx = index.unwrap_or(len).min(len);
    Ok(TimelineCmd::AddEffect {
        owner,
        index: idx,
        effect: Box::new(effect),
    })
}

pub fn remove_effect_scoped(
    p: &TimelineProject,
    owner: VfxOwner,
    index: usize,
) -> Result<TimelineCmd, EditError> {
    let effect = effect_stack(p, owner)?
        .get(index)
        .ok_or(EditError::IndexOutOfRange)?
        .clone();
    Ok(TimelineCmd::RemoveEffect {
        owner,
        index,
        effect: Box::new(effect),
    })
}

/// `new_order[i]` is the index (in the current stack) of the effect that ends
/// up at position `i` — a gather permutation, same convention as the clip form.
pub fn reorder_effects_scoped(
    p: &TimelineProject,
    owner: VfxOwner,
    new_order: Vec<usize>,
) -> Result<TimelineCmd, EditError> {
    let len = effect_stack(p, owner)?.len();
    if new_order.len() != len || !is_permutation(&new_order, len) {
        return Err(EditError::IndexOutOfRange);
    }
    Ok(TimelineCmd::ReorderEffects {
        owner,
        old_order: (0..len).collect(),
        new_order,
    })
}

/// Replace one stacked effect (enable/disable, static param edit) at any scope.
pub fn set_effect_scoped(
    p: &TimelineProject,
    owner: VfxOwner,
    index: usize,
    new: ClipEffect,
) -> Result<TimelineCmd, EditError> {
    let old = effect_stack(p, owner)?
        .get(index)
        .ok_or(EditError::IndexOutOfRange)?
        .clone();
    Ok(TimelineCmd::SetEffect {
        owner,
        index,
        old: Box::new(old),
        new: Box::new(new),
    })
}

/// K-B3: set (or clear) the effect zone on stack entry `index`.
/// `zone` is half-open `[start, end)` in the stack's evaluation domain.
/// `None` clears the zone (effect applies to the whole span).
/// Refuses non-positive ranges (`end <= start`).
pub fn set_effect_zone(
    p: &TimelineProject,
    owner: VfxOwner,
    index: usize,
    zone: Option<(Tick, Tick)>,
) -> Result<TimelineCmd, EditError> {
    if let Some((a, b)) = zone {
        if b.0 <= a.0 {
            return Err(EditError::NonPositiveDuration);
        }
    }
    let mut new = effect_stack(p, owner)?
        .get(index)
        .ok_or(EditError::IndexOutOfRange)?
        .clone();
    new.zone = zone;
    set_effect_scoped(p, owner, index, new)
}

pub fn set_grade_scoped(
    p: &TimelineProject,
    owner: VfxOwner,
    new: Option<Grade>,
) -> Result<TimelineCmd, EditError> {
    let old = scope_grade(p, owner)?.cloned();
    Ok(TimelineCmd::SetGrade {
        owner,
        old: old.map(Box::new),
        new: new.map(Box::new),
    })
}

/// True when `order` is a permutation of `0..len` — a reorder that dropped or
/// duplicated a slot would silently lose an effect on apply.
fn is_permutation(order: &[usize], len: usize) -> bool {
    let mut seen = vec![false; len];
    for &i in order {
        match seen.get_mut(i) {
            Some(slot) if !*slot => *slot = true,
            _ => return false,
        }
    }
    true
}

pub fn add_effect(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    effect: ClipEffect,
    index: Option<usize>,
) -> Result<TimelineCmd, EditError> {
    let s = seq(p, id)?;
    let t = track(s, track_id)?;
    clip(t, clip_id)?;
    add_effect_scoped(p, VfxOwner::Clip(clip_id), effect, index)
}

pub fn remove_effect(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    index: usize,
) -> Result<TimelineCmd, EditError> {
    let s = seq(p, id)?;
    let t = track(s, track_id)?;
    clip(t, clip_id)?;
    remove_effect_scoped(p, VfxOwner::Clip(clip_id), index)
}

pub fn reorder_effects(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    new_order: Vec<usize>,
) -> Result<TimelineCmd, EditError> {
    let s = seq(p, id)?;
    let t = track(s, track_id)?;
    clip(t, clip_id)?;
    reorder_effects_scoped(p, VfxOwner::Clip(clip_id), new_order)
}

pub fn set_grade(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    new: Option<Grade>,
) -> Result<TimelineCmd, EditError> {
    let s = seq(p, id)?;
    let t = track(s, track_id)?;
    clip(t, clip_id)?;
    set_grade_scoped(p, VfxOwner::Clip(clip_id), new)
}

// ── Paste Attributes (26 §10 K-B15) ─────────────────────────────────────────
//
// "Make these ten shots look like that one." Copies the *look-carrying* half of
// a clip onto one or more already-existing clips, leaving every clip's identity
// and timing alone. Deliberately NOT the clip clipboard (`edit.copy` /
// `video.paste`), which lays down whole new clips.
//
// WHAT IS CARRIED, and why exactly this set:
//
//   effects   `Clip::effects`  — the stack itself, the headline of the verb.
//   grade     `Clip::grade`    — 07's colour pipeline, the other half of "look".
//   transform `Clip::transform` — pos/scale/rotation/anchor/opacity, keyframes
//             included.
//   audio     `Clip::audio`    — gain, fades, channel map.
//
// WHAT IS NOT, and why (each exclusion is a "a paste that silently moves a clip
// is a bug" call):
//
//   start / duration / source / source_in / id / name  — identity and timing.
//   speed      `SpeedMap` decides *which source frames* fill the slot. Pasting
//              it silently changes what media the target shows, which is a
//              timing edit wearing a look edit's clothes. 26 §10 K-B15's own
//              selector list stops at `{effects, grade, transform, audio}`.
//   reframe    Per-`SequenceFormat` transform override (CAP-012). Argued for —
//              it is transform data — but excluded twice over: 26 §10 K-B15's
//              selector list does not name it, and `Clip::reframe` is a
//              `HashMap<usize, _>` that a `SetClipProp` CANNOT round-trip
//              today (`TimelineCmd` is an internally-tagged enum, and serde's
//              content buffer will not coerce a JSON string map key back to
//              `usize`), so carrying it would mint commands the history
//              journal cannot reload. That defect already bites
//              `set_clip_reframe`; it is filed separately rather than papered
//              over here. Consequence to know: after a transform paste the
//              target keeps its OWN per-format overrides.
//   composition A per-clip `GraphId` into the shared graph arena; carrying it
//              would alias two clips onto one graph, and deep-cloning it is
//              `paste_composition`'s job (08 §4), not this verb's.
//   transitions `Sequence::validate_transitions` REJECTS a `transition_out` at
//              a hard cut, so a pasted transition really could break the
//              sequence invariant `TimelineCmd::apply` debug-asserts after
//              every command. Excluding them is what makes the multi-target
//              paste safe as a plain `Command::Batch` (see below).
//   enabled / color_label / markers / group / link_group / multicam — per-clip
//              organisation and identity, not look.
//
// ONE UNDO UNIT: `paste_clip_attributes` returns one `SetClipProp` per target
// for the caller to wrap in a single `Command::Batch`. `Command::Batch` applies
// its members one at a time and `TimelineCmd::apply` debug-asserts
// `Sequence::validate()` after each, so a batched multi-clip edit is only safe
// if every intermediate state is valid. It is here, provably: `validate()`
// inspects clip durations, per-track ordering/overlap, transitions and the
// group tree — and the attribute set above touches NONE of those fields.
// `paste_attributes_batch_never_breaks_the_sequence_invariant` pins that.

/// The look-carrying half of a [`Clip`] (26 §10 K-B15) — everything
/// [`paste_clip_attributes`] can transfer, and nothing that identifies or times
/// a clip. Detached from the source clip so a GUI clipboard can hold it across
/// edits (and so the LUT-asset check below is meaningful even for a stale one).
#[derive(Clone, Debug, PartialEq)]
pub struct ClipAttributes {
    pub effects: Vec<ClipEffect>,
    pub grade: Option<Grade>,
    pub transform: AnimProps<ClipTransform>,
    pub audio: Option<ClipAudio>,
}

impl ClipAttributes {
    /// Snapshot the pasteable attributes of `clip`.
    pub fn of(clip: &Clip) -> Self {
        ClipAttributes {
            effects: clip.effects.clone(),
            grade: clip.grade.clone(),
            transform: clip.transform.clone(),
            audio: clip.audio.clone(),
        }
    }
}

/// Which attribute families a paste transfers (26 §10 K-B15's selector flags).
/// A `false` flag leaves the target's own value completely untouched — it is
/// not "paste the default", it is "do not touch".
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct AttrSelector {
    pub effects: bool,
    pub grade: bool,
    /// `Clip::transform` only — `Clip::reframe` is deliberately not carried
    /// (see the module comment above this section).
    pub transform: bool,
    pub audio: bool,
}

impl AttrSelector {
    /// Everything — the default a bare "Paste Attributes" means.
    pub const ALL: AttrSelector = AttrSelector {
        effects: true,
        grade: true,
        transform: true,
        audio: true,
    };
    /// Kdenlive's narrower "Paste Effects".
    pub const EFFECTS_ONLY: AttrSelector = AttrSelector {
        effects: true,
        grade: false,
        transform: false,
        audio: false,
    };

    /// True when no family is selected — the paste can only be a no-op.
    pub fn is_empty(&self) -> bool {
        !(self.effects || self.grade || self.transform || self.audio)
    }
}

impl Default for AttrSelector {
    fn default() -> Self {
        AttrSelector::ALL
    }
}

/// Read one clip's pasteable attributes by id, from anywhere in the project.
pub fn clip_attributes(p: &TimelineProject, clip: ClipId) -> Result<ClipAttributes, EditError> {
    let c = find_clip_anywhere(p, clip).ok_or(EditError::NoClip(clip))?;
    Ok(ClipAttributes::of(c))
}

/// `(sequence, track, clip)` for a clip id, searched across every sequence —
/// the address `SetClipProp` needs, which [`find_clip_anywhere`] cannot give.
fn locate_clip_anywhere(p: &TimelineProject, id: ClipId) -> Option<(SequenceId, TrackId, &Clip)> {
    p.sequences.values().find_map(|s| {
        s.video_tracks
            .iter()
            .chain(s.audio_tracks.iter())
            .find_map(|t| t.clips.iter().find(|c| c.id == id).map(|c| (s.id, t.id, c)))
    })
}

/// Every `AssetId` a grade references (today: `Lut3d`). 26 §10 K-B15's
/// watch-out — a pasted grade must not leave the target pointing at a LUT the
/// project does not have.
fn grade_asset_refs(g: &Grade) -> Vec<AssetId> {
    g.ops
        .iter()
        .filter_map(|op| match &op.params.base {
            GradeOpParams::Lut3d { asset, .. } => Some(*asset),
            _ => None,
        })
        .collect()
}

/// **Paste Attributes** (26 §10 K-B15): stamp `attrs` onto each of `targets`,
/// filtered by `sel`. Returns one [`TimelineCmd::SetClipProp`] per target that
/// actually changes, for the caller to wrap in ONE `Command::Batch` — pasting
/// onto N clips is one user verb and must be one undo step.
///
/// Timing is untouched by construction: each command is the target's own clip
/// with only the selected attribute families replaced.
///
/// Rejects (atomically — no partial paste, the whole call returns `Err`):
/// * [`EditError::NoClip`] — a target id that is not in the project.
/// * [`EditError::NoAsset`] — the grade carries a `Lut3d` whose asset is absent
///   from this project (a clipboard outliving a `remove_asset`).
///
/// Audio fades are clamped to each target's own duration: an `AudioFade` longer
/// than its clip never reaches unity gain (`mixer::shape_gain` divides by
/// `fade.duration`), so copying a 2s fade onto a 1s shot would quietly duck it
/// for its whole length.
pub fn paste_clip_attributes(
    p: &TimelineProject,
    attrs: &ClipAttributes,
    targets: &[ClipId],
    sel: AttrSelector,
) -> Result<Vec<TimelineCmd>, EditError> {
    if sel.grade {
        if let Some(g) = attrs.grade.as_ref() {
            for a in grade_asset_refs(g) {
                if !p.media.assets.contains_key(&a) {
                    return Err(EditError::NoAsset(a));
                }
            }
        }
    }

    // Resolve every target first so an unknown id fails before any command is
    // built — a paste that lands on nine of ten selected clips is worse than a
    // paste that refuses.
    let mut resolved: Vec<(SequenceId, TrackId, Clip)> = Vec::with_capacity(targets.len());
    let mut seen: Vec<ClipId> = Vec::with_capacity(targets.len());
    for &t in targets {
        let (s, tr, c) = locate_clip_anywhere(p, t).ok_or(EditError::NoClip(t))?;
        if seen.contains(&t) {
            continue; // a duplicated id in the selection is one paste, not two
        }
        seen.push(t);
        resolved.push((s, tr, c.clone()));
    }
    if sel.is_empty() {
        return Ok(Vec::new());
    }

    let mut cmds = Vec::with_capacity(resolved.len());
    for (seq_id, track_id, old) in resolved {
        let mut new = old.clone();
        if sel.effects {
            new.effects = attrs.effects.clone();
        }
        if sel.grade {
            new.grade = attrs.grade.clone();
        }
        if sel.transform {
            new.transform = attrs.transform.clone();
        }
        if sel.audio {
            new.audio = attrs.audio.clone().map(|mut a| {
                for fade in [&mut a.fade_in, &mut a.fade_out] {
                    if let Some(f) = fade.as_mut() {
                        if f.duration > new.duration {
                            f.duration = new.duration;
                        }
                    }
                }
                a
            });
        }
        if new == old {
            continue; // no-op target: keep it out of the undo step entirely
        }
        cmds.push(TimelineCmd::SetClipProp {
            seq: seq_id,
            track: track_id,
            old: Box::new(old),
            new: Box::new(new),
        });
    }
    Ok(cmds)
}

// ── Compositions (08 §4) ────────────────────────────────────────────────────

/// Create a per-clip composition (`ClipIn → Output`) and point the clip at it.
/// Rejected for `ClipSource::Adjustment` (07 §6.6). Returns
/// `[AddGraph, SetClipComposition]` for the caller to batch.
pub fn create_clip_composition(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
) -> Result<Vec<TimelineCmd>, EditError> {
    let s = seq(p, id)?;
    let t = track(s, track_id)?;
    let c = clip(t, clip_id)?;
    if matches!(c.source, ClipSource::Adjustment) {
        return Err(EditError::CompositionOnAdjustment);
    }
    let (graph, _clip_in) = NodeGraph::new_clip_composition(format!("{} comp", c.name));
    let new_ref = graph.id;
    Ok(vec![
        TimelineCmd::AddGraph {
            graph: Box::new(graph),
        },
        TimelineCmd::SetClipComposition {
            seq: id,
            track: track_id,
            clip: clip_id,
            old: c.composition,
            new: Some(new_ref),
        },
    ])
}

/// Detach a clip's composition (revert). The graph stays in the arena (08 §4).
pub fn detach_clip_composition(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
) -> Result<TimelineCmd, EditError> {
    let s = seq(p, id)?;
    let t = track(s, track_id)?;
    let c = clip(t, clip_id)?;
    Ok(TimelineCmd::SetClipComposition {
        seq: id,
        track: track_id,
        clip: clip_id,
        old: c.composition,
        new: None,
    })
}

/// Paste a composition, DEEP-CLONING the source graph under a fresh id so the
/// two clips never alias (08 §4). Returns `[AddGraph(clone), SetClipComposition]`.
pub fn paste_clip_composition(
    p: &TimelineProject,
    source_graph: GraphId,
    id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
) -> Result<Vec<TimelineCmd>, EditError> {
    let src = p
        .graphs
        .get(&source_graph)
        .ok_or(EditError::NoGraph(source_graph))?;
    let s = seq(p, id)?;
    let t = track(s, track_id)?;
    let c = clip(t, clip_id)?;
    if matches!(c.source, ClipSource::Adjustment) {
        return Err(EditError::CompositionOnAdjustment);
    }
    let clone = src.deep_clone_fresh_ids();
    let new_ref = clone.id;
    Ok(vec![
        TimelineCmd::AddGraph {
            graph: Box::new(clone),
        },
        TimelineCmd::SetClipComposition {
            seq: id,
            track: track_id,
            clip: clip_id,
            old: c.composition,
            new: Some(new_ref),
        },
    ])
}

/// Set the project graph, allocating a fresh empty graph if `graph` is `None`.
pub fn set_project_graph(p: &TimelineProject, graph: Option<NodeGraph>) -> Vec<TimelineCmd> {
    let g = graph.unwrap_or_else(|| NodeGraph::new_project_graph("Project Graph"));
    let new_ref = g.id;
    vec![
        TimelineCmd::AddGraph { graph: Box::new(g) },
        TimelineCmd::SetProjectGraph {
            old: p.project_graph,
            new: Some(new_ref),
        },
    ]
}

// ── Audio ───────────────────────────────────────────────────────────────────

/// Apply the one-click ducking preset (AS-2, 09 §6.3): ensure a sidechained
/// `Compressor` exists on `music_track`, keyed off `voiceover_track`. Rejects a
/// sidechain cycle (09 §6.3).
pub fn apply_ducking_preset(
    p: &TimelineProject,
    music_track: TrackId,
    voiceover_track: TrackId,
) -> Result<TimelineCmd, EditError> {
    if music_track == voiceover_track || sidechain_reaches(p, voiceover_track, music_track) {
        return Err(EditError::SidechainCycle);
    }
    let t = track(seq_of_track(p, music_track)?, music_track)?;
    let old_fx_chain = t
        .audio
        .as_ref()
        .map(|a| a.fx_chain.clone())
        .unwrap_or_default();
    let mut new_fx_chain = old_fx_chain.clone();
    let mut comp = super::audio::AudioFxUnit::new(super::audio::AudioFxKind::Compressor);
    comp.sidechain = Some(voiceover_track);
    if let Some(slot) = new_fx_chain
        .iter_mut()
        .find(|u| u.kind == super::audio::AudioFxKind::Compressor)
    {
        *slot = comp;
    } else {
        new_fx_chain.push(comp);
    }
    Ok(TimelineCmd::AudioEdit(AudioCmd::ApplyDuckingPreset {
        track: music_track,
        sidechain: voiceover_track,
        old_fx_chain,
        new_fx_chain,
    }))
}

/// Does `from`'s compressor-sidechain graph (transitively) reach `to`?
fn sidechain_reaches(p: &TimelineProject, from: TrackId, to: TrackId) -> bool {
    let Some(t) = find_track_ro(p, from) else {
        return false;
    };
    let Some(a) = &t.audio else {
        return false;
    };
    for u in &a.fx_chain {
        if let Some(sc) = u.sidechain {
            if sc == to || sidechain_reaches(p, sc, to) {
                return true;
            }
        }
    }
    false
}

fn find_track_ro(p: &TimelineProject, id: TrackId) -> Option<&Track> {
    p.sequences
        .values()
        .flat_map(|s| s.video_tracks.iter().chain(s.audio_tracks.iter()))
        .find(|t| t.id == id)
}

fn seq_of_track(p: &TimelineProject, id: TrackId) -> Result<&Sequence, EditError> {
    p.sequences
        .values()
        .find(|s| s.track(id).is_some())
        .ok_or(EditError::NoTrack(id))
}

// ── Markers & work range ────────────────────────────────────────────────────

/// Add a marker to a sequence. The caller supplies the fully-built `Marker`
/// (name/color/note/position); `Marker::new` fills a fresh `MarkerId`.
pub fn add_marker(
    p: &TimelineProject,
    id: SequenceId,
    marker: Marker,
) -> Result<TimelineCmd, EditError> {
    seq(p, id)?;
    Ok(TimelineCmd::AddMarker { seq: id, marker })
}

pub fn remove_marker(
    p: &TimelineProject,
    id: SequenceId,
    marker_id: MarkerId,
) -> Result<TimelineCmd, EditError> {
    let s = seq(p, id)?;
    let marker = s
        .markers
        .iter()
        .find(|m| m.id == marker_id)
        .ok_or(EditError::IndexOutOfRange)?
        .clone();
    Ok(TimelineCmd::RemoveMarker { seq: id, marker })
}

/// Edit a marker's fields (name/color/note/position). `new.id` identifies the
/// marker; the op captures the old state for a self-contained inverse.
pub fn set_marker(
    p: &TimelineProject,
    id: SequenceId,
    new: Marker,
) -> Result<TimelineCmd, EditError> {
    let s = seq(p, id)?;
    let old = s
        .markers
        .iter()
        .find(|m| m.id == new.id)
        .ok_or(EditError::IndexOutOfRange)?
        .clone();
    Ok(TimelineCmd::SetMarker {
        seq: id,
        id: new.id,
        old,
        new,
    })
}

// ── Clip-scoped markers (35 §1.5) ───────────────────────────────────────────
//
// The sequence-marker trio above addresses `(SequenceId, MarkerId)`; a clip
// marker lives on the clip and travels with it, so it is addressed by
// `ClipId` alone (`find_clip_mut`'s convention, shared with
// `SetClipComposition`). `at` is CLIP-RELATIVE — use
// `Clip::marker_sequence_tick` to place one on the timeline.

/// Locate a clip anywhere in the project, by id alone.
fn clip_anywhere(p: &TimelineProject, id: ClipId) -> Result<&Clip, EditError> {
    p.sequences
        .values()
        .flat_map(|s| s.video_tracks.iter().chain(s.audio_tracks.iter()))
        .flat_map(|t| t.clips.iter())
        .find(|c| c.id == id)
        .ok_or(EditError::NoClip(id))
}

/// Add a clip-scoped marker. The caller supplies the fully-built `Marker`;
/// [`Marker::clip_scoped`] fills a fresh id and the `Content` anchor.
///
/// `at` is validated against the clip's own length, because an out-of-range
/// clip marker is invisible: it maps to a sequence tick outside the clip and
/// no consumer would ever draw it.
pub fn add_clip_marker(
    p: &TimelineProject,
    clip_id: ClipId,
    marker: Marker,
) -> Result<TimelineCmd, EditError> {
    let c = clip_anywhere(p, clip_id)?;
    if marker.at.0 < 0 || marker.at > c.duration {
        return Err(EditError::IndexOutOfRange);
    }
    Ok(TimelineCmd::AddClipMarker {
        clip: clip_id,
        marker,
    })
}

pub fn remove_clip_marker(
    p: &TimelineProject,
    clip_id: ClipId,
    marker_id: MarkerId,
) -> Result<TimelineCmd, EditError> {
    let c = clip_anywhere(p, clip_id)?;
    let marker = c
        .markers
        .iter()
        .find(|m| m.id == marker_id)
        .ok_or(EditError::NoMarker(marker_id))?
        .clone();
    Ok(TimelineCmd::RemoveClipMarker {
        clip: clip_id,
        marker,
    })
}

/// Edit a clip marker's fields. `new.id` identifies the marker; the op captures
/// the old state for a self-contained inverse.
pub fn set_clip_marker(
    p: &TimelineProject,
    clip_id: ClipId,
    new: Marker,
) -> Result<TimelineCmd, EditError> {
    let c = clip_anywhere(p, clip_id)?;
    let old = c
        .markers
        .iter()
        .find(|m| m.id == new.id)
        .ok_or(EditError::NoMarker(new.id))?
        .clone();
    if new.at.0 < 0 || new.at > c.duration {
        return Err(EditError::IndexOutOfRange);
    }
    Ok(TimelineCmd::SetClipMarker {
        clip: clip_id,
        id: new.id,
        old,
        new,
    })
}

// ── Marker categories (35 §1.3) ─────────────────────────────────────────────

/// Add a project marker category, appended to the end of display order.
///
/// Rejects a category whose id is already present — ids are stable and unique
/// within the registry, and a duplicate would make `marker_category` ambiguous.
pub fn add_marker_category(
    p: &TimelineProject,
    category: MarkerCategory,
) -> Result<TimelineCmd, EditError> {
    if p.marker_category(category.id).is_some() {
        return Err(EditError::IndexOutOfRange);
    }
    Ok(TimelineCmd::AddMarkerCategory {
        index: p.marker_categories.len(),
        category,
        retarget: Vec::new(),
    })
}

/// Seed the default categories ([`MarkerCategory::default_seed`]) as ONE
/// batch of commands — the caller commits them as a single undo unit.
///
/// Returns an empty vec when the project already has categories, so "seed
/// defaults" is idempotent rather than duplicating the set.
pub fn seed_marker_categories(p: &TimelineProject) -> Vec<TimelineCmd> {
    if !p.marker_categories.is_empty() {
        return Vec::new();
    }
    MarkerCategory::default_seed()
        .into_iter()
        .enumerate()
        .map(|(i, category)| TimelineCmd::AddMarkerCategory {
            category,
            index: i,
            retarget: Vec::new(),
        })
        .collect()
}

/// Ensure a "Bookmarks" category exists (proposal 210). If the registry is
/// empty, seeds the full default set. If categories already exist but none is
/// named Bookmarks, appends one. Returns the commands to commit (may be empty
/// when Bookmarks is already present) and the category id to use.
pub fn ensure_bookmarks_category(
    p: &TimelineProject,
) -> Result<(Vec<TimelineCmd>, MarkerCategoryId), EditError> {
    if let Some(c) = p
        .marker_categories
        .iter()
        .find(|c| c.name == MarkerCategory::BOOKMARKS_CATEGORY_NAME)
    {
        return Ok((Vec::new(), c.id));
    }
    if p.marker_categories.is_empty() {
        let cmds = seed_marker_categories(p);
        // After seed, the Bookmarks entry is the last of default_seed — but
        // ids are only known from the commands themselves.
        let id = cmds
            .iter()
            .find_map(|cmd| match cmd {
                TimelineCmd::AddMarkerCategory { category, .. }
                    if category.name == MarkerCategory::BOOKMARKS_CATEGORY_NAME =>
                {
                    Some(category.id)
                }
                _ => None,
            })
            .ok_or(EditError::IndexOutOfRange)?;
        return Ok((cmds, id));
    }
    let cat = MarkerCategory {
        id: MarkerCategoryId::new(),
        name: MarkerCategory::BOOKMARKS_CATEGORY_NAME.to_string(),
        color: crate::Color::rgb(0.70, 0.45, 0.95),
        glyph: MarkerGlyph::Flag,
    };
    let id = cat.id;
    let cmd = add_marker_category(p, cat)?;
    Ok((vec![cmd], id))
}

/// Drop a bookmark (sequence marker in the Bookmarks category) at `at`.
/// One logical user verb; caller should batch seed+add as one undo unit.
pub fn add_bookmark(
    p: &TimelineProject,
    seq_id: SequenceId,
    at: Tick,
    name: impl Into<String>,
) -> Result<Vec<TimelineCmd>, EditError> {
    let (mut cmds, cat_id) = ensure_bookmarks_category(p)?;
    // If we just seeded categories, the project snapshot `p` still has an
    // empty registry — `add_marker` only checks the sequence exists, so that
    // is fine. Category id is carried on the marker itself.
    let mut marker = Marker::new(at, name);
    marker.category = Some(cat_id);
    cmds.push(add_marker(p, seq_id, marker)?);
    Ok(cmds)
}

/// Rename / recolour / re-glyph a category. `new.id` identifies it; the id is
/// never changed by this op (that would orphan every referencing marker).
pub fn set_marker_category(
    p: &TimelineProject,
    new: MarkerCategory,
) -> Result<TimelineCmd, EditError> {
    let old = p
        .marker_category(new.id)
        .ok_or(EditError::NoMarkerCategory(new.id))?
        .clone();
    Ok(TimelineCmd::SetMarkerCategory {
        id: new.id,
        old,
        new,
    })
}

/// Remove a marker category, deciding explicitly what happens to the markers
/// that referenced it (26 K-A2's "reassign on delete").
///
/// * `reassign_to == Some(other)` — every referencing marker, in both scopes,
///   moves to `other` as part of the same command.
/// * `reassign_to == None` — the references are cleared to `None`.
///
/// Either way the change is *recorded* per marker, so undo restores each one
/// to the deleted category and 35 §1.3's "never silently remapped" rule holds:
/// nothing is left pointing at a category that no longer exists, and nothing
/// is re-pointed without the command saying so.
pub fn remove_marker_category(
    p: &TimelineProject,
    id: MarkerCategoryId,
    reassign_to: Option<MarkerCategoryId>,
) -> Result<TimelineCmd, EditError> {
    let category = p
        .marker_category(id)
        .ok_or(EditError::NoMarkerCategory(id))?
        .clone();
    let index = p
        .marker_category_index(id)
        .ok_or(EditError::NoMarkerCategory(id))?;
    if let Some(target) = reassign_to {
        // Reassigning to the category being deleted would leave every marker
        // pointing at a missing id — the exact outcome the rule forbids.
        if target == id || p.marker_category(target).is_none() {
            return Err(EditError::NoMarkerCategory(target));
        }
    }
    let retarget = p
        .markers_in_category(id)
        .into_iter()
        .map(|marker| MarkerRetarget {
            marker,
            old: Some(id),
            new: reassign_to,
        })
        .collect();
    Ok(TimelineCmd::RemoveMarkerCategory {
        category,
        index,
        retarget,
    })
}

/// Point one marker (either scope) at a category, or clear it with `None`.
/// A convenience over [`set_marker`] / [`set_clip_marker`] for the panel's
/// per-row category picker; validates that the target category exists so a
/// dangling reference can never be *created* by an edit (only inherited from a
/// file, where the UI flags it).
pub fn set_marker_category_of(
    p: &TimelineProject,
    marker: MarkerRef,
    category: Option<MarkerCategoryId>,
) -> Result<TimelineCmd, EditError> {
    if let Some(c) = category {
        if p.marker_category(c).is_none() {
            return Err(EditError::NoMarkerCategory(c));
        }
    }
    match marker {
        MarkerRef::Sequence { seq: sid, marker } => {
            let s = seq(p, sid)?;
            let mut new = s
                .markers
                .iter()
                .find(|m| m.id == marker)
                .ok_or(EditError::NoMarker(marker))?
                .clone();
            new.category = category;
            set_marker(p, sid, new)
        }
        MarkerRef::Clip { clip, marker } => {
            let c = clip_anywhere(p, clip)?;
            let mut new = c
                .markers
                .iter()
                .find(|m| m.id == marker)
                .ok_or(EditError::NoMarker(marker))?
                .clone();
            new.category = category;
            set_clip_marker(p, clip, new)
        }
    }
}

/// Set (or clear, with `None`) a sequence's preview/export work range.
/// Replace preview intent with sorted, merged, outward-frame-snapped zones.
pub fn set_preview_zones(
    p: &TimelineProject,
    id: SequenceId,
    zones: &[super::sequence::PreviewZone],
) -> Result<TimelineCmd, EditError> {
    let sequence = seq(p, id)?;
    if zones.len() > 128 {
        return Err(EditError::IndexOutOfRange);
    }
    let rate = sequence.frame_rate;
    let mut zones = zones.to_vec();
    for zone in &mut zones {
        if zone.start.0 < 0 || zone.end <= zone.start {
            return Err(EditError::NonPositiveDuration);
        }
        zone.start = rate.frame_start(rate.frame_at(zone.start));
        zone.end = rate.frame_start(rate.frame_at(Tick(zone.end.0 - 1)) + 1);
    }
    zones.sort_by_key(|zone| zone.start);
    let mut merged: Vec<super::sequence::PreviewZone> = Vec::new();
    for zone in zones {
        if let Some(last) = merged.last_mut().filter(|last| last.end >= zone.start) {
            last.end = last.end.max(zone.end);
        } else {
            merged.push(zone);
        }
    }
    Ok(TimelineCmd::SetPreviewZones {
        seq: id,
        old: sequence.preview_zones.clone(),
        new: merged,
    })
}

pub fn set_work_range(
    p: &TimelineProject,
    id: SequenceId,
    new: Option<(Tick, Tick)>,
) -> Result<TimelineCmd, EditError> {
    let s = seq(p, id)?;
    Ok(TimelineCmd::SetWorkRange {
        seq: id,
        old: s.work_range,
        new,
    })
}

// ── Media bins ──────────────────────────────────────────────────────────────

/// Create a media bin (folder), optionally nested under `parent`.
pub fn create_bin(name: impl Into<String>, parent: Option<BinId>) -> TimelineCmd {
    TimelineCmd::AddBin {
        bin: MediaBin::new(name, parent),
    }
}

/// Remove a media bin. Assets/child bins referencing it are left untouched (a
/// dangling ref reads as unfiled); re-adding restores the bin verbatim.
pub fn remove_bin(p: &TimelineProject, bin_id: BinId) -> Result<TimelineCmd, EditError> {
    let bin = p
        .media
        .bins
        .iter()
        .find(|b| b.id == bin_id)
        .ok_or(EditError::IndexOutOfRange)?
        .clone();
    Ok(TimelineCmd::RemoveBin { bin })
}

/// Move an asset into `new_bin` (or to the pool root with `None`).
pub fn assign_asset_bin(
    p: &TimelineProject,
    asset: AssetId,
    new_bin: Option<BinId>,
) -> Result<TimelineCmd, EditError> {
    let a = p
        .media
        .assets
        .get(&asset)
        .ok_or(EditError::NoAsset(asset))?;
    // A target bin, if given, must exist.
    if let Some(b) = new_bin {
        if !p.media.bins.iter().any(|bin| bin.id == b) {
            return Err(EditError::IndexOutOfRange);
        }
    }
    Ok(TimelineCmd::AssignAssetBin {
        asset,
        old: a.bin,
        new: new_bin,
    })
}

// ── Multicam (17 §G-20) ─────────────────────────────────────────────────────

/// Consolidate several camera angles into one multicam clip (17 §G-20): attach
/// a [`MulticamGroup`] to `primary_clip` built from itself (angle 0) plus each
/// clip in `angle_clips` (its `source`/`source_in`/`name` become angles 1..),
/// and remove those folded clips. The primary's `source`/`source_in` are
/// unchanged (they already equal angle 0). Any `angle_clips` entry that names
/// the primary itself is skipped (a clip can't be its own extra angle).
/// Returns a batch (`SetClipProp` for the primary + one `RemoveClip` per folded
/// clip) for the caller to wrap in one undo step.
pub fn create_multicam_group(
    p: &TimelineProject,
    id: SequenceId,
    primary_track: TrackId,
    primary_clip: ClipId,
    angle_clips: &[(TrackId, ClipId)],
) -> Result<Vec<TimelineCmd>, EditError> {
    let s = seq(p, id)?;
    let primary = clip(track(s, primary_track)?, primary_clip)?.clone();

    let mut angles = vec![MulticamAngle::new(
        primary.name.clone(),
        primary.source.clone(),
        primary.source_in,
    )];
    let mut removes = Vec::new();
    for (tk, cid) in angle_clips {
        if *cid == primary_clip {
            continue; // the primary is already angle 0
        }
        let c = clip(track(s, *tk)?, *cid)?;
        angles.push(MulticamAngle::new(
            c.name.clone(),
            c.source.clone(),
            c.source_in,
        ));
        removes.push(TimelineCmd::RemoveClip {
            seq: id,
            track: *tk,
            clip: Box::new(c.clone()),
        });
    }

    let mut new_primary = primary;
    new_primary.multicam = Some(MulticamGroup { angles, active: 0 });

    let mut cmds = vec![set_clip_prop(p, id, primary_track, new_primary)?];
    cmds.extend(removes);
    Ok(cmds)
}

/// Set the live angle of a multicam clip (17 §G-20). Clamps `angle` to a valid
/// index and mirrors the chosen angle's `source`/`source_in` onto the clip so a
/// multicam-unaware consumer still shows the live camera. Returns a
/// `SetClipProp`. Undoable. Errors ([`EditError::IndexOutOfRange`]) if the clip
/// carries no (non-empty) multicam group.
pub fn set_multicam_active_angle(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    angle: usize,
) -> Result<TimelineCmd, EditError> {
    let s = seq(p, id)?;
    let t = track(s, track_id)?;
    let mut new = clip(t, clip_id)?.clone();
    let group = new.multicam.as_ref().ok_or(EditError::IndexOutOfRange)?;
    if group.angles.is_empty() {
        return Err(EditError::IndexOutOfRange);
    }
    let a = angle.min(group.angles.len() - 1);
    let chosen = group.angles[a].clone();
    new.multicam.as_mut().unwrap().active = a;
    new.source = chosen.source;
    new.source_in = chosen.source_in;
    set_clip_prop(p, id, track_id, new)
}

// ── Nested sequences (17 §G-16) & open/breadcrumb (17 §G-17) ─────────────────

/// Wrap the `clip_ids` selection on one track into a new nested sequence (17
/// §G-16): build a fresh sequence holding a copy of the selected clips (rebased
/// so the earliest starts at 0), then replace them on the outer track with one
/// [`ClipSource::NestedSequence`] clip spanning their bounding box. Returns the
/// new sequence's id plus a batch (`AddSequence`, one `RemoveClip` per selected
/// clip, then `InsertClip` for the nested clip) for the caller to wrap in one
/// undo step; the ordering keeps every intermediate apply-state invariant-valid.
///
/// Rejects an empty selection ([`EditError::IndexOutOfRange`]), a requested clip
/// absent from the track ([`EditError::NoClip`]), or a non-selected clip lying
/// inside the selection's span (which the single replacement clip would overlap
/// — [`EditError::Overlap`]). Internal gaps *between* selected clips are fine
/// (they become empty space in the nested sequence). No cycle can arise: the
/// nested sequence is brand-new and nothing references it yet.
pub fn create_nested_sequence(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    clip_ids: &[ClipId],
    name: impl Into<String>,
) -> Result<(SequenceId, Vec<TimelineCmd>), EditError> {
    let s = seq(p, id)?;
    let t = track(s, track_id)?;
    if clip_ids.is_empty() {
        return Err(EditError::IndexOutOfRange);
    }
    // Any requested id that isn't on this track is an error.
    if let Some(missing) = clip_ids
        .iter()
        .find(|cid| !t.clips.iter().any(|c| c.id == **cid))
    {
        return Err(EditError::NoClip(*missing));
    }
    let sel_ids: std::collections::HashSet<ClipId> = clip_ids.iter().copied().collect();
    let selected: Vec<&Clip> = t.clips.iter().filter(|c| sel_ids.contains(&c.id)).collect();

    let min_start = selected.iter().map(|c| c.start).min().unwrap();
    let max_end = selected.iter().map(|c| c.end()).max().unwrap();
    // A non-selected clip inside the span would collide with the replacement.
    if t.clips
        .iter()
        .any(|c| !sel_ids.contains(&c.id) && c.start < max_end && min_start < c.end())
    {
        return Err(EditError::Overlap);
    }

    // Build the nested sequence: the selection rebased so `min_start → 0`, on a
    // fresh single track sized to the outer sequence's active format.
    let fmt = s.format();
    let mut inner = Sequence::new(name, s.frame_rate, fmt.width, fmt.height);
    let mut inner_track = Track::new(t.kind, t.name.clone());
    for c in &selected {
        let mut nc = (*c).clone();
        nc.id = ClipId::new();
        nc.start = c.start - min_start;
        inner_track.clips.push(nc);
    }
    inner.tracks_for_mut(t.kind).push(inner_track);
    let inner_id = inner.id;

    // Replace the selection with one NestedSequence clip over its bounding box.
    let mut nested_clip = Clip::new(
        ClipSource::NestedSequence { sequence: inner_id },
        min_start,
        max_end - min_start,
    );
    nested_clip.name = inner.name.clone();

    let mut cmds = vec![add_sequence(inner)];
    for c in &selected {
        cmds.push(TimelineCmd::RemoveClip {
            seq: id,
            track: track_id,
            clip: Box::new((*c).clone()),
        });
    }
    cmds.push(TimelineCmd::InsertClip {
        seq: id,
        track: track_id,
        clip: Box::new(nested_clip),
    });
    Ok((inner_id, cmds))
}

/// The nested sequence a clip opens into (17 §G-16/G-17 double-click target),
/// or `None` for a non-nested clip. Pure read.
pub fn nested_target(c: &Clip) -> Option<SequenceId> {
    match &c.source {
        ClipSource::NestedSequence { sequence } => Some(*sequence),
        _ => None,
    }
}

/// The breadcrumb ancestry of `target`: the chain `[root, …, target]` where
/// each entry nests the next via a [`ClipSource::NestedSequence`] clip (17
/// §G-17). `target` is always last; a sequence no other sequence nests is its
/// own root (a single-element chain). Nesting is acyclic (cycle-checked at edit
/// time) so the walk terminates; a `seen` guard defends against a malformed
/// cycle regardless. Pure read; the GUI renders it as a clickable trail.
pub fn sequence_ancestry(p: &TimelineProject, target: SequenceId) -> Vec<SequenceId> {
    let mut chain = vec![target];
    let mut current = target;
    let mut seen = std::collections::HashSet::new();
    seen.insert(current);
    while let Some(parent) = nesting_parent(p, current) {
        if !seen.insert(parent) {
            break;
        }
        chain.push(parent);
        current = parent;
    }
    chain.reverse();
    chain
}

/// A sequence that directly nests `child` via a `NestedSequence` clip, if any.
fn nesting_parent(p: &TimelineProject, child: SequenceId) -> Option<SequenceId> {
    p.sequences.values().find_map(|s| {
        let nests = s
            .video_tracks
            .iter()
            .chain(s.audio_tracks.iter())
            .flat_map(|t| t.clips.iter())
            .any(|c| {
                matches!(&c.source, ClipSource::NestedSequence { sequence } if *sequence == child)
            });
        nests.then_some(s.id)
    })
}

// ── 3/4-point editing: insert / overwrite / lift / extract (16 §2, gap L-1) ──
//
// The four reference-NLE edit ops. Each is a pure fn returning a batch of the
// existing timeline primitives (`SplitClip`/`RemoveClip`/`RippleEdit`/
// `TrimClip`/`InsertClip`) for the caller to wrap in ONE `Command::Batch` (one
// undo step). The command order is chosen so every intermediate apply-state
// stays invariant-valid (the debug-time per-command `validate()` in
// `TimelineCmd::apply`): removes/shrinks precede any insert into freshly-opened
// space, and multi-clip shifts ride a single atomic `RippleEdit`.

/// New timing for trimming a clip's OUT-point back to `boundary` (shrink; the
/// clip keeps its `start`/`source_in`).
fn trim_end_to(c: &Clip, boundary: Tick) -> ClipTiming {
    ClipTiming {
        start: c.start,
        duration: boundary - c.start,
        source_in: c.source_in,
    }
}

/// New timing for trimming a clip's IN-point forward to `boundary` (shrink; the
/// timeline `start` moves to `boundary` and `source_in` advances by the
/// speed-scaled delta).
fn trim_start_to(c: &Clip, boundary: Tick) -> ClipTiming {
    let delta = boundary - c.start;
    ClipTiming {
        start: boundary,
        duration: c.duration - delta,
        source_in: c.source_in + c.speed.source_delta(delta),
    }
}

/// Reject a source clip whose nested sequence would cycle (mirrors
/// [`insert_clip`]); a no-op for every other source kind.
fn reject_nested_cycle(
    p: &TimelineProject,
    id: SequenceId,
    source: &Clip,
) -> Result<(), EditError> {
    if let ClipSource::NestedSequence { sequence } = &source.source {
        if *sequence == id || nests_into(p, *sequence, id) {
            return Err(EditError::SequenceCycle);
        }
    }
    Ok(())
}

/// Shared core of lift/overwrite/extract: clear the content in `[rs, re)` on
/// `t`, then shift every clip that survives at/after `re` by `delta`
/// (`Tick::ZERO` = leave the gap, for lift/overwrite; `-(re-rs)` = close it, for
/// extract). Emits removes first, then one atomic `RippleEdit` carrying every
/// timing change (trims + shifts) with each clip's *original* timing as `old`,
/// then an `InsertClip` for the tail of a clip that spanned the whole range —
/// an order in which every intermediate state is invariant-valid.
fn clear_and_shift(
    t: &Track,
    id: SequenceId,
    track_id: TrackId,
    rs: Tick,
    re: Tick,
    delta: Tick,
) -> Vec<TimelineCmd> {
    let mut removes: Vec<TimelineCmd> = Vec::new();
    let mut changes: Vec<(ClipId, ClipTiming, ClipTiming)> = Vec::new();
    let mut tail: Option<Clip> = None;

    for c in &t.clips {
        if c.end() <= rs {
            continue; // entirely left of the range — untouched
        }
        if c.start >= re {
            // Entirely right of the range — shift wholesale by `delta`.
            if delta.0 != 0 {
                let old = ClipTiming::of(c);
                changes.push((
                    c.id,
                    old,
                    ClipTiming {
                        start: c.start + delta,
                        ..old
                    },
                ));
            }
            continue;
        }
        // `c` intersects the range.
        match (c.start < rs, c.end() > re) {
            (false, false) => {
                // Fully inside the range — remove.
                removes.push(TimelineCmd::RemoveClip {
                    seq: id,
                    track: track_id,
                    clip: Box::new(c.clone()),
                });
            }
            (true, false) => {
                // Left overhang — trim the OUT-point to `rs` (stays put).
                changes.push((c.id, ClipTiming::of(c), trim_end_to(c, rs)));
            }
            (false, true) => {
                // Right overhang — trim the IN-point to `re`, then shift by delta.
                let post = trim_start_to(c, re);
                changes.push((
                    c.id,
                    ClipTiming::of(c),
                    ClipTiming {
                        start: post.start + delta,
                        ..post
                    },
                ));
            }
            (true, true) => {
                // Spans the whole range — head stays trimmed to `[start, rs)`;
                // the tail becomes a fresh clip at `re + delta`.
                changes.push((c.id, ClipTiming::of(c), trim_end_to(c, rs)));
                let mut nt = c.clone();
                nt.id = ClipId::new();
                nt.transition_in = None;
                nt.transition_out = None;
                let post = trim_start_to(c, re);
                ClipTiming {
                    start: post.start + delta,
                    ..post
                }
                .apply_to(&mut nt);
                tail = Some(nt);
            }
        }
    }

    let mut cmds = removes;
    if !changes.is_empty() {
        cmds.push(TimelineCmd::RippleEdit {
            seq: id,
            track: track_id,
            changes,
        });
    }
    if let Some(nt) = tail {
        cmds.push(TimelineCmd::InsertClip {
            seq: id,
            track: track_id,
            clip: Box::new(nt),
        });
    }
    cmds
}

/// **Insert edit** (3-point, Premiere `,`): open a gap of `source`'s duration at
/// `at` on `target_track` — splitting any clip straddling `at` and rippling all
/// clips at/after `at` on that track RIGHT — then drop `source` into the gap.
/// The track's content grows by the source duration. Returns a batch
/// (`SplitClip?`, `RippleEdit?`, `InsertClip`) for the caller to wrap in one
/// undo step.
pub fn insert_edit(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    at: Tick,
    source: Clip,
) -> Result<Vec<TimelineCmd>, EditError> {
    let s = seq(p, id)?;
    let t = track(s, track_id)?;
    let shift = source.duration;
    if shift.0 <= 0 {
        return Err(EditError::NonPositiveDuration);
    }
    if at.0 < 0 {
        return Err(EditError::Overlap);
    }
    reject_nested_cycle(p, id, &source)?;

    let mut cmds = Vec::new();
    let mut changes: Vec<(ClipId, ClipTiming, ClipTiming)> = Vec::new();

    // 1. Split a clip straddling `at`; its right half must ripple too.
    if let Some(c) = t.clips.iter().find(|c| c.start < at && at < c.end()) {
        let new_clip_id = ClipId::new();
        cmds.push(TimelineCmd::SplitClip {
            seq: id,
            track: track_id,
            clip: c.id,
            at,
            new_clip_id,
        });
        // Post-split timing of the right half (mirrors `SplitClip::apply`, which
        // advances `source_in` by the left-half duration without speed scaling).
        let right = ClipTiming {
            start: at,
            duration: c.end() - at,
            source_in: c.source_in + (at - c.start).max(Tick::ZERO),
        };
        changes.push((
            new_clip_id,
            right,
            ClipTiming {
                start: at + shift,
                ..right
            },
        ));
    }

    // 2. Ripple every clip at/after `at` right by the source duration.
    for other in &t.clips {
        if other.start >= at {
            let old = ClipTiming::of(other);
            changes.push((
                other.id,
                old,
                ClipTiming {
                    start: other.start + shift,
                    ..old
                },
            ));
        }
    }
    if !changes.is_empty() {
        cmds.push(TimelineCmd::RippleEdit {
            seq: id,
            track: track_id,
            changes,
        });
    }

    // 3. Drop the source clip into the opened gap at `at`.
    let mut placed = source;
    placed.start = at;
    cmds.push(TimelineCmd::InsertClip {
        seq: id,
        track: track_id,
        clip: Box::new(placed),
    });

    // 4. Sync-locked sibling tracks ripple right by the same source duration in
    //    the SAME batch (14 §M-9) — an insert is a ripple op.
    cmds.extend(expand_sync_lock_ripple(p, id, track_id, at, shift));
    Ok(cmds)
}

/// **Overwrite edit** (Premiere `.`): drop `source` at `at` on `target_track`,
/// replacing whatever it covers — trimming partially-covered clips, removing
/// fully-covered ones, splitting a clip that spans the region — with NO ripple.
/// Timeline duration is unchanged unless `source` extends past the old end.
pub fn overwrite_edit(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    at: Tick,
    source: Clip,
) -> Result<Vec<TimelineCmd>, EditError> {
    let s = seq(p, id)?;
    let t = track(s, track_id)?;
    let dur = source.duration;
    if dur.0 <= 0 {
        return Err(EditError::NonPositiveDuration);
    }
    if at.0 < 0 {
        return Err(EditError::Overlap);
    }
    reject_nested_cycle(p, id, &source)?;

    // Clear `[at, at+dur)` leaving the gap (no ripple), then fill it.
    let mut cmds = clear_and_shift(t, id, track_id, at, at + dur, Tick::ZERO);
    let mut placed = source;
    placed.start = at;
    cmds.push(TimelineCmd::InsertClip {
        seq: id,
        track: track_id,
        clip: Box::new(placed),
    });
    Ok(cmds)
}

/// **Lift edit** (Premiere `;`): remove the content in `range` on `track`,
/// leaving a gap (no ripple). Timeline duration is unchanged.
pub fn lift_edit(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    range: (Tick, Tick),
) -> Result<Vec<TimelineCmd>, EditError> {
    let (rs, re) = range;
    if re <= rs {
        return Err(EditError::NonPositiveDuration);
    }
    let s = seq(p, id)?;
    let t = track(s, track_id)?;
    Ok(clear_and_shift(t, id, track_id, rs, re, Tick::ZERO))
}

/// **Extract edit** (Premiere `'`): remove the content in `range` on `track`
/// AND ripple everything after it LEFT to close the gap (generalizes
/// [`ripple_delete`]). The track's content shrinks by the range width.
pub fn extract_edit(
    p: &TimelineProject,
    id: SequenceId,
    track_id: TrackId,
    range: (Tick, Tick),
) -> Result<Vec<TimelineCmd>, EditError> {
    let (rs, re) = range;
    if re <= rs {
        return Err(EditError::NonPositiveDuration);
    }
    let s = seq(p, id)?;
    let t = track(s, track_id)?;
    // Right-of-range content closes the gap by shifting left by its width.
    let delta = Tick(rs.0 - re.0);
    let mut cmds = clear_and_shift(t, id, track_id, rs, re, delta);
    // Sync-locked sibling tracks ripple left by the same width in the SAME batch
    // (14 §M-9) — an extract is a ripple op. Their clips shift from the edit
    // point `rs`, matching the gap that closes on the edited track.
    cmds.extend(expand_sync_lock_ripple(p, id, track_id, rs, delta));
    Ok(cmds)
}

#[cfg(test)]
mod tests {
    use super::super::effect_kind::EffectKind;
    use super::super::time::FrameRate;
    use super::*;
    use crate::document::Document;
    use crate::history::Command;

    // ── move_clips: multi-select body move (04 §2.6, 210 §5) ───────────────

    /// A fixture with a contiguous run on one video track: 100..200, 200..300,
    /// 300..400. Contiguity is the point — it is what makes a naive loop over
    /// `move_clip` refuse, and what makes an unordered fan-out panic. The run
    /// starts at 100, not 0, so there is headroom to test moving *earlier*
    /// without immediately hitting the before-zero refusal.
    fn run_of_three() -> (Document, SequenceId, TrackId, Vec<ClipId>) {
        let (mut doc, seq_id, vtrack, first) = fixture();
        let mut ids = vec![first];
        {
            let project = doc.timeline.as_mut().unwrap();
            let t = project
                .sequences
                .get_mut(&seq_id)
                .unwrap()
                .track_mut(vtrack)
                .unwrap();
            t.clips.iter_mut().find(|c| c.id == first).unwrap().start = Tick(100);
            for start in [200, 300] {
                let c = Clip::new(ClipSource::Adjustment, Tick(start), Tick(100));
                ids.push(c.id);
                t.clips.push(c);
            }
        }
        (doc, seq_id, vtrack, ids)
    }

    fn starts(doc: &Document, seq_id: SequenceId, track_id: TrackId) -> Vec<i64> {
        let mut v: Vec<i64> = doc
            .timeline
            .as_ref()
            .unwrap()
            .sequences
            .get(&seq_id)
            .unwrap()
            .track(track_id)
            .unwrap()
            .clips
            .iter()
            .map(|c| c.start.0)
            .collect();
        v.sort_unstable();
        v
    }

    /// The whole selection shifts by one delta. Applying the commands in the
    /// returned order must never trip `TimelineCmd::apply`'s per-command
    /// `Sequence::validate()` debug-assert — this test runs in debug, so a
    /// wrong order panics here rather than in the field (194 K-A5's hazard).
    #[test]
    fn move_clips_shifts_a_contiguous_run_without_a_transient_overlap() {
        let (mut doc, seq_id, vtrack, ids) = run_of_three();
        let moving: Vec<_> = ids.iter().map(|c| (vtrack, *c)).collect();
        let project = doc.timeline.as_ref().unwrap();
        let cmds = move_clips(project, seq_id, &moving, Tick(50), 0).unwrap();
        assert_eq!(cmds.len(), 3);
        for c in &cmds {
            c.apply(&mut doc);
        }
        assert_eq!(starts(&doc, seq_id, vtrack), vec![150, 250, 350]);
    }

    /// Same, moving earlier: the order flips to leading-edge-first.
    #[test]
    fn move_clips_shifts_a_contiguous_run_earlier() {
        let (mut doc, seq_id, vtrack, ids) = run_of_three();
        let moving: Vec<_> = ids.iter().map(|c| (vtrack, *c)).collect();
        let project = doc.timeline.as_ref().unwrap();
        let cmds = move_clips(project, seq_id, &moving, Tick(-100), 0).unwrap();
        for c in &cmds {
            c.apply(&mut doc);
        }
        assert_eq!(starts(&doc, seq_id, vtrack), vec![0, 100, 200]);
    }

    /// Emission order is the safe one, not the selection's order. Moving later
    /// must start from the trailing clip.
    #[test]
    fn move_clips_orders_trailing_edge_first_when_moving_later() {
        let (doc, seq_id, vtrack, ids) = run_of_three();
        let moving: Vec<_> = ids.iter().map(|c| (vtrack, *c)).collect();
        let project = doc.timeline.as_ref().unwrap();

        let later = move_clips(project, seq_id, &moving, Tick(50), 0).unwrap();
        let order: Vec<i64> = later
            .iter()
            .map(|c| match c {
                TimelineCmd::MoveClip { old_start, .. } => old_start.0,
                other => panic!("expected MoveClip, got {other:?}"),
            })
            .collect();
        assert_eq!(order, vec![300, 200, 100], "trailing edge first");

        let earlier = move_clips(project, seq_id, &moving, Tick(-50), 0).unwrap();
        let order: Vec<i64> = earlier
            .iter()
            .map(|c| match c {
                TimelineCmd::MoveClip { old_start, .. } => old_start.0,
                other => panic!("expected MoveClip, got {other:?}"),
            })
            .collect();
        assert_eq!(order, vec![100, 200, 300], "leading edge first");
    }

    /// A clip the selection is vacating is not an obstacle — this is the case
    /// a loop over `move_clip` cannot express, since each call would see its
    /// neighbour still in place.
    #[test]
    fn move_clips_ignores_collisions_within_the_moving_set() {
        let (doc, seq_id, vtrack, ids) = run_of_three();
        // Move only the first two into space the third is NOT vacating would
        // collide; move all three and it is fine.
        let all: Vec<_> = ids.iter().map(|c| (vtrack, *c)).collect();
        let project = doc.timeline.as_ref().unwrap();
        assert!(move_clips(project, seq_id, &all, Tick(100), 0).is_ok());

        let leading: Vec<_> = ids[..2].iter().map(|c| (vtrack, *c)).collect();
        assert_eq!(
            move_clips(project, seq_id, &leading, Tick(100), 0),
            Err(EditError::Overlap),
            "the third clip stays put, so the second has nowhere to land"
        );
    }

    /// A stationary clip blocks the move, and nothing partial is emitted.
    #[test]
    fn move_clips_refuses_when_a_stationary_clip_blocks() {
        let (mut doc, seq_id, vtrack, ids) = run_of_three();
        {
            let project = doc.timeline.as_mut().unwrap();
            let t = project
                .sequences
                .get_mut(&seq_id)
                .unwrap()
                .track_mut(vtrack)
                .unwrap();
            t.clips
                .push(Clip::new(ClipSource::Adjustment, Tick(420), Tick(50)));
        }
        let moving: Vec<_> = ids.iter().map(|c| (vtrack, *c)).collect();
        let project = doc.timeline.as_ref().unwrap();
        assert_eq!(
            move_clips(project, seq_id, &moving, Tick(50), 0),
            Err(EditError::Overlap),
            "the trailing clip would land on the stationary one at 420"
        );
    }

    /// Moving before zero is refused for the whole set, not clamped — clamping
    /// would silently collapse the spacing the selection had.
    #[test]
    fn move_clips_refuses_a_move_before_zero() {
        let (doc, seq_id, vtrack, ids) = run_of_three();
        let moving: Vec<_> = ids.iter().map(|c| (vtrack, *c)).collect();
        let project = doc.timeline.as_ref().unwrap();
        // The run starts at 100, so -101 takes its leading clip to -1.
        assert_eq!(
            move_clips(project, seq_id, &moving, Tick(-101), 0),
            Err(EditError::Overlap)
        );
        // ...and the whole set is refused, not just the offending clip.
        assert!(move_clips(project, seq_id, &moving, Tick(-100), 0).is_ok());
    }

    /// Clips on different tracks each keep their own track and shift together.
    #[test]
    fn move_clips_spans_tracks_keeping_each_clip_on_its_own() {
        let (mut doc, seq_id, vtrack, vclip) = fixture();
        let (atrack, aclip) = add_audio_clip(&mut doc, seq_id);
        let moving = vec![(vtrack, vclip), (atrack, aclip)];
        let project = doc.timeline.as_ref().unwrap();
        let cmds = move_clips(project, seq_id, &moving, Tick(75), 0).unwrap();
        for c in &cmds {
            c.apply(&mut doc);
        }
        assert_eq!(starts(&doc, seq_id, vtrack), vec![75]);
        assert_eq!(starts(&doc, seq_id, atrack), vec![75]);
    }

    /// A zero delta is not an undo entry.
    #[test]
    fn move_clips_zero_delta_emits_nothing() {
        let (doc, seq_id, vtrack, ids) = run_of_three();
        let moving: Vec<_> = ids.iter().map(|c| (vtrack, *c)).collect();
        let project = doc.timeline.as_ref().unwrap();
        assert!(move_clips(project, seq_id, &moving, Tick(0), 0)
            .unwrap()
            .is_empty());
        assert!(move_clips(project, seq_id, &[], Tick(50), 0)
            .unwrap()
            .is_empty());
    }

    /// Two clips on adjacent video lanes share one vertical track_delta and
    /// land on the next lanes without a transient validate panic (210 residual).
    #[test]
    fn move_clips_vertical_track_offset_shifts_each_lane() {
        let (mut doc, seq_id, v1, c1) = fixture();
        let v2 = {
            let project = doc.timeline.as_mut().unwrap();
            let seq = project.sequences.get_mut(&seq_id).unwrap();
            let mut t = Track::new(TrackKind::Video, "V2");
            let id = t.id;
            let c = Clip::new(ClipSource::Adjustment, Tick(0), Tick(100));
            t.clips.push(c);
            seq.video_tracks.push(t);
            id
        };
        let c2 = {
            let project = doc.timeline.as_ref().unwrap();
            project.sequences[&seq_id].track(v2).unwrap().clips[0].id
        };
        let v3 = {
            let project = doc.timeline.as_mut().unwrap();
            let seq = project.sequences.get_mut(&seq_id).unwrap();
            let t = Track::new(TrackKind::Video, "V3");
            let id = t.id;
            seq.video_tracks.push(t);
            id
        };
        let moving = vec![(v1, c1), (v2, c2)];
        let project = doc.timeline.as_ref().unwrap();
        let cmds = move_clips(project, seq_id, &moving, Tick(0), 1).unwrap();
        assert_eq!(cmds.len(), 2);
        // Higher source lane first so V2→V3 vacates before V1→V2 lands.
        match &cmds[0] {
            TimelineCmd::MoveClip {
                clip,
                new_track,
                track,
                ..
            } => {
                assert_eq!(*clip, c2);
                assert_eq!(*track, v2);
                assert_eq!(*new_track, Some(v3));
            }
            other => panic!("expected MoveClip, got {other:?}"),
        }
        for c in &cmds {
            c.apply(&mut doc);
        }
        let project = doc.timeline.as_ref().unwrap();
        let seq = &project.sequences[&seq_id];
        assert!(seq.track(v1).unwrap().clips.is_empty(), "V1 vacated");
        assert_eq!(
            seq.track(v2)
                .unwrap()
                .clips
                .iter()
                .map(|c| c.id)
                .collect::<Vec<_>>(),
            vec![c1]
        );
        assert_eq!(
            seq.track(v3)
                .unwrap()
                .clips
                .iter()
                .map(|c| c.id)
                .collect::<Vec<_>>(),
            vec![c2]
        );
    }

    /// Vertical + horizontal in one batch keeps relative spacing and track
    /// offsets; applying in emission order never trips Sequence::validate.
    #[test]
    fn move_clips_vertical_and_horizontal_together() {
        let (mut doc, seq_id, v1, c1) = fixture();
        // c1 starts at 0; move later by 50 and up one lane.
        let v2 = {
            let project = doc.timeline.as_mut().unwrap();
            let seq = project.sequences.get_mut(&seq_id).unwrap();
            let t = Track::new(TrackKind::Video, "V2");
            let id = t.id;
            seq.video_tracks.push(t);
            id
        };
        let moving = vec![(v1, c1)];
        let project = doc.timeline.as_ref().unwrap();
        let cmds = move_clips(project, seq_id, &moving, Tick(50), 1).unwrap();
        for c in &cmds {
            c.apply(&mut doc);
        }
        let project = doc.timeline.as_ref().unwrap();
        let clip = project.sequences[&seq_id]
            .track(v2)
            .unwrap()
            .clips
            .iter()
            .find(|c| c.id == c1)
            .expect("clip landed on V2");
        assert_eq!(clip.start, Tick(50));
    }

    /// An out-of-range track_delta for a kind is time-only for that kind (no
    /// half-applied vertical). With zero time delta the move is a no-op.
    #[test]
    fn move_clips_out_of_range_track_delta_is_time_only_noop() {
        let (doc, seq_id, vtrack, ids) = run_of_three();
        let moving: Vec<_> = ids.iter().map(|c| (vtrack, *c)).collect();
        let project = doc.timeline.as_ref().unwrap();
        assert!(
            move_clips(project, seq_id, &moving, Tick(0), 1)
                .unwrap()
                .is_empty(),
            "pure-vertical OOR must not emit commands"
        );
        assert!(move_clips(project, seq_id, &moving, Tick(0), -1)
            .unwrap()
            .is_empty());
        // Time still applies when the vertical half is dropped.
        let cmds = move_clips(project, seq_id, &moving, Tick(50), 1).unwrap();
        assert_eq!(cmds.len(), 3);
        for c in &cmds {
            match c {
                TimelineCmd::MoveClip { new_track, .. } => {
                    assert!(new_track.is_none(), "OOR vertical must not reassign")
                }
                other => panic!("expected MoveClip, got {other:?}"),
            }
        }
    }

    /// Linked A/V pair: videos take track_delta; the audio partner stays on its
    /// own track (time-only) — same contract as single-clip cross-track.
    #[test]
    fn move_clips_vertical_keeps_linked_audio_on_its_track() {
        let (mut doc, seq_id, v1, c1) = fixture();
        let v2 = {
            let project = doc.timeline.as_mut().unwrap();
            let seq = project.sequences.get_mut(&seq_id).unwrap();
            let t = Track::new(TrackKind::Video, "V2");
            let id = t.id;
            seq.video_tracks.push(t);
            id
        };
        let (atrack, aclip) = add_audio_clip(&mut doc, seq_id);
        // Link V1 clip to the audio partner (same shape as GUI partner fold-in).
        {
            let project = doc.timeline.as_mut().unwrap();
            let seq = project.sequences.get_mut(&seq_id).unwrap();
            let group = LinkGroupId::new();
            seq.track_mut(v1)
                .unwrap()
                .clips
                .iter_mut()
                .find(|c| c.id == c1)
                .unwrap()
                .link_group = Some(group);
            seq.track_mut(atrack)
                .unwrap()
                .clips
                .iter_mut()
                .find(|c| c.id == aclip)
                .unwrap()
                .link_group = Some(group);
        }
        // Moving set as ops_bridge builds it: video + folded audio partner.
        let moving = vec![(v1, c1), (atrack, aclip)];
        let project = doc.timeline.as_ref().unwrap();
        let cmds = move_clips(project, seq_id, &moving, Tick(25), 1).unwrap();
        assert_eq!(cmds.len(), 2, "both members emit (time and/or track)");
        for c in &cmds {
            c.apply(&mut doc);
        }
        let project = doc.timeline.as_ref().unwrap();
        let seq = &project.sequences[&seq_id];
        // Video reassigned V1 → V2 with +25 ticks.
        assert!(seq.track(v1).unwrap().clips.iter().all(|c| c.id != c1));
        let v = seq
            .track(v2)
            .unwrap()
            .clips
            .iter()
            .find(|c| c.id == c1)
            .expect("video on V2");
        assert_eq!(v.start, Tick(25));
        // Audio stayed on A1, time-only.
        let a = seq
            .track(atrack)
            .unwrap()
            .clips
            .iter()
            .find(|c| c.id == aclip)
            .expect("audio stays on its track");
        assert_eq!(a.start, Tick(25));
    }

    /// One undo unit restores a vertical multi-move (via Batch invert).
    #[test]
    fn move_clips_vertical_undo_restores() {
        let (mut doc, seq_id, v1, c1) = fixture();
        let (v2, c2) = {
            let project = doc.timeline.as_mut().unwrap();
            let seq = project.sequences.get_mut(&seq_id).unwrap();
            let mut t = Track::new(TrackKind::Video, "V2");
            let tid = t.id;
            let c = Clip::new(ClipSource::Adjustment, Tick(10), Tick(80));
            let cid = c.id;
            t.clips.push(c);
            seq.video_tracks.push(t);
            // spare landing lane
            seq.video_tracks.push(Track::new(TrackKind::Video, "V3"));
            (tid, cid)
        };
        let before = doc.timeline.clone();
        let project = doc.timeline.as_ref().unwrap();
        let cmds = move_clips(project, seq_id, &[(v1, c1), (v2, c2)], Tick(0), 1).unwrap();
        let batch = if cmds.len() == 1 {
            cmds.into_iter().next().unwrap()
        } else {
            // Apply as the GUI does: each cmd, then invert via history style.
            for c in &cmds {
                c.apply(&mut doc);
            }
            // Rebuild inverse by applying inverses in reverse order.
            let mut invs = Vec::new();
            for c in cmds.iter().rev() {
                invs.push(c.inverse(&doc).expect("invert"));
            }
            for inv in invs {
                inv.apply(&mut doc);
            }
            assert_eq!(doc.timeline, before, "vertical multi-move must fully undo");
            return;
        };
        batch.apply(&mut doc);
        let inv = batch.inverse(&doc).unwrap();
        inv.apply(&mut doc);
        assert_eq!(doc.timeline, before);
    }

    #[test]
    fn insert_space_shifts_later_clips_on_all_unlocked_tracks() {
        let (mut doc, seq_id, vtrack, vclip) = fixture();
        // Second clip on V1 after a gap, plus an audio partner.
        {
            let project = doc.timeline.as_mut().unwrap();
            let seq = project.sequences.get_mut(&seq_id).unwrap();
            let t = seq.track_mut(vtrack).unwrap();
            t.clips
                .push(Clip::new(ClipSource::Adjustment, Tick(200), Tick(50)));
        }
        let (atrack, aclip) = add_audio_clip(&mut doc, seq_id);
        {
            // Move audio clip to start=200 so it sits after the insert point.
            let project = doc.timeline.as_mut().unwrap();
            let seq = project.sequences.get_mut(&seq_id).unwrap();
            let t = seq.track_mut(atrack).unwrap();
            t.clips.iter_mut().find(|c| c.id == aclip).unwrap().start = Tick(200);
        }
        let project = doc.timeline.as_ref().unwrap();
        let cmds = insert_space(project, seq_id, Tick(100), Tick(50)).unwrap();
        assert!(!cmds.is_empty());
        for c in &cmds {
            c.apply(&mut doc);
        }
        let project = doc.timeline.as_ref().unwrap();
        let seq = project.sequences.get(&seq_id).unwrap();
        // First V clip at 0 is before the point — unmoved.
        let v0 = seq
            .track(vtrack)
            .unwrap()
            .clips
            .iter()
            .find(|c| c.id == vclip)
            .unwrap();
        assert_eq!(v0.start, Tick(0));
        // Later V clip 200 → 250.
        let v1 = seq
            .track(vtrack)
            .unwrap()
            .clips
            .iter()
            .find(|c| c.start == Tick(250) || c.duration == Tick(50))
            .unwrap();
        assert_eq!(v1.start, Tick(250));
        // Audio at 200 → 250.
        let a = seq
            .track(atrack)
            .unwrap()
            .clips
            .iter()
            .find(|c| c.id == aclip)
            .unwrap();
        assert_eq!(a.start, Tick(250));
    }

    #[test]
    fn insert_space_skips_locked_tracks() {
        let (mut doc, seq_id, vtrack, _vclip) = fixture();
        {
            let project = doc.timeline.as_mut().unwrap();
            let seq = project.sequences.get_mut(&seq_id).unwrap();
            let t = seq.track_mut(vtrack).unwrap();
            t.clips
                .push(Clip::new(ClipSource::Adjustment, Tick(200), Tick(50)));
            t.locked = true;
        }
        let project = doc.timeline.as_ref().unwrap();
        let cmds = insert_space(project, seq_id, Tick(100), Tick(50)).unwrap();
        // Locked track contributes nothing.
        assert!(cmds.is_empty());
    }

    #[test]
    fn remove_space_closes_shared_gap() {
        let (mut doc, seq_id, vtrack, _) = fixture();
        {
            let project = doc.timeline.as_mut().unwrap();
            let seq = project.sequences.get_mut(&seq_id).unwrap();
            // First clip [0,100); second [200,250) — 100-tick gap at 100.
            let t = seq.track_mut(vtrack).unwrap();
            t.clips
                .push(Clip::new(ClipSource::Adjustment, Tick(200), Tick(50)));
        }
        let project = doc.timeline.as_ref().unwrap();
        assert_eq!(
            space_available_after(project, seq_id, Tick(100)),
            Some(Tick(100))
        );
        let cmds = remove_space(project, seq_id, Tick(100), Tick(100)).unwrap();
        for c in &cmds {
            c.apply(&mut doc);
        }
        let second = doc
            .timeline
            .as_ref()
            .unwrap()
            .sequences
            .get(&seq_id)
            .unwrap()
            .track(vtrack)
            .unwrap()
            .clips
            .iter()
            .find(|c| c.duration == Tick(50))
            .unwrap();
        assert_eq!(second.start, Tick(100));
    }

    #[test]
    fn remove_space_refuses_when_clip_covers_point() {
        let (doc, seq_id, _, _) = fixture();
        // fixture clip covers [0,100); point 50 is mid-clip.
        let project = doc.timeline.as_ref().unwrap();
        assert_eq!(
            space_available_after(project, seq_id, Tick(50)),
            Some(Tick(0))
        );
        assert!(matches!(
            remove_space(project, seq_id, Tick(50), Tick(10)),
            Err(EditError::Overlap) | Ok(_)
        ));
        // With zero available, remove_space returns Ok(empty) when amount>0
        // and avail==0 after the early return path — pin the contract:
        // covering → Some(0); amount>0 → either Overlap or empty.
        let r = remove_space(project, seq_id, Tick(50), Tick(10));
        assert!(
            matches!(r, Ok(ref v) if v.is_empty()) || matches!(r, Err(EditError::Overlap)),
            "{r:?}"
        );
    }

    #[test]
    fn remove_all_spaces_after_packs_track() {
        let (mut doc, seq_id, vtrack, _) = fixture();
        {
            let project = doc.timeline.as_mut().unwrap();
            let seq = project.sequences.get_mut(&seq_id).unwrap();
            let t = seq.track_mut(vtrack).unwrap();
            // [0,100) already; add [150,180) and [250,280).
            t.clips
                .push(Clip::new(ClipSource::Adjustment, Tick(150), Tick(30)));
            t.clips
                .push(Clip::new(ClipSource::Adjustment, Tick(250), Tick(30)));
        }
        let project = doc.timeline.as_ref().unwrap();
        // Pack from tick 100: clips at 150 and 250 → 100 and 130.
        let cmds = remove_all_spaces_after(project, seq_id, Tick(100)).unwrap();
        for c in &cmds {
            c.apply(&mut doc);
        }
        let starts: Vec<i64> = doc
            .timeline
            .as_ref()
            .unwrap()
            .sequences
            .get(&seq_id)
            .unwrap()
            .track(vtrack)
            .unwrap()
            .clips
            .iter()
            .map(|c| c.start.0)
            .collect();
        // Original first clip stays at 0; the two later ones pack from 100.
        assert!(starts.contains(&0));
        assert!(starts.contains(&100), "starts={starts:?}");
        assert!(starts.contains(&130), "starts={starts:?}");
        assert!(!starts.contains(&150));
        assert!(!starts.contains(&250));
    }

    #[test]
    fn remove_clips_after_deletes_only_later() {
        let (mut doc, seq_id, vtrack, vclip) = fixture();
        let later_id;
        {
            let project = doc.timeline.as_mut().unwrap();
            let seq = project.sequences.get_mut(&seq_id).unwrap();
            let t = seq.track_mut(vtrack).unwrap();
            let c = Clip::new(ClipSource::Adjustment, Tick(200), Tick(50));
            later_id = c.id;
            t.clips.push(c);
        }
        let project = doc.timeline.as_ref().unwrap();
        let cmds = remove_clips_after(project, seq_id, Tick(150)).unwrap();
        assert_eq!(cmds.len(), 1);
        for c in &cmds {
            c.apply(&mut doc);
        }
        let ids: Vec<_> = doc
            .timeline
            .as_ref()
            .unwrap()
            .sequences
            .get(&seq_id)
            .unwrap()
            .track(vtrack)
            .unwrap()
            .clips
            .iter()
            .map(|c| c.id)
            .collect();
        assert!(ids.contains(&vclip));
        assert!(!ids.contains(&later_id));
    }

    #[test]
    fn freeze_frame_sets_zero_speed_and_source_in() {
        let (mut doc, seq_id, track_id, clip_id) = fixture();
        {
            let project = doc.timeline.as_mut().unwrap();
            let clip = project
                .sequences
                .get_mut(&seq_id)
                .unwrap()
                .track_mut(track_id)
                .unwrap()
                .clips
                .iter_mut()
                .find(|c| c.id == clip_id)
                .unwrap();
            // 2× speed from source_in=100: at clip-rel 50 → source 200.
            clip.source_in = Tick(100);
            clip.speed = SpeedMap::Constant(Ratio::new(2, 1));
            clip.duration = Tick(100);
        }
        let project = doc.timeline.as_ref().unwrap();
        let cmd = freeze_frame(project, seq_id, track_id, clip_id, Tick(50))
            .unwrap()
            .expect("should produce a command");
        cmd.apply(&mut doc);
        let c = doc
            .timeline
            .as_ref()
            .unwrap()
            .sequences
            .get(&seq_id)
            .unwrap()
            .track(track_id)
            .unwrap()
            .clips
            .iter()
            .find(|c| c.id == clip_id)
            .unwrap();
        assert_eq!(c.source_in, Tick(200));
        assert_eq!(c.speed, SpeedMap::Constant(Ratio::new(0, 1)));
        // Zero-rate source delta is zero for any timeline span.
        assert_eq!(c.speed.source_delta(Tick(1_000_000)), Tick(0));
        // Already frozen at the same frame → no-op.
        let project = doc.timeline.as_ref().unwrap();
        assert!(freeze_frame(project, seq_id, track_id, clip_id, Tick(0))
            .unwrap()
            .is_none());
    }

    #[test]
    fn freeze_frame_clamps_out_of_range_at() {
        let (doc, seq_id, track_id, clip_id) = fixture();
        let project = doc.timeline.as_ref().unwrap();
        let c = project
            .sequences
            .get(&seq_id)
            .unwrap()
            .track(track_id)
            .unwrap()
            .clips
            .iter()
            .find(|c| c.id == clip_id)
            .unwrap();
        let dur = c.duration;
        // Past the out-point freezes the last frame (duration-1), not an error.
        let cmd = freeze_frame(project, seq_id, track_id, clip_id, dur + Tick(999))
            .unwrap()
            .expect("clamped freeze");
        match cmd {
            TimelineCmd::SetClipProp { new, .. } => {
                assert_eq!(new.speed, SpeedMap::Constant(Ratio::new(0, 1)));
                // Default fixture speed is 1×, source_in 0 → freeze at duration-1.
                assert_eq!(new.source_in, Tick((dur.0 - 1).max(0)));
            }
            other => panic!("expected SetClipProp, got {other:?}"),
        }
    }

    #[test]
    fn freeze_frame_inverse_restores_speed() {
        let (mut doc, seq_id, track_id, clip_id) = fixture();
        {
            let project = doc.timeline.as_mut().unwrap();
            let clip = project
                .sequences
                .get_mut(&seq_id)
                .unwrap()
                .track_mut(track_id)
                .unwrap()
                .clips
                .iter_mut()
                .find(|c| c.id == clip_id)
                .unwrap();
            clip.source_in = Tick(40);
            clip.speed = SpeedMap::Constant(Ratio::new(1, 1));
        }
        let project = doc.timeline.as_ref().unwrap();
        let cmd = freeze_frame(project, seq_id, track_id, clip_id, Tick(10))
            .unwrap()
            .unwrap();
        let inv = cmd.inverse(&doc).expect("SetClipProp always inverts");
        cmd.apply(&mut doc);
        let frozen = doc
            .timeline
            .as_ref()
            .unwrap()
            .sequences
            .get(&seq_id)
            .unwrap()
            .track(track_id)
            .unwrap()
            .clips
            .iter()
            .find(|c| c.id == clip_id)
            .unwrap();
        assert_eq!(frozen.source_in, Tick(50)); // 40 + 10×1
        assert_eq!(frozen.speed, SpeedMap::Constant(Ratio::new(0, 1)));
        inv.apply(&mut doc);
        let after = doc
            .timeline
            .as_ref()
            .unwrap()
            .sequences
            .get(&seq_id)
            .unwrap()
            .track(track_id)
            .unwrap()
            .clips
            .iter()
            .find(|c| c.id == clip_id)
            .unwrap();
        assert_eq!(after.source_in, Tick(40));
        assert_eq!(after.speed, SpeedMap::Constant(Ratio::ONE));
    }

    #[test]
    fn split_av_link_unlinks_both_members() {
        let (mut doc, seq_id, vtrack, vclip) = fixture();
        let (atrack, aclip) = add_audio_clip(&mut doc, seq_id);
        let project = doc.timeline.as_ref().unwrap();
        let cmds = link_clips(project, seq_id, vtrack, vclip, atrack, aclip).unwrap();
        for c in cmds {
            c.apply(&mut doc);
        }
        let project = doc.timeline.as_ref().unwrap();
        let group = find_clip(&doc, seq_id, vtrack, vclip).link_group;
        assert!(group.is_some());
        let split = split_av_link(project, seq_id, vtrack, vclip).unwrap();
        assert_eq!(split.len(), 2);
        for c in split {
            c.apply(&mut doc);
        }
        assert!(find_clip(&doc, seq_id, vtrack, vclip).link_group.is_none());
        assert!(find_clip(&doc, seq_id, atrack, aclip).link_group.is_none());
    }

    #[test]
    fn add_effect_scoped_honours_applicability() {
        use super::super::effect_manifest::Applicability;
        // Unit: CLIP_ONLY vs ALL_SCOPES on every owner shape.
        let track = VfxOwner::Track(TrackId::nil());
        let clip = VfxOwner::Clip(ClipId::nil());
        let master = VfxOwner::Master(SequenceId::nil());
        let asset = VfxOwner::Asset(AssetId::nil());
        assert!(Applicability::CLIP_ONLY.allows(clip));
        assert!(!Applicability::CLIP_ONLY.allows(track));
        assert!(!Applicability::CLIP_ONLY.allows(master));
        assert!(!Applicability::CLIP_ONLY.allows(asset));
        for o in [clip, track, master, asset] {
            assert!(Applicability::ALL_SCOPES.allows(o));
        }
        let (doc, seq, track_id, clip_id, asset_id) = scoped_fixture();
        let p = doc.timeline.as_ref().unwrap();
        // Live path under ALL_SCOPES catalogue: all four scopes accept Blur.
        assert!(add_effect_scoped(p, VfxOwner::Clip(clip_id), fx(EffectKind::Blur), None).is_ok());
        assert!(
            add_effect_scoped(p, VfxOwner::Track(track_id), fx(EffectKind::Blur), None).is_ok()
        );
        assert!(add_effect_scoped(p, VfxOwner::Master(seq), fx(EffectKind::Blur), None).is_ok());
        assert!(
            add_effect_scoped(p, VfxOwner::Asset(asset_id), fx(EffectKind::Blur), None).is_ok()
        );
        // Unmanifested id: no gate (forward-compat).
        let mut unknown = fx(EffectKind::Blur);
        unknown.id = super::super::effect_manifest::EffectId::new("future.fx".to_string());
        assert!(add_effect_scoped(p, VfxOwner::Track(track_id), unknown, None).is_ok());
    }

    #[test]
    fn media_tag_registry_resolves_names_to_ids() {
        let (mut doc, _seq, _track, _clip, asset) = scoped_fixture();
        let p = doc.timeline.as_ref().unwrap();
        let cmds = set_asset_tags_resolved(
            p,
            asset,
            vec!["Hero".into(), "hero".into(), "B-roll".into()],
        )
        .unwrap();
        // One AddMediaTag per unique name (case-insensitive) + SetAssetTags + SetAssetTagIds.
        assert!(
            cmds.iter()
                .filter(|c| matches!(c, TimelineCmd::AddMediaTag { .. }))
                .count()
                >= 2
        );
        for c in cmds {
            Command::Timeline(c).apply(&mut doc);
        }
        let p = doc.timeline.as_ref().unwrap();
        assert_eq!(p.media_tags.len(), 2);
        let a = p.media.assets.get(&asset).unwrap();
        assert_eq!(a.tag_ids.len(), 2);
        assert!(a.tags.iter().any(|t| t.eq_ignore_ascii_case("Hero")));
        // Second resolve reuses registry, no new tags.
        let again = set_asset_tags_resolved(p, asset, vec!["Hero".into()]).unwrap();
        assert!(
            !again
                .iter()
                .any(|c| matches!(c, TimelineCmd::AddMediaTag { .. })),
            "existing tag must not re-create"
        );
    }

    #[test]
    fn set_asset_rating_and_unused_assets() {
        use super::super::media::{AssetKind, AssetSource, MediaAsset};
        use std::path::PathBuf;

        let mut project = TimelineProject::new();
        let used = MediaAsset::new(
            AssetKind::Video,
            AssetSource::File {
                path: PathBuf::from("/a.mp4"),
                rel_path: None,
            },
        );
        let used_id = used.id;
        let free = MediaAsset::new(
            AssetKind::Video,
            AssetSource::File {
                path: PathBuf::from("/b.mp4"),
                rel_path: None,
            },
        );
        let free_id = free.id;
        project.media.insert(used);
        project.media.insert(free);

        let mut seq = Sequence::new("S", FrameRate::FPS_30, 320, 180);
        let mut track = Track::new(TrackKind::Video, "V1");
        track.clips.push(Clip::new(
            ClipSource::Asset { asset: used_id },
            Tick(0),
            Tick(100),
        ));
        seq.video_tracks.push(track);
        project.insert_sequence(seq);

        let unused = unused_assets(&project);
        assert!(unused.contains(&free_id));
        assert!(!unused.contains(&used_id));

        let mut doc = Document::new("t", 1.0, 1.0);
        doc.timeline = Some(project);
        let project = doc.timeline.as_ref().unwrap();
        let cmd = set_asset_rating(project, used_id, Some(4)).unwrap();
        cmd.apply(&mut doc);
        assert_eq!(
            doc.timeline.as_ref().unwrap().media.assets[&used_id].rating,
            Some(4)
        );

        let inv = cmd.inverse(&doc).unwrap();
        inv.apply(&mut doc);
        assert_eq!(
            doc.timeline.as_ref().unwrap().media.assets[&used_id].rating,
            None
        );

        // Out of range clears.
        doc.timeline
            .as_mut()
            .unwrap()
            .media
            .assets
            .get_mut(&used_id)
            .unwrap()
            .rating = Some(3);
        let cmd = set_asset_rating(doc.timeline.as_ref().unwrap(), used_id, Some(9)).unwrap();
        cmd.apply(&mut doc);
        assert_eq!(
            doc.timeline.as_ref().unwrap().media.assets[&used_id].rating,
            None
        );

        let removed = remove_unused_assets(doc.timeline.as_ref().unwrap());
        assert_eq!(removed.len(), 1);
    }

    #[test]
    fn create_subclip_shares_hash_and_sets_range() {
        use super::super::media::{AssetKind, AssetSource, MediaAsset};
        use std::path::PathBuf;

        let mut project = TimelineProject::new();
        let mut parent = MediaAsset::new(
            AssetKind::Video,
            AssetSource::File {
                path: PathBuf::from("/long-take.mp4"),
                rel_path: None,
            },
        );
        parent.content_hash = Some("xxh3:deadbeef".into());
        let parent_id = parent.id;
        project.media.insert(parent);

        let (cmd, child_id) = create_subclip(
            &project,
            parent_id,
            (Tick(100), Tick(500)),
            Some("select-A".into()),
        )
        .unwrap();
        let mut doc = Document::new("t", 1.0, 1.0);
        doc.timeline = Some(project);
        cmd.apply(&mut doc);
        let child = &doc.timeline.as_ref().unwrap().media.assets[&child_id];
        assert_eq!(child.parent, Some(parent_id));
        assert_eq!(child.subclip_range, Some((Tick(100), Tick(500))));
        assert_eq!(child.content_hash.as_deref(), Some("xxh3:deadbeef"));
        assert!(child.is_subclip());
        let (sin, dur) = subclip_default_timing(child).unwrap();
        assert_eq!(sin, Tick(100));
        assert_eq!(dur, Tick(400));
        // Nested subclip refused.
        assert!(matches!(
            create_subclip(
                doc.timeline.as_ref().unwrap(),
                child_id,
                (Tick(0), Tick(10)),
                None
            ),
            Err(EditError::Overlap)
        ));
        assert!(matches!(
            create_subclip(
                doc.timeline.as_ref().unwrap(),
                parent_id,
                (Tick(10), Tick(10)),
                None
            ),
            Err(EditError::NonPositiveDuration)
        ));
    }

    #[test]
    fn unused_assets_counts_every_non_clip_reference_class() {
        use super::super::grade::{GradeOp, GradeOpKind, GradeOpParams, LutInterp};
        use super::super::graph::{GraphNode, GraphOp, NodeGraph};
        use super::super::media::{AssetKind, AssetSource, MediaAsset};
        use std::path::PathBuf;

        let mut project = TimelineProject::new();

        let mut new_asset = |kind: AssetKind, name: &str| {
            let a = MediaAsset::new(
                kind,
                AssetSource::File {
                    path: PathBuf::from(format!("/{name}")),
                    rel_path: None,
                },
            );
            let id = a.id;
            project.media.insert(a);
            id
        };

        // A LUT is referenced *only* by a grade op — never by a clip source — so
        // a clip-only scan reported every one of these as unused and deleted it.
        let lut_clip = new_asset(AssetKind::Lut3d, "clip.cube");
        let lut_track = new_asset(AssetKind::Lut3d, "track.cube");
        let lut_master = new_asset(AssetKind::Lut3d, "master.cube");
        let lut_asset_scope = new_asset(AssetKind::Lut3d, "asset.cube");
        let lut_graph_op = new_asset(AssetKind::Lut3d, "graph_op.cube");
        let lut_graph_grade = new_asset(AssetKind::Lut3d, "graph_grade.cube");
        let graph_media = new_asset(AssetKind::Video, "graph_media.mp4");
        let clip_media = new_asset(AssetKind::Video, "clip.mp4");
        let graded_asset = new_asset(AssetKind::Video, "graded.mp4");
        let orphan = new_asset(AssetKind::Video, "orphan.mp4");

        let lut_grade = |asset: AssetId| Grade {
            ops: vec![GradeOp::new(
                GradeOpKind::Lut3d,
                GradeOpParams::Lut3d {
                    asset,
                    intensity: 1.0,
                    interp: LutInterp::Trilinear,
                },
            )],
            bypass: false,
        };

        let mut clip = Clip::new(ClipSource::Asset { asset: clip_media }, Tick(0), Tick(100));
        clip.grade = Some(lut_grade(lut_clip));
        let mut track = Track::new(TrackKind::Video, "V1");
        track.clips.push(clip);
        track.grade = Some(lut_grade(lut_track));
        let mut seq = Sequence::new("S", FrameRate::FPS_30, 320, 180);
        seq.video_tracks.push(track);
        seq.master_grade = Some(lut_grade(lut_master));
        project.insert_sequence(seq);

        // Asset scope: a grade bound beneath every clip using `graded_asset`.
        project.media.assets.get_mut(&graded_asset).unwrap().grade =
            Some(lut_grade(lut_asset_scope));

        // Graph arena: `MediaIn` / `Lut` name assets directly; `Grade` embeds a
        // whole stack that can itself carry a `Lut3d`.
        let (mut graph, _clip_in) = NodeGraph::new_clip_composition("g");
        for op in [
            GraphOp::MediaIn {
                asset: graph_media,
                time_source: Default::default(),
            },
            GraphOp::Lut {
                asset: lut_graph_op,
            },
            GraphOp::Grade {
                grade: lut_grade(lut_graph_grade),
            },
        ] {
            let node = GraphNode::new(op);
            graph.nodes.insert(node.id, node);
        }
        project.graphs.insert(graph.id, graph);

        let unused = unused_assets(&project);

        for (id, what) in [
            (lut_clip, "clip grade LUT"),
            (lut_track, "track grade LUT"),
            (lut_master, "master grade LUT"),
            (lut_asset_scope, "asset grade LUT"),
            (lut_graph_op, "graph Lut op"),
            (lut_graph_grade, "graph embedded grade LUT"),
            (graph_media, "graph MediaIn"),
            (clip_media, "clip source"),
        ] {
            assert!(!unused.contains(&id), "{what} must not be reported unused");
        }

        // `graded_asset` carries a grade but nothing references it, so it is
        // genuinely unused — as is the plain orphan.
        assert!(unused.contains(&graded_asset));
        assert!(unused.contains(&orphan));
        assert_eq!(unused.len(), 2);

        // Deterministic order, so the removal batch does not vary per run.
        let mut sorted = unused.clone();
        sorted.sort();
        assert_eq!(unused, sorted);
    }

    #[test]
    fn edit_clip_timing_shortens_without_ripple() {
        let (doc, seq, track, clip_id) = fixture();
        // Second clip after the first so we can prove no-ripple leaves it.
        let mut d = doc.clone();
        {
            let t = d
                .timeline
                .as_mut()
                .unwrap()
                .sequences
                .get_mut(&seq)
                .unwrap()
                .track_mut(track)
                .unwrap();
            t.clips
                .push(Clip::new(ClipSource::Adjustment, Tick(100), Tick(50)));
        }
        let p = d.timeline.as_ref().unwrap();
        let cmds = edit_clip_timing(
            p,
            seq,
            track,
            clip_id,
            ClipTiming {
                start: Tick(0),
                duration: Tick(60),
                source_in: Tick(0),
            },
            false,
        )
        .unwrap();
        assert_eq!(cmds.len(), 1);
        for c in &cmds {
            c.apply(&mut d);
        }
        let t = d
            .timeline
            .as_ref()
            .unwrap()
            .sequences
            .get(&seq)
            .unwrap()
            .track(track)
            .unwrap();
        assert_eq!(t.clips[0].duration, Tick(60));
        // Later clip stays put (gap opened).
        assert_eq!(t.clips[1].start, Tick(100));
    }

    #[test]
    fn edit_clip_timing_ripple_shortens_and_shifts_later() {
        let (doc, seq, track, clip_id) = fixture();
        let mut d = doc.clone();
        {
            let t = d
                .timeline
                .as_mut()
                .unwrap()
                .sequences
                .get_mut(&seq)
                .unwrap()
                .track_mut(track)
                .unwrap();
            t.clips
                .push(Clip::new(ClipSource::Adjustment, Tick(100), Tick(50)));
        }
        let p = d.timeline.as_ref().unwrap();
        let cmds = edit_clip_timing(
            p,
            seq,
            track,
            clip_id,
            ClipTiming {
                start: Tick(0),
                duration: Tick(60),
                source_in: Tick(0),
            },
            true,
        )
        .unwrap();
        assert!(!cmds.is_empty());
        for c in &cmds {
            c.apply(&mut d);
        }
        let t = d
            .timeline
            .as_ref()
            .unwrap()
            .sequences
            .get(&seq)
            .unwrap()
            .track(track)
            .unwrap();
        assert_eq!(t.clips[0].duration, Tick(60));
        // Later clip ripples left by 40.
        assert_eq!(t.clips[1].start, Tick(60));
    }

    #[test]
    fn edit_clip_timing_pure_move_and_noop() {
        let (doc, seq, track, clip_id) = fixture();
        let p = doc.timeline.as_ref().unwrap();
        let cmds = edit_clip_timing(
            p,
            seq,
            track,
            clip_id,
            ClipTiming {
                start: Tick(25),
                duration: Tick(100),
                source_in: Tick(0),
            },
            false,
        )
        .unwrap();
        assert_eq!(cmds.len(), 1);
        match &cmds[0] {
            TimelineCmd::MoveClip { new_start, .. } => assert_eq!(*new_start, Tick(25)),
            other => panic!("expected MoveClip, got {other:?}"),
        }
        let noop = edit_clip_timing(
            p,
            seq,
            track,
            clip_id,
            ClipTiming {
                start: Tick(0),
                duration: Tick(100),
                source_in: Tick(0),
            },
            false,
        )
        .unwrap();
        assert!(noop.is_empty());
    }

    #[test]
    fn edit_clip_timing_rejects_non_positive_duration() {
        let (doc, seq, track, clip_id) = fixture();
        let p = doc.timeline.as_ref().unwrap();
        let err = edit_clip_timing(
            p,
            seq,
            track,
            clip_id,
            ClipTiming {
                start: Tick(0),
                duration: Tick(0),
                source_in: Tick(0),
            },
            false,
        );
        assert_eq!(err, Err(EditError::NonPositiveDuration));
    }

    /// One sequence, one video track with a single clip. Returns the
    /// document plus the ids needed to address that clip.
    fn fixture() -> (Document, SequenceId, TrackId, ClipId) {
        let mut project = TimelineProject::new();
        let mut sequence = Sequence::new("Seq", FrameRate::FPS_30, 1920, 1080);
        let mut vtrack = Track::new(TrackKind::Video, "V1");
        let c = Clip::new(ClipSource::Adjustment, Tick(0), Tick(100));
        let clip_id = c.id;
        vtrack.clips.push(c);
        let track_id = vtrack.id;
        sequence.video_tracks.push(vtrack);
        let seq_id = sequence.id;
        project.insert_sequence(sequence);

        let mut doc = Document::new("t", 100.0, 100.0);
        doc.timeline = Some(project);
        (doc, seq_id, track_id, clip_id)
    }

    /// Adds a second (audio) clip to the fixture's sequence — for the
    /// linking tests, which need two distinct clips.
    fn add_audio_clip(doc: &mut Document, seq_id: SequenceId) -> (TrackId, ClipId) {
        let mut atrack = Track::new(TrackKind::Audio, "A1");
        let c = Clip::new(ClipSource::Adjustment, Tick(0), Tick(100));
        let clip_id = c.id;
        atrack.clips.push(c);
        let track_id = atrack.id;
        doc.timeline
            .as_mut()
            .unwrap()
            .sequences
            .get_mut(&seq_id)
            .unwrap()
            .audio_tracks
            .push(atrack);
        (track_id, clip_id)
    }

    /// Mirrors `tests/timeline.rs::assert_undo_roundtrip` (kept file-local —
    /// this story's territory is `ops.rs`, not the shared integration test
    /// file): `apply → inverse → apply` reproduces the post-apply state, and
    /// `inverse` alone restores the pre-apply state.
    fn assert_undo_roundtrip(doc: &Document, cmd: &TimelineCmd) {
        let before = doc.timeline.clone();

        let mut d1 = doc.clone();
        Command::Timeline(cmd.clone()).apply(&mut d1);
        let after_apply = d1.timeline.clone();

        let inv = cmd
            .inverse(&d1)
            .expect("SetClipProp-based ops always invert");
        let mut d2 = d1.clone();
        Command::Timeline(inv).apply(&mut d2);
        assert_eq!(
            d2.timeline, before,
            "inverse did not restore the original state"
        );

        let mut d3 = d2.clone();
        Command::Timeline(cmd.clone()).apply(&mut d3);
        assert_eq!(
            d3.timeline, after_apply,
            "apply -> inverse -> apply != apply"
        );
    }

    fn find_clip(doc: &Document, seq_id: SequenceId, track_id: TrackId, clip_id: ClipId) -> &Clip {
        doc.timeline
            .as_ref()
            .unwrap()
            .sequences
            .get(&seq_id)
            .unwrap()
            .track(track_id)
            .unwrap()
            .clips
            .iter()
            .find(|c| c.id == clip_id)
            .unwrap()
    }

    // ── Color label ──────────────────────────────────────────────────────

    #[test]
    fn set_clip_color_label_is_undo_idempotent() {
        let (doc, seq_id, track_id, clip_id) = fixture();
        let project = doc.timeline.as_ref().unwrap();
        let cmd = set_clip_color_label(project, seq_id, track_id, clip_id, Some(2)).unwrap();
        assert_undo_roundtrip(&doc, &cmd);

        let mut applied = doc.clone();
        Command::Timeline(cmd).apply(&mut applied);
        assert_eq!(
            find_clip(&applied, seq_id, track_id, clip_id).color_label,
            Some(2)
        );
    }

    #[test]
    fn set_clip_color_label_clear_is_undo_idempotent() {
        let (doc, seq_id, track_id, clip_id) = fixture();
        let project = doc.timeline.as_ref().unwrap();
        let set_cmd = set_clip_color_label(project, seq_id, track_id, clip_id, Some(5)).unwrap();
        let mut labeled = doc.clone();
        Command::Timeline(set_cmd).apply(&mut labeled);

        let project = labeled.timeline.as_ref().unwrap();
        let clear_cmd = set_clip_color_label(project, seq_id, track_id, clip_id, None).unwrap();
        assert_undo_roundtrip(&labeled, &clear_cmd);

        let mut cleared = labeled.clone();
        Command::Timeline(clear_cmd).apply(&mut cleared);
        assert_eq!(
            find_clip(&cleared, seq_id, track_id, clip_id).color_label,
            None
        );
    }

    // ── Linking ──────────────────────────────────────────────────────────

    #[test]
    fn link_clips_groups_both_and_is_undo_idempotent() {
        let (mut doc, seq_id, vtrack, vclip) = fixture();
        let (atrack, aclip) = add_audio_clip(&mut doc, seq_id);

        let project = doc.timeline.as_ref().unwrap();
        let cmds = link_clips(project, seq_id, vtrack, vclip, atrack, aclip).unwrap();
        assert_eq!(cmds.len(), 2);

        let before = doc.timeline.clone();
        let mut d1 = doc.clone();
        for c in &cmds {
            Command::Timeline(c.clone()).apply(&mut d1);
        }
        let group_v = find_clip(&d1, seq_id, vtrack, vclip).link_group;
        let group_a = find_clip(&d1, seq_id, atrack, aclip).link_group;
        assert!(group_v.is_some());
        assert_eq!(group_v, group_a, "both clips must share one link group");

        assert_eq!(
            clips_in_link_group(d1.timeline.as_ref().unwrap(), group_v.unwrap())
                .into_iter()
                .collect::<std::collections::HashSet<_>>(),
            [vclip, aclip].into_iter().collect()
        );

        // apply -> inverse (both, any order — each targets a distinct clip)
        // -> apply reproduces the linked state; inverse alone restores the
        // pre-link state.
        let invs: Vec<TimelineCmd> = cmds.iter().map(|c| c.inverse(&d1).unwrap()).collect();
        let mut d2 = d1.clone();
        for inv in &invs {
            Command::Timeline(inv.clone()).apply(&mut d2);
        }
        assert_eq!(d2.timeline, before);

        let mut d3 = d2.clone();
        for c in &cmds {
            Command::Timeline(c.clone()).apply(&mut d3);
        }
        assert_eq!(d3.timeline, d1.timeline);
    }

    #[test]
    fn link_clips_reuses_an_existing_group() {
        let (mut doc, seq_id, vtrack, vclip) = fixture();
        let (atrack, aclip) = add_audio_clip(&mut doc, seq_id);
        let (btrack, bclip) = add_audio_clip(&mut doc, seq_id);

        let project = doc.timeline.as_ref().unwrap();
        let cmds = link_clips(project, seq_id, vtrack, vclip, atrack, aclip).unwrap();
        for c in &cmds {
            Command::Timeline(c.clone()).apply(&mut doc);
        }
        let group = find_clip(&doc, seq_id, vtrack, vclip).link_group.unwrap();

        // Linking a third clip against the already-linked video clip must
        // join the *same* group rather than minting a new one.
        let project = doc.timeline.as_ref().unwrap();
        let more_cmds = link_clips(project, seq_id, vtrack, vclip, btrack, bclip).unwrap();
        for c in &more_cmds {
            Command::Timeline(c.clone()).apply(&mut doc);
        }
        assert_eq!(
            find_clip(&doc, seq_id, btrack, bclip).link_group,
            Some(group)
        );
    }

    #[test]
    fn unlink_clip_is_undo_idempotent() {
        let (doc, seq_id, track_id, clip_id) = fixture();
        let project = doc.timeline.as_ref().unwrap();
        let mut c = find_clip(&doc, seq_id, track_id, clip_id).clone();
        c.link_group = Some(LinkGroupId::new());
        let seed_cmd = set_clip_prop(project, seq_id, track_id, c).unwrap();
        let mut linked = doc.clone();
        Command::Timeline(seed_cmd).apply(&mut linked);
        assert!(find_clip(&linked, seq_id, track_id, clip_id)
            .link_group
            .is_some());

        let project = linked.timeline.as_ref().unwrap();
        let cmd = unlink_clip(project, seq_id, track_id, clip_id).unwrap();
        assert_undo_roundtrip(&linked, &cmd);

        let mut unlinked = linked.clone();
        Command::Timeline(cmd).apply(&mut unlinked);
        assert_eq!(
            find_clip(&unlinked, seq_id, track_id, clip_id).link_group,
            None
        );
    }

    #[test]
    fn unlink_clip_on_an_unlinked_clip_is_a_harmless_noop() {
        let (doc, seq_id, track_id, clip_id) = fixture();
        let project = doc.timeline.as_ref().unwrap();
        let cmd = unlink_clip(project, seq_id, track_id, clip_id).unwrap();
        assert_undo_roundtrip(&doc, &cmd);
    }

    #[test]
    fn clips_in_link_group_is_empty_for_an_unused_group() {
        let (doc, ..) = fixture();
        let project = doc.timeline.as_ref().unwrap();
        assert!(clips_in_link_group(project, LinkGroupId::new()).is_empty());
    }

    // ── Replace edit + adjustment/text create (G-5 / G-7 / G-12) ──────────

    #[test]
    fn replace_clip_source_keeps_timing_effects_grade_and_is_undo_idempotent() {
        use super::super::clip::{Ratio, SpeedKey, SpeedMap};
        use super::super::effect_kind::EffectKind;

        let (doc, seq_id, track_id, clip_id) = fixture();

        // Seed the slot with an effect, a grade, and a keyframed speed ramp —
        // exactly the "everything the editor built" that Replace must preserve.
        let mut seeded = find_clip(&doc, seq_id, track_id, clip_id).clone();
        seeded.effects.push(ClipEffect::new(EffectKind::Blur));
        seeded.grade = Some(Grade::default());
        seeded.speed = SpeedMap::Keyframed {
            keys: vec![SpeedKey::new(Tick(0), Ratio::new(1, 2))],
        };
        let install =
            set_clip_prop(doc.timeline.as_ref().unwrap(), seq_id, track_id, seeded).unwrap();
        let mut seeded_doc = doc.clone();
        Command::Timeline(install).apply(&mut seeded_doc);

        let new_source = ClipSource::SolidColor {
            color: crate::Color {
                r: 0.2,
                g: 0.4,
                b: 0.6,
                a: 1.0,
            },
        };
        let project = seeded_doc.timeline.as_ref().unwrap();
        let cmd = replace_clip_source(
            project,
            seq_id,
            track_id,
            clip_id,
            new_source.clone(),
            Some(Tick(500)),
        )
        .unwrap();
        assert_undo_roundtrip(&seeded_doc, &cmd);

        let mut replaced = seeded_doc.clone();
        Command::Timeline(cmd).apply(&mut replaced);
        let c = find_clip(&replaced, seq_id, track_id, clip_id);
        assert_eq!(c.source, new_source, "source swapped");
        assert_eq!(c.source_in, Tick(500), "source_in updated");
        assert_eq!(c.start, Tick(0), "start preserved");
        assert_eq!(c.duration, Tick(100), "duration preserved");
        assert_eq!(c.effects.len(), 1, "effect stack preserved");
        assert!(c.grade.is_some(), "grade preserved");
        assert!(
            matches!(c.speed, SpeedMap::Keyframed { .. }),
            "speed ramp preserved"
        );
    }

    #[test]
    fn replace_clip_source_none_keeps_source_in_and_is_undo_idempotent() {
        let (doc, seq_id, track_id, clip_id) = fixture();
        let new_source = ClipSource::SolidColor {
            color: crate::Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
        };
        let project = doc.timeline.as_ref().unwrap();
        let cmd =
            replace_clip_source(project, seq_id, track_id, clip_id, new_source, None).unwrap();
        assert_undo_roundtrip(&doc, &cmd);
    }

    #[test]
    fn add_adjustment_clip_inserts_and_is_undo_idempotent() {
        let (doc, seq_id, track_id) = track_fixture(&[]);
        let project = doc.timeline.as_ref().unwrap();
        let cmd = add_adjustment_clip(project, seq_id, track_id, Tick(200), Tick(150)).unwrap();
        assert_undo_roundtrip(&doc, &cmd);

        let mut applied = doc.clone();
        Command::Timeline(cmd).apply(&mut applied);
        let clips = &the_track(&applied, seq_id, track_id).clips;
        assert_eq!(clips.len(), 1);
        assert!(matches!(clips[0].source, ClipSource::Adjustment));
        assert_eq!((clips[0].start.0, clips[0].duration.0), (200, 150));
    }

    #[test]
    fn add_adjustment_clip_rejects_overlap() {
        let (doc, seq_id, track_id) = track_fixture(&[(0, 100)]);
        let project = doc.timeline.as_ref().unwrap();
        assert_eq!(
            add_adjustment_clip(project, seq_id, track_id, Tick(50), Tick(100)),
            Err(EditError::Overlap)
        );
    }

    #[test]
    fn add_text_clip_inserts_titled_and_is_undo_idempotent() {
        use super::super::clip::TextClipContent;
        let (doc, seq_id, track_id) = track_fixture(&[]);
        let project = doc.timeline.as_ref().unwrap();
        let cmd = add_text_clip(
            project,
            seq_id,
            track_id,
            Tick(0),
            Tick(90),
            TextClipContent::new("Hello"),
        )
        .unwrap();
        assert_undo_roundtrip(&doc, &cmd);

        let mut applied = doc.clone();
        Command::Timeline(cmd).apply(&mut applied);
        let clips = &the_track(&applied, seq_id, track_id).clips;
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0].name, "Hello", "name defaults to the text");
        assert!(
            matches!(&clips[0].source, ClipSource::Text { content } if content.text == "Hello")
        );
    }

    // ── Auto-reframe ─────────────────────────────────────────────────────

    #[test]
    fn fit_clip_to_format_16x9_to_9x16_covers_and_centers() {
        let content = SequenceFormat::new("16:9", 1920, 1080);
        let target = SequenceFormat::new("9:16", 1080, 1920);
        let xf = fit_clip_to_format(&content, &target);

        // Cover scale = max(1080/1920, 1920/1080) = 1920/1080 (the taller
        // axis needs more scale-up) — and it's > 1 (content must grow to
        // cover a narrower, taller frame).
        let expected = 1920.0 / 1080.0;
        assert!((xf.scale_x - expected).abs() < 1e-9);
        assert_eq!(xf.scale_x, xf.scale_y, "scale must be isotropic");
        assert!(xf.scale_x > 1.0);

        // Centered: position/anchor/rotation untouched from the default.
        assert_eq!(xf.x, 0.0);
        assert_eq!(xf.y, 0.0);
        assert_eq!(xf.anchor_x, 0.0);
        assert_eq!(xf.anchor_y, 0.0);
        assert_eq!(xf.rotation, 0.0);
        assert_eq!(xf.opacity, 1.0);
    }

    #[test]
    fn fit_clip_to_format_identity_for_matching_format() {
        let f = SequenceFormat::new("16:9", 1920, 1080);
        let xf = fit_clip_to_format(&f, &f);
        assert_eq!(xf.scale_x, 1.0);
        assert_eq!(xf.scale_y, 1.0);
    }

    #[test]
    fn fit_clip_to_format_9x16_to_16x9_also_covers() {
        // The reverse retarget: same numeric ratio picks the axis that now
        // needs the scale-up (still isotropic, still > 1).
        let content = SequenceFormat::new("9:16", 1080, 1920);
        let target = SequenceFormat::new("16:9", 1920, 1080);
        let xf = fit_clip_to_format(&content, &target);
        let expected = 1920.0 / 1080.0;
        assert!((xf.scale_x - expected).abs() < 1e-9);
        assert!(xf.scale_x > 1.0);
    }

    #[test]
    fn set_clip_reframe_set_and_clear_are_undo_idempotent() {
        let (doc, seq_id, track_id, clip_id) = fixture();
        let content = SequenceFormat::new("16:9", 1920, 1080);
        let target = SequenceFormat::new("9:16", 1080, 1920);
        let xf = fit_clip_to_format(&content, &target);

        let project = doc.timeline.as_ref().unwrap();
        let set_cmd = set_clip_reframe(project, seq_id, track_id, clip_id, 1, Some(xf)).unwrap();
        assert_undo_roundtrip(&doc, &set_cmd);

        let mut reframed = doc.clone();
        Command::Timeline(set_cmd).apply(&mut reframed);
        assert_eq!(
            find_clip(&reframed, seq_id, track_id, clip_id)
                .reframe
                .get(&1),
            Some(&xf)
        );

        let project = reframed.timeline.as_ref().unwrap();
        let clear_cmd = set_clip_reframe(project, seq_id, track_id, clip_id, 1, None).unwrap();
        assert_undo_roundtrip(&reframed, &clear_cmd);

        let mut cleared = reframed.clone();
        Command::Timeline(clear_cmd).apply(&mut cleared);
        assert!(!find_clip(&cleared, seq_id, track_id, clip_id)
            .reframe
            .contains_key(&1));
    }

    // ── Sync lock (M-9) ──────────────────────────────────────────────────

    #[test]
    fn toggle_sync_lock_flips_and_is_undo_idempotent() {
        let (doc, seq_id, track_id, _) = fixture();
        let project = doc.timeline.as_ref().unwrap();
        assert!(
            !project.sequences[&seq_id]
                .track(track_id)
                .unwrap()
                .sync_lock
        );

        let cmd = toggle_sync_lock(project, seq_id, track_id).unwrap();
        assert_undo_roundtrip(&doc, &cmd);

        let mut on = doc.clone();
        Command::Timeline(cmd).apply(&mut on);
        assert!(
            on.timeline.as_ref().unwrap().sequences[&seq_id]
                .track(track_id)
                .unwrap()
                .sync_lock,
            "toggle must set sync_lock"
        );

        // A second toggle flips it back off.
        let project = on.timeline.as_ref().unwrap();
        let cmd2 = toggle_sync_lock(project, seq_id, track_id).unwrap();
        let mut off = on.clone();
        Command::Timeline(cmd2).apply(&mut off);
        assert!(
            !off.timeline.as_ref().unwrap().sequences[&seq_id]
                .track(track_id)
                .unwrap()
                .sync_lock
        );
    }

    // ── 3/4-point editing (insert / overwrite / lift / extract) ──────────

    /// A document with one video track carrying `spans` (`(start, dur)` pairs,
    /// assumed sorted + non-overlapping). Returns the ids to address the track.
    fn track_fixture(spans: &[(i64, i64)]) -> (Document, SequenceId, TrackId) {
        let mut project = TimelineProject::new();
        let mut sequence = Sequence::new("Seq", FrameRate::FPS_30, 1920, 1080);
        let mut vtrack = Track::new(TrackKind::Video, "V1");
        for (start, dur) in spans {
            vtrack
                .clips
                .push(Clip::new(ClipSource::Adjustment, Tick(*start), Tick(*dur)));
        }
        let track_id = vtrack.id;
        sequence.video_tracks.push(vtrack);
        let seq_id = sequence.id;
        project.insert_sequence(sequence);
        let mut doc = Document::new("t", 100.0, 100.0);
        doc.timeline = Some(project);
        (doc, seq_id, track_id)
    }

    fn the_track(doc: &Document, seq_id: SequenceId, track_id: TrackId) -> &Track {
        doc.timeline.as_ref().unwrap().sequences[&seq_id]
            .track(track_id)
            .unwrap()
    }

    /// `(start, duration)` of every clip on the track, in stored order.
    fn spans_of(doc: &Document, seq_id: SequenceId, track_id: TrackId) -> Vec<(i64, i64)> {
        the_track(doc, seq_id, track_id)
            .clips
            .iter()
            .map(|c| (c.start.0, c.duration.0))
            .collect()
    }

    /// Max clip end on the track (its content length); 0 when empty.
    fn track_end(doc: &Document, seq_id: SequenceId, track_id: TrackId) -> i64 {
        the_track(doc, seq_id, track_id)
            .clips
            .iter()
            .map(|c| c.end().0)
            .max()
            .unwrap_or(0)
    }

    fn as_batch(cmds: &[TimelineCmd]) -> Command {
        Command::Batch(cmds.iter().cloned().map(Command::Timeline).collect())
    }

    fn apply_batch(doc: &Document, cmds: &[TimelineCmd]) -> Document {
        let mut d = doc.clone();
        as_batch(cmds).apply(&mut d);
        d
    }

    /// A batch (the shape the four edit ops return) round-trips: `apply →
    /// inverse` restores the pre-state, and `apply → inverse → apply`
    /// reproduces the post-apply state (undo idempotency).
    fn assert_batch_undo_roundtrip(doc: &Document, cmds: &[TimelineCmd]) {
        let before = doc.timeline.clone();
        let batch = as_batch(cmds);

        let mut d1 = doc.clone();
        batch.apply(&mut d1);
        let after_apply = d1.timeline.clone();

        let inv = batch.inverse(&d1).expect("edit batches always invert");
        let mut d2 = d1.clone();
        inv.apply(&mut d2);
        assert_eq!(d2.timeline, before, "inverse did not restore the pre-state");

        let mut d3 = d2.clone();
        batch.apply(&mut d3);
        assert_eq!(
            d3.timeline, after_apply,
            "apply -> inverse -> apply != apply"
        );
    }

    fn validate_ok(doc: &Document, seq_id: SequenceId) {
        let s = &doc.timeline.as_ref().unwrap().sequences[&seq_id];
        assert!(s.validate().is_ok(), "invariant broken: {:?}", s.validate());
    }

    fn adj_clip(dur: i64) -> Clip {
        Clip::new(ClipSource::Adjustment, Tick::ZERO, Tick(dur))
    }

    // ── Insert ───────────────────────────────────────────────────────────

    #[test]
    fn insert_edit_grows_track_by_source_duration() {
        // `at` straddles the middle clip; the whole track grows by the source.
        let (doc, seq_id, track_id) = track_fixture(&[(0, 100), (100, 100), (200, 100)]);
        let before_end = track_end(&doc, seq_id, track_id);
        let p = doc.timeline.as_ref().unwrap();
        let cmds = insert_edit(p, seq_id, track_id, Tick(150), adj_clip(50)).unwrap();
        let out = apply_batch(&doc, &cmds);

        validate_ok(&out, seq_id);
        assert_eq!(track_end(&out, seq_id, track_id), before_end + 50);
        // The split boundary produced a source clip in the opened gap [150,200).
        assert_eq!(
            spans_of(&out, seq_id, track_id),
            vec![(0, 100), (100, 50), (150, 50), (200, 50), (250, 100)]
        );
        assert_batch_undo_roundtrip(&doc, &cmds);
    }

    #[test]
    fn insert_edit_at_clip_boundary_ripples_without_splitting() {
        let (doc, seq_id, track_id) = track_fixture(&[(0, 100), (100, 100)]);
        let p = doc.timeline.as_ref().unwrap();
        let cmds = insert_edit(p, seq_id, track_id, Tick(100), adj_clip(40)).unwrap();
        // No SplitClip — `at` is a cut point, not inside a clip.
        assert!(!cmds
            .iter()
            .any(|c| matches!(c, TimelineCmd::SplitClip { .. })));
        let out = apply_batch(&doc, &cmds);
        validate_ok(&out, seq_id);
        assert_eq!(
            spans_of(&out, seq_id, track_id),
            vec![(0, 100), (100, 40), (140, 100)]
        );
        assert_batch_undo_roundtrip(&doc, &cmds);
    }

    #[test]
    fn insert_edit_beyond_content_leaves_a_lead_gap() {
        let (doc, seq_id, track_id) = track_fixture(&[(0, 100)]);
        let p = doc.timeline.as_ref().unwrap();
        let cmds = insert_edit(p, seq_id, track_id, Tick(300), adj_clip(50)).unwrap();
        let out = apply_batch(&doc, &cmds);
        validate_ok(&out, seq_id);
        assert_eq!(track_end(&out, seq_id, track_id), 350);
        assert_batch_undo_roundtrip(&doc, &cmds);
    }

    // ── Sync lock (14 §M-9) ──────────────────────────────────────────────────

    /// A three-track sequence, one clip at `[100,100)` on each track: the edited
    /// track, a `sync_lock`ed sibling, and an unlocked sibling. Returns the doc
    /// plus `(seq, edited_track, sync_track, free_track)`.
    fn sync_lock_fixture() -> (Document, SequenceId, TrackId, TrackId, TrackId) {
        let mut project = TimelineProject::new();
        let mut sequence = Sequence::new("Seq", FrameRate::FPS_30, 1920, 1080);

        let mut mk = |name: &str, sync: bool| {
            let mut tr = Track::new(TrackKind::Video, name);
            tr.sync_lock = sync;
            tr.clips
                .push(Clip::new(ClipSource::Adjustment, Tick(100), Tick(100)));
            let id = tr.id;
            sequence.video_tracks.push(tr);
            id
        };
        let edit_id = mk("V_edit", false);
        let sync_id = mk("V_sync", true);
        let free_id = mk("V_free", false);

        let seq_id = sequence.id;
        project.insert_sequence(sequence);
        let mut doc = Document::new("t", 100.0, 100.0);
        doc.timeline = Some(project);
        (doc, seq_id, edit_id, sync_id, free_id)
    }

    #[test]
    fn insert_edit_ripples_sync_locked_sibling_but_not_an_unlocked_one() {
        let (doc, seq_id, edit_id, sync_id, free_id) = sync_lock_fixture();
        let p = doc.timeline.as_ref().unwrap();
        // Insert a 50-tick source at the head of the edited clip (a cut point).
        let cmds = insert_edit(p, seq_id, edit_id, Tick(100), adj_clip(50)).unwrap();
        let out = apply_batch(&doc, &cmds);

        validate_ok(&out, seq_id);
        // Edited track: source dropped into the opened gap, its clip shifted right.
        assert_eq!(spans_of(&out, seq_id, edit_id), vec![(100, 50), (150, 100)]);
        // The sync-locked sibling rode along: 100 → 150.
        assert_eq!(spans_of(&out, seq_id, sync_id), vec![(150, 100)]);
        // The unlocked sibling did NOT move.
        assert_eq!(spans_of(&out, seq_id, free_id), vec![(100, 100)]);
        assert_batch_undo_roundtrip(&doc, &cmds);
    }

    #[test]
    fn extract_edit_ripples_sync_locked_sibling_but_not_an_unlocked_one() {
        let (doc, seq_id, edit_id, sync_id, free_id) = sync_lock_fixture();
        let p = doc.timeline.as_ref().unwrap();
        // Extract [100,150): removes the edited clip's head and closes the gap by
        // shifting later content left by 50 — a ripple the sync sibling shares.
        let cmds = extract_edit(p, seq_id, edit_id, (Tick(100), Tick(150))).unwrap();
        let out = apply_batch(&doc, &cmds);

        validate_ok(&out, seq_id);
        // The sync-locked sibling's clip shifted left: 100 → 50.
        assert_eq!(spans_of(&out, seq_id, sync_id), vec![(50, 100)]);
        // The unlocked sibling stayed put.
        assert_eq!(spans_of(&out, seq_id, free_id), vec![(100, 100)]);
        assert_batch_undo_roundtrip(&doc, &cmds);
    }

    #[test]
    fn ripple_delete_ripples_sync_locked_sibling_but_not_an_unlocked_one() {
        let (doc, seq_id, edit_id, sync_id, free_id) = sync_lock_fixture();
        // Two clips on the edited track so a delete of the first has something
        // to ripple on its own track; the sync sibling rides the same left-shift.
        let mut doc = doc;
        {
            let p = doc.timeline.as_mut().unwrap();
            let t = p
                .sequences
                .get_mut(&seq_id)
                .unwrap()
                .track_mut(edit_id)
                .unwrap();
            // Existing clip sits at [100,100); prepend [0,100) to delete.
            t.clips
                .insert(0, Clip::new(ClipSource::Adjustment, Tick(0), Tick(100)));
        }
        let p = doc.timeline.as_ref().unwrap();
        let head = p.sequences[&seq_id].track(edit_id).unwrap().clips[0].id;
        let cmds = ripple_delete(p, seq_id, edit_id, head).unwrap();
        let out = apply_batch(&doc, &cmds);

        validate_ok(&out, seq_id);
        // Sync sibling: 100 → 0 (left by the deleted clip's duration).
        assert_eq!(spans_of(&out, seq_id, sync_id), vec![(0, 100)]);
        // Free sibling stayed put.
        assert_eq!(spans_of(&out, seq_id, free_id), vec![(100, 100)]);
        assert_batch_undo_roundtrip(&doc, &cmds);
    }

    #[test]
    fn ripple_trim_end_ripples_sync_locked_sibling_but_not_an_unlocked_one() {
        // End-trim expands from `old_end`: only clips with start >= old_end on
        // other tracks ride along (mirrors the edited track's own downstream
        // rule). Seed every track with a clip that starts at the head's out
        // point so the expand has something to shift.
        let mut project = TimelineProject::new();
        let mut sequence = Sequence::new("Seq", FrameRate::FPS_30, 1920, 1080);

        let mut edit = Track::new(TrackKind::Video, "V_edit");
        edit.clips
            .push(Clip::new(ClipSource::Adjustment, Tick(100), Tick(100))); // [100,200)
        edit.clips
            .push(Clip::new(ClipSource::Adjustment, Tick(200), Tick(100))); // [200,300)
        let edit_id = edit.id;
        sequence.video_tracks.push(edit);

        let mut sync = Track::new(TrackKind::Video, "V_sync");
        sync.sync_lock = true;
        sync.clips
            .push(Clip::new(ClipSource::Adjustment, Tick(200), Tick(100)));
        let sync_id = sync.id;
        sequence.video_tracks.push(sync);

        let mut free = Track::new(TrackKind::Video, "V_free");
        free.clips
            .push(Clip::new(ClipSource::Adjustment, Tick(200), Tick(100)));
        let free_id = free.id;
        sequence.video_tracks.push(free);

        let seq_id = sequence.id;
        project.insert_sequence(sequence);
        let mut doc = Document::new("t", 100.0, 100.0);
        doc.timeline = Some(project);

        let p = doc.timeline.as_ref().unwrap();
        let head = p.sequences[&seq_id].track(edit_id).unwrap().clips[0].id;
        // End-trim head 200 → 150 (delta -50): edited downstream + sync at 200
        // both shift left; free track does not.
        let cmds = ripple_trim(p, seq_id, edit_id, head, ClipEdge::End, Tick(150)).unwrap();
        let out = apply_batch(&doc, &cmds);

        validate_ok(&out, seq_id);
        assert_eq!(spans_of(&out, seq_id, edit_id), vec![(100, 50), (150, 100)]);
        assert_eq!(spans_of(&out, seq_id, sync_id), vec![(150, 100)]);
        assert_eq!(spans_of(&out, seq_id, free_id), vec![(200, 100)]);
        assert_batch_undo_roundtrip(&doc, &cmds);
    }

    // ── Overwrite ────────────────────────────────────────────────────────

    #[test]
    fn overwrite_edit_keeps_duration_and_punches_a_hole() {
        // Source lands inside one long clip → head + source + tail, same end.
        let (doc, seq_id, track_id) = track_fixture(&[(0, 300)]);
        let before_end = track_end(&doc, seq_id, track_id);
        let p = doc.timeline.as_ref().unwrap();
        let cmds = overwrite_edit(p, seq_id, track_id, Tick(100), adj_clip(50)).unwrap();
        let out = apply_batch(&doc, &cmds);

        validate_ok(&out, seq_id);
        assert_eq!(track_end(&out, seq_id, track_id), before_end);
        assert_eq!(
            spans_of(&out, seq_id, track_id),
            vec![(0, 100), (100, 50), (150, 150)]
        );
        assert_batch_undo_roundtrip(&doc, &cmds);
    }

    #[test]
    fn overwrite_edit_removes_fully_covered_and_trims_neighbours() {
        let (doc, seq_id, track_id) =
            track_fixture(&[(0, 100), (100, 100), (200, 100), (300, 100)]);
        let p = doc.timeline.as_ref().unwrap();
        // [80, 320) covers clip #2 fully, trims the tail of #1 and head of #4.
        let cmds = overwrite_edit(p, seq_id, track_id, Tick(80), adj_clip(240)).unwrap();
        let out = apply_batch(&doc, &cmds);
        validate_ok(&out, seq_id);
        assert_eq!(
            spans_of(&out, seq_id, track_id),
            vec![(0, 80), (80, 240), (320, 80)]
        );
        assert_batch_undo_roundtrip(&doc, &cmds);
    }

    #[test]
    fn overwrite_edit_past_end_extends() {
        let (doc, seq_id, track_id) = track_fixture(&[(0, 100)]);
        let p = doc.timeline.as_ref().unwrap();
        let cmds = overwrite_edit(p, seq_id, track_id, Tick(50), adj_clip(200)).unwrap();
        let out = apply_batch(&doc, &cmds);
        validate_ok(&out, seq_id);
        assert_eq!(spans_of(&out, seq_id, track_id), vec![(0, 50), (50, 200)]);
        assert_batch_undo_roundtrip(&doc, &cmds);
    }

    // ── Lift ─────────────────────────────────────────────────────────────

    #[test]
    fn lift_edit_keeps_duration_and_leaves_a_gap() {
        let (doc, seq_id, track_id) = track_fixture(&[(0, 100), (100, 100), (200, 100)]);
        let before_end = track_end(&doc, seq_id, track_id);
        let p = doc.timeline.as_ref().unwrap();
        let cmds = lift_edit(p, seq_id, track_id, (Tick(100), Tick(200))).unwrap();
        let out = apply_batch(&doc, &cmds);

        validate_ok(&out, seq_id);
        assert_eq!(track_end(&out, seq_id, track_id), before_end);
        // The middle clip is gone; the [100,200) gap remains open.
        assert_eq!(spans_of(&out, seq_id, track_id), vec![(0, 100), (200, 100)]);
        assert_batch_undo_roundtrip(&doc, &cmds);
    }

    #[test]
    fn lift_edit_splits_a_spanning_clip_into_head_and_tail() {
        let (doc, seq_id, track_id) = track_fixture(&[(0, 300)]);
        let p = doc.timeline.as_ref().unwrap();
        let cmds = lift_edit(p, seq_id, track_id, (Tick(100), Tick(200))).unwrap();
        let out = apply_batch(&doc, &cmds);
        validate_ok(&out, seq_id);
        assert_eq!(spans_of(&out, seq_id, track_id), vec![(0, 100), (200, 100)]);
        // Nothing intersects the lifted range.
        for c in &the_track(&out, seq_id, track_id).clips {
            assert!(c.end().0 <= 100 || c.start.0 >= 200);
        }
        assert_batch_undo_roundtrip(&doc, &cmds);
    }

    // ── Extract ──────────────────────────────────────────────────────────

    #[test]
    fn extract_edit_shrinks_track_by_range_width() {
        let (doc, seq_id, track_id) = track_fixture(&[(0, 100), (100, 100), (200, 100)]);
        let before_end = track_end(&doc, seq_id, track_id);
        let p = doc.timeline.as_ref().unwrap();
        let cmds = extract_edit(p, seq_id, track_id, (Tick(100), Tick(200))).unwrap();
        let out = apply_batch(&doc, &cmds);

        validate_ok(&out, seq_id);
        assert_eq!(track_end(&out, seq_id, track_id), before_end - 100);
        // The gap closed: the third clip slid left into the second's slot.
        assert_eq!(spans_of(&out, seq_id, track_id), vec![(0, 100), (100, 100)]);
        assert_batch_undo_roundtrip(&doc, &cmds);
    }

    #[test]
    fn extract_edit_spanning_clip_closes_the_gap() {
        // One clip spans the whole range and there is trailing content to slide.
        let (doc, seq_id, track_id) = track_fixture(&[(0, 300), (300, 100)]);
        let before_end = track_end(&doc, seq_id, track_id);
        let p = doc.timeline.as_ref().unwrap();
        let cmds = extract_edit(p, seq_id, track_id, (Tick(100), Tick(200))).unwrap();
        let out = apply_batch(&doc, &cmds);
        validate_ok(&out, seq_id);
        assert_eq!(track_end(&out, seq_id, track_id), before_end - 100);
        assert_eq!(
            spans_of(&out, seq_id, track_id),
            vec![(0, 100), (100, 100), (200, 100)]
        );
        assert_batch_undo_roundtrip(&doc, &cmds);
    }

    #[test]
    fn extract_edit_matches_ripple_delete_for_a_whole_clip_range() {
        // Extract over exactly one clip's span equals ripple_delete of it.
        let (doc, seq_id, track_id) = track_fixture(&[(0, 100), (100, 100), (200, 100)]);
        let p = doc.timeline.as_ref().unwrap();
        let extract = extract_edit(p, seq_id, track_id, (Tick(100), Tick(200))).unwrap();
        let mid = the_track(&doc, seq_id, track_id).clips[1].id;
        let ripple = ripple_delete(p, seq_id, track_id, mid).unwrap();

        let a = apply_batch(&doc, &extract);
        let b = apply_batch(&doc, &ripple);
        assert_eq!(
            spans_of(&a, seq_id, track_id),
            spans_of(&b, seq_id, track_id)
        );
    }

    // ── Invariant proptests (sorted / non-overlap / undo) ────────────────

    use proptest::prelude::*;

    /// A random sorted, non-overlapping span set, built by walking a cursor
    /// forward through random (gap, duration) pairs.
    fn arb_spans() -> impl Strategy<Value = Vec<(i64, i64)>> {
        prop::collection::vec((0i64..200, 1i64..200), 0..8).prop_map(|pairs| {
            let mut cursor = 0i64;
            let mut spans = Vec::new();
            for (gap, dur) in pairs {
                cursor += gap;
                spans.push((cursor, dur));
                cursor += dur;
            }
            spans
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn insert_edit_preserves_invariants_and_end(
            spans in arb_spans(),
            at in 0i64..2500,
            dur in 1i64..300,
        ) {
            let (doc, seq_id, track_id) = track_fixture(&spans);
            let before_end = track_end(&doc, seq_id, track_id);
            let p = doc.timeline.as_ref().unwrap();
            let cmds = insert_edit(p, seq_id, track_id, Tick(at), adj_clip(dur)).unwrap();
            let out = apply_batch(&doc, &cmds);
            let s = &out.timeline.as_ref().unwrap().sequences[&seq_id];
            prop_assert!(s.validate().is_ok(), "invariant broken: {:?}", s.validate());
            prop_assert_eq!(track_end(&out, seq_id, track_id), before_end.max(at) + dur);
            assert_batch_undo_roundtrip(&doc, &cmds);
        }

        #[test]
        fn overwrite_edit_preserves_invariants_and_end(
            spans in arb_spans(),
            at in 0i64..2500,
            dur in 1i64..300,
        ) {
            let (doc, seq_id, track_id) = track_fixture(&spans);
            let before_end = track_end(&doc, seq_id, track_id);
            let p = doc.timeline.as_ref().unwrap();
            let cmds = overwrite_edit(p, seq_id, track_id, Tick(at), adj_clip(dur)).unwrap();
            let out = apply_batch(&doc, &cmds);
            let s = &out.timeline.as_ref().unwrap().sequences[&seq_id];
            prop_assert!(s.validate().is_ok(), "invariant broken: {:?}", s.validate());
            prop_assert_eq!(track_end(&out, seq_id, track_id), before_end.max(at + dur));
            assert_batch_undo_roundtrip(&doc, &cmds);
        }

        #[test]
        fn lift_edit_preserves_invariants_and_clears_range(
            spans in arb_spans(),
            rs in 0i64..2500,
            width in 1i64..400,
        ) {
            let re = rs + width;
            let (doc, seq_id, track_id) = track_fixture(&spans);
            let before_end = track_end(&doc, seq_id, track_id);
            let p = doc.timeline.as_ref().unwrap();
            let cmds = lift_edit(p, seq_id, track_id, (Tick(rs), Tick(re))).unwrap();
            let out = apply_batch(&doc, &cmds);
            let s = &out.timeline.as_ref().unwrap().sequences[&seq_id];
            prop_assert!(s.validate().is_ok(), "invariant broken: {:?}", s.validate());
            // Lift never shifts content, so the track can only shorten, never grow.
            prop_assert!(track_end(&out, seq_id, track_id) <= before_end);
            // Nothing intersects the lifted (still-open) range.
            for c in &the_track(&out, seq_id, track_id).clips {
                prop_assert!(c.end().0 <= rs || c.start.0 >= re);
            }
            assert_batch_undo_roundtrip(&doc, &cmds);
        }

        #[test]
        fn extract_edit_preserves_invariants_and_shrinks(
            spans in arb_spans(),
            rs in 0i64..2500,
            width in 1i64..400,
        ) {
            let re = rs + width;
            let (doc, seq_id, track_id) = track_fixture(&spans);
            let before_end = track_end(&doc, seq_id, track_id);
            let p = doc.timeline.as_ref().unwrap();
            let cmds = extract_edit(p, seq_id, track_id, (Tick(rs), Tick(re))).unwrap();
            let out = apply_batch(&doc, &cmds);
            let s = &out.timeline.as_ref().unwrap().sequences[&seq_id];
            prop_assert!(s.validate().is_ok(), "invariant broken: {:?}", s.validate());
            // With content strictly past the range, the gap closes by its width.
            if re < before_end {
                prop_assert_eq!(track_end(&out, seq_id, track_id), before_end - width);
            }
            assert_batch_undo_roundtrip(&doc, &cmds);
        }
    }

    // ── Sequence management (17 §G-17) ────────────────────────────────────

    #[test]
    fn rename_sequence_is_undo_idempotent() {
        let (doc, seq_id, _t, _c) = fixture();
        let p = doc.timeline.as_ref().unwrap();
        let cmd = rename_sequence(p, seq_id, "Act 1").unwrap();
        assert_undo_roundtrip(&doc, &cmd);

        let mut applied = doc.clone();
        Command::Timeline(cmd).apply(&mut applied);
        assert_eq!(
            applied.timeline.as_ref().unwrap().sequences[&seq_id].name,
            "Act 1"
        );
    }

    #[test]
    fn duplicate_sequence_copies_content_with_fresh_ids_and_is_undo_idempotent() {
        let (doc, seq_id, track_id, _c) = fixture();
        let orig_clip = the_track(&doc, seq_id, track_id).clips[0].id;
        let p = doc.timeline.as_ref().unwrap();
        let cmd = duplicate_sequence(p, seq_id).unwrap();
        assert_undo_roundtrip(&doc, &cmd);

        let mut applied = doc.clone();
        Command::Timeline(cmd).apply(&mut applied);
        let proj = applied.timeline.as_ref().unwrap();
        assert_eq!(proj.sequences.len(), 2);
        let dup = proj.sequences.values().find(|s| s.id != seq_id).unwrap();
        assert_eq!(dup.name, "Seq copy");
        // Same clip timing, fresh clip id.
        assert_eq!(
            dup.video_tracks[0]
                .clips
                .iter()
                .map(|c| (c.start.0, c.duration.0))
                .collect::<Vec<_>>(),
            spans_of(&applied, seq_id, track_id)
        );
        assert_ne!(dup.video_tracks[0].clips[0].id, orig_clip);
    }

    // ── Nested sequences (17 §G-16) ───────────────────────────────────────

    #[test]
    fn create_nested_sequence_wraps_selection_and_is_undo_idempotent() {
        let (doc, seq_id, track_id) = track_fixture(&[(0, 100), (100, 100), (200, 100)]);
        // Nest the first two clips → span [0,200).
        let ids: Vec<ClipId> = the_track(&doc, seq_id, track_id).clips[..2]
            .iter()
            .map(|c| c.id)
            .collect();
        let p = doc.timeline.as_ref().unwrap();
        let (inner_id, cmds) = create_nested_sequence(p, seq_id, track_id, &ids, "Nested").unwrap();
        let out = apply_batch(&doc, &cmds);
        validate_ok(&out, seq_id);

        // Outer: one NestedSequence clip [0,200) + the untouched [200,300).
        assert_eq!(spans_of(&out, seq_id, track_id), vec![(0, 200), (200, 100)]);
        let outer = the_track(&out, seq_id, track_id);
        assert!(matches!(
            outer.clips[0].source,
            ClipSource::NestedSequence { sequence } if sequence == inner_id
        ));
        // Inner: the two clips rebased to start at 0.
        let inner = &out.timeline.as_ref().unwrap().sequences[&inner_id];
        assert_eq!(
            inner.video_tracks[0]
                .clips
                .iter()
                .map(|c| (c.start.0, c.duration.0))
                .collect::<Vec<_>>(),
            vec![(0, 100), (100, 100)]
        );
        assert_batch_undo_roundtrip(&doc, &cmds);
    }

    #[test]
    fn create_nested_sequence_rejects_interior_nonselected_clip() {
        let (doc, seq_id, track_id) = track_fixture(&[(0, 100), (100, 100), (200, 100)]);
        // Select the outer two, leaving [100,200) inside the span.
        let clips = &the_track(&doc, seq_id, track_id).clips;
        let ids = vec![clips[0].id, clips[2].id];
        let p = doc.timeline.as_ref().unwrap();
        assert_eq!(
            create_nested_sequence(p, seq_id, track_id, &ids, "N").unwrap_err(),
            EditError::Overlap
        );
    }

    #[test]
    fn create_nested_sequence_rejects_empty_selection() {
        let (doc, seq_id, track_id) = track_fixture(&[(0, 100)]);
        let p = doc.timeline.as_ref().unwrap();
        assert_eq!(
            create_nested_sequence(p, seq_id, track_id, &[], "N").unwrap_err(),
            EditError::IndexOutOfRange
        );
    }

    #[test]
    fn nested_target_and_ancestry() {
        let (doc, seq_id, track_id) = track_fixture(&[(0, 100), (100, 100)]);
        let ids: Vec<ClipId> = the_track(&doc, seq_id, track_id)
            .clips
            .iter()
            .map(|c| c.id)
            .collect();
        let p = doc.timeline.as_ref().unwrap();
        let (inner_id, cmds) = create_nested_sequence(p, seq_id, track_id, &ids, "Nested").unwrap();
        let out = apply_batch(&doc, &cmds);

        let nested_clip = &the_track(&out, seq_id, track_id).clips[0];
        assert_eq!(nested_target(nested_clip), Some(inner_id));
        let p2 = out.timeline.as_ref().unwrap();
        assert_eq!(sequence_ancestry(p2, inner_id), vec![seq_id, inner_id]);
        assert_eq!(sequence_ancestry(p2, seq_id), vec![seq_id]);
    }

    // ── Multicam (17 §G-20) ───────────────────────────────────────────────

    #[test]
    fn create_multicam_group_folds_angles_and_is_undo_idempotent() {
        let (mut doc, seq_id, v_track, primary) = fixture();
        let (a_track, angle_clip) = add_audio_clip(&mut doc, seq_id);
        let p = doc.timeline.as_ref().unwrap();
        let cmds =
            create_multicam_group(p, seq_id, v_track, primary, &[(a_track, angle_clip)]).unwrap();
        let out = apply_batch(&doc, &cmds);
        validate_ok(&out, seq_id);

        // Primary carries a two-angle group; the folded clip is gone.
        let pc = find_clip(&out, seq_id, v_track, primary);
        let group = pc.multicam.as_ref().unwrap();
        assert_eq!(group.angles.len(), 2);
        assert_eq!(group.active, 0);
        assert!(the_track(&out, seq_id, a_track).clips.is_empty());

        assert_batch_undo_roundtrip(&doc, &cmds);
    }

    #[test]
    fn set_multicam_active_angle_mirrors_source_and_is_undo_idempotent() {
        let (mut doc, seq_id, v_track, primary) = fixture();
        let (a_track, angle_clip) = add_audio_clip(&mut doc, seq_id);
        // Give the angle clip a distinct source so the mirror is observable.
        let asset = AssetId::new();
        {
            let proj = doc.timeline.as_mut().unwrap();
            let c = proj.sequences.get_mut(&seq_id).unwrap().audio_tracks[0]
                .clips
                .iter_mut()
                .find(|c| c.id == angle_clip)
                .unwrap();
            c.source = ClipSource::Asset { asset };
        }
        let p = doc.timeline.as_ref().unwrap();
        let cmds =
            create_multicam_group(p, seq_id, v_track, primary, &[(a_track, angle_clip)]).unwrap();
        let grouped = apply_batch(&doc, &cmds);

        // Cut to angle 1: the clip's source mirrors that angle.
        let p2 = grouped.timeline.as_ref().unwrap();
        let cut = set_multicam_active_angle(p2, seq_id, v_track, primary, 1).unwrap();
        assert_undo_roundtrip(&grouped, &cut);

        let mut applied = grouped.clone();
        Command::Timeline(cut).apply(&mut applied);
        let pc = find_clip(&applied, seq_id, v_track, primary);
        assert_eq!(pc.multicam.as_ref().unwrap().active, 1);
        assert!(matches!(pc.source, ClipSource::Asset { asset: a } if a == asset));
    }

    #[test]
    fn set_multicam_active_angle_errors_without_group() {
        let (doc, seq_id, v_track, primary) = fixture();
        let p = doc.timeline.as_ref().unwrap();
        assert_eq!(
            set_multicam_active_angle(p, seq_id, v_track, primary, 0).unwrap_err(),
            EditError::IndexOutOfRange
        );
    }

    #[test]
    fn set_asset_meta_fills_probe_and_hash_undoably() {
        use super::super::media::{AssetKind, MediaAsset, MediaProbe};
        use super::super::time::Tick;
        let mut doc = Document::new("t", 100.0, 100.0);
        let mut project = TimelineProject::new();
        let asset = MediaAsset::from_file(AssetKind::Video, "/tmp/clip.mp4");
        let id = asset.id;
        project.media.insert(asset);
        doc.timeline = Some(project);

        let probe = MediaProbe {
            duration: Tick(1_000_000),
            video: None,
            audio: None,
            container: "mp4".into(),
            codec: "h264".into(),
            is_vfr: false,
            pixel_format: None,
            has_alpha: false,
        };
        let p = doc.timeline.as_ref().unwrap();
        let cmd = set_asset_meta(p, id, Some(probe.clone()), Some("hash-abc".into())).unwrap();
        assert_undo_roundtrip(&doc, &cmd);
        Command::Timeline(cmd).apply(&mut doc);
        let a = doc
            .timeline
            .as_ref()
            .unwrap()
            .media
            .assets
            .get(&id)
            .unwrap();
        assert_eq!(a.probe.as_ref().map(|p| p.codec.as_str()), Some("h264"));
        assert_eq!(a.content_hash.as_deref(), Some("hash-abc"));
    }

    #[test]
    fn set_generate_proxies_on_import_undoably() {
        let mut doc = Document::new("t", 100.0, 100.0);
        doc.timeline = Some(TimelineProject::new());
        assert!(doc.timeline.as_ref().unwrap().settings.generate_proxies);

        let p = doc.timeline.as_ref().unwrap();
        let cmd = set_generate_proxies_on_import(p, false);
        assert_undo_roundtrip(&doc, &cmd);
        Command::Timeline(cmd).apply(&mut doc);
        assert!(!doc.timeline.as_ref().unwrap().settings.generate_proxies);

        let p = doc.timeline.as_ref().unwrap();
        let cmd = set_generate_proxies_on_import(p, true);
        assert_undo_roundtrip(&doc, &cmd);
        Command::Timeline(cmd).apply(&mut doc);
        assert!(doc.timeline.as_ref().unwrap().settings.generate_proxies);
    }

    // ── K-B1 / K-B2: track, master and asset effect stacks ───────────────────

    /// A fixture with all four scopes populated: one asset, one sequence, one
    /// video track, one clip on it that references the asset.
    fn scoped_fixture() -> (Document, SequenceId, TrackId, ClipId, AssetId) {
        use super::super::media::{AssetKind, AssetSource, MediaAsset};

        let mut project = TimelineProject::new();
        let asset = MediaAsset::new(
            AssetKind::Video,
            AssetSource::File {
                path: std::path::PathBuf::from("/a.mp4"),
                rel_path: None,
            },
        );
        let asset_id = asset.id;
        project.media.insert(asset);

        let mut sequence = Sequence::new("Seq", FrameRate::FPS_30, 1920, 1080);
        let mut vtrack = Track::new(TrackKind::Video, "V1");
        let c = Clip::new(ClipSource::Asset { asset: asset_id }, Tick(0), Tick(100));
        let clip_id = c.id;
        vtrack.clips.push(c);
        let track_id = vtrack.id;
        sequence.video_tracks.push(vtrack);
        let seq_id = sequence.id;
        project.insert_sequence(sequence);

        let mut doc = Document::new("t", 100.0, 100.0);
        doc.timeline = Some(project);
        (doc, seq_id, track_id, clip_id, asset_id)
    }

    fn owners(seq: SequenceId, track: TrackId, clip: ClipId, asset: AssetId) -> Vec<VfxOwner> {
        vec![
            VfxOwner::Clip(clip),
            VfxOwner::Track(track),
            VfxOwner::Master(seq),
            VfxOwner::Asset(asset),
        ]
    }

    fn fx(kind: EffectKind) -> ClipEffect {
        ClipEffect::new(kind)
    }

    /// Add/remove/reorder/set on every scope: each is one command, and each
    /// command's inverse restores the exact prior stack (order included).
    #[test]
    fn scoped_effect_ops_are_one_undo_unit_at_every_scope() {
        let (doc, seq, track, clip, asset) = scoped_fixture();
        for owner in owners(seq, track, clip, asset) {
            let mut d = doc.clone();

            // add ×3
            for kind in [EffectKind::Blur, EffectKind::Sharpen, EffectKind::Invert] {
                let p = d.timeline.as_ref().unwrap();
                let cmd = add_effect_scoped(p, owner, fx(kind), None).unwrap();
                assert_undo_roundtrip(&d, &cmd);
                Command::Timeline(cmd).apply(&mut d);
            }
            let stack = effect_stack(d.timeline.as_ref().unwrap(), owner).unwrap();
            assert_eq!(stack.len(), 3, "{owner:?}");
            assert_eq!(stack[0].kind, EffectKind::Blur, "{owner:?}");
            assert_eq!(stack[2].kind, EffectKind::Invert, "{owner:?}");

            // reorder (rotate) — undo must restore exact order, not membership
            let p = d.timeline.as_ref().unwrap();
            let cmd = reorder_effects_scoped(p, owner, vec![2, 0, 1]).unwrap();
            assert_undo_roundtrip(&d, &cmd);
            Command::Timeline(cmd).apply(&mut d);
            let kinds: Vec<EffectKind> = effect_stack(d.timeline.as_ref().unwrap(), owner)
                .unwrap()
                .iter()
                .map(|e| e.kind)
                .collect();
            assert_eq!(
                kinds,
                vec![EffectKind::Invert, EffectKind::Blur, EffectKind::Sharpen],
                "{owner:?}"
            );

            // set (disable the middle one)
            let p = d.timeline.as_ref().unwrap();
            let mut edited = effect_stack(p, owner).unwrap()[1].clone();
            edited.enabled = false;
            let cmd = set_effect_scoped(p, owner, 1, edited).unwrap();
            assert_undo_roundtrip(&d, &cmd);
            Command::Timeline(cmd).apply(&mut d);
            assert!(!effect_stack(d.timeline.as_ref().unwrap(), owner).unwrap()[1].enabled);

            // remove from the middle — the inverse must re-insert *at index 1*
            let p = d.timeline.as_ref().unwrap();
            let cmd = remove_effect_scoped(p, owner, 1).unwrap();
            assert_undo_roundtrip(&d, &cmd);
            Command::Timeline(cmd).apply(&mut d);
            let kinds: Vec<EffectKind> = effect_stack(d.timeline.as_ref().unwrap(), owner)
                .unwrap()
                .iter()
                .map(|e| e.kind)
                .collect();
            assert_eq!(
                kinds,
                vec![EffectKind::Invert, EffectKind::Sharpen],
                "{owner:?}"
            );
        }
    }

    /// The four stacks are independent — editing one never touches another.
    #[test]
    fn scoped_stacks_do_not_alias() {
        let (mut doc, seq, track, clip, asset) = scoped_fixture();
        for owner in owners(seq, track, clip, asset) {
            let p = doc.timeline.as_ref().unwrap();
            let cmd = add_effect_scoped(p, owner, fx(EffectKind::Blur), None).unwrap();
            Command::Timeline(cmd).apply(&mut doc);
        }
        let p = doc.timeline.as_ref().unwrap();
        for owner in owners(seq, track, clip, asset) {
            assert_eq!(effect_stack(p, owner).unwrap().len(), 1, "{owner:?}");
        }
        // Remove the track one only.
        let cmd = remove_effect_scoped(p, VfxOwner::Track(track), 0).unwrap();
        Command::Timeline(cmd).apply(&mut doc);
        let p = doc.timeline.as_ref().unwrap();
        assert!(effect_stack(p, VfxOwner::Track(track)).unwrap().is_empty());
        assert_eq!(effect_stack(p, VfxOwner::Clip(clip)).unwrap().len(), 1);
        assert_eq!(effect_stack(p, VfxOwner::Master(seq)).unwrap().len(), 1);
        assert_eq!(effect_stack(p, VfxOwner::Asset(asset)).unwrap().len(), 1);
    }

    #[test]
    fn scoped_grade_roundtrips_at_every_scope() {
        let (doc, seq, track, clip, asset) = scoped_fixture();
        for owner in owners(seq, track, clip, asset) {
            let mut d = doc.clone();
            let p = d.timeline.as_ref().unwrap();
            assert!(scope_grade(p, owner).unwrap().is_none(), "{owner:?}");
            let cmd = set_grade_scoped(p, owner, Some(Grade::new())).unwrap();
            assert_undo_roundtrip(&d, &cmd);
            Command::Timeline(cmd).apply(&mut d);
            assert!(scope_grade(d.timeline.as_ref().unwrap(), owner)
                .unwrap()
                .is_some());

            // …and clearing it back to neutral is also one undoable step.
            let p = d.timeline.as_ref().unwrap();
            let cmd = set_grade_scoped(p, owner, None).unwrap();
            assert_undo_roundtrip(&d, &cmd);
            Command::Timeline(cmd).apply(&mut d);
            assert!(scope_grade(d.timeline.as_ref().unwrap(), owner)
                .unwrap()
                .is_none());
        }
    }

    /// A missing owner is refused before a command exists, with the error that
    /// names the missing thing.
    #[test]
    fn scoped_ops_reject_unknown_owners_and_bad_indices() {
        let (doc, seq, track, clip, asset) = scoped_fixture();
        let p = doc.timeline.as_ref().unwrap();

        let ghost_clip = ClipId::new();
        let ghost_track = TrackId::new();
        let ghost_seq = SequenceId::new();
        let ghost_asset = AssetId::new();
        assert_eq!(
            add_effect_scoped(p, VfxOwner::Clip(ghost_clip), fx(EffectKind::Blur), None),
            Err(EditError::NoClip(ghost_clip))
        );
        assert_eq!(
            add_effect_scoped(p, VfxOwner::Track(ghost_track), fx(EffectKind::Blur), None),
            Err(EditError::NoTrack(ghost_track))
        );
        assert_eq!(
            add_effect_scoped(p, VfxOwner::Master(ghost_seq), fx(EffectKind::Blur), None),
            Err(EditError::NoSequence(ghost_seq))
        );
        assert_eq!(
            add_effect_scoped(p, VfxOwner::Asset(ghost_asset), fx(EffectKind::Blur), None),
            Err(EditError::NoAsset(ghost_asset))
        );

        for owner in owners(seq, track, clip, asset) {
            assert_eq!(
                remove_effect_scoped(p, owner, 0),
                Err(EditError::IndexOutOfRange)
            );
            assert_eq!(
                set_effect_scoped(p, owner, 0, fx(EffectKind::Blur)),
                Err(EditError::IndexOutOfRange)
            );
            // Wrong length, and (with the right length) a non-permutation.
            assert_eq!(
                reorder_effects_scoped(p, owner, vec![0]),
                Err(EditError::IndexOutOfRange)
            );
        }

        // A same-length order that duplicates a slot would drop an effect.
        let mut d = doc.clone();
        for kind in [EffectKind::Blur, EffectKind::Sharpen] {
            let p = d.timeline.as_ref().unwrap();
            let cmd = add_effect_scoped(p, VfxOwner::Track(track), fx(kind), None).unwrap();
            Command::Timeline(cmd).apply(&mut d);
        }
        assert_eq!(
            reorder_effects_scoped(
                d.timeline.as_ref().unwrap(),
                VfxOwner::Track(track),
                vec![0, 0]
            ),
            Err(EditError::IndexOutOfRange)
        );
    }

    /// `index` places the effect: stacks are ordered, so insert-at-0 must go to
    /// the head at every scope (an appended-only stack would silently reorder
    /// the render on undo).
    #[test]
    fn scoped_add_honours_index() {
        let (mut doc, _seq, track, _clip, _asset) = scoped_fixture();
        let owner = VfxOwner::Track(track);
        for kind in [EffectKind::Blur, EffectKind::Sharpen] {
            let p = doc.timeline.as_ref().unwrap();
            let cmd = add_effect_scoped(p, owner, fx(kind), None).unwrap();
            Command::Timeline(cmd).apply(&mut doc);
        }
        let p = doc.timeline.as_ref().unwrap();
        let cmd = add_effect_scoped(p, owner, fx(EffectKind::Invert), Some(0)).unwrap();
        assert_undo_roundtrip(&doc, &cmd);
        Command::Timeline(cmd).apply(&mut doc);
        let kinds: Vec<EffectKind> = effect_stack(doc.timeline.as_ref().unwrap(), owner)
            .unwrap()
            .iter()
            .map(|e| e.kind)
            .collect();
        assert_eq!(
            kinds,
            vec![EffectKind::Invert, EffectKind::Blur, EffectKind::Sharpen]
        );

        // Out-of-range index clamps to the tail rather than failing.
        let p = doc.timeline.as_ref().unwrap();
        let cmd = add_effect_scoped(p, owner, fx(EffectKind::Glow), Some(99)).unwrap();
        Command::Timeline(cmd).apply(&mut doc);
        assert_eq!(
            effect_stack(doc.timeline.as_ref().unwrap(), owner)
                .unwrap()
                .last()
                .unwrap()
                .kind,
            EffectKind::Glow
        );
    }

    /// The clip-shaped wrappers still validate the `(seq, track, clip)` triple
    /// and produce the same clip-scoped command as the general form.
    #[test]
    fn clip_wrappers_delegate_to_the_scoped_form() {
        let (doc, seq, track, clip, _asset) = scoped_fixture();
        let p = doc.timeline.as_ref().unwrap();
        assert_eq!(
            add_effect(p, seq, track, clip, fx(EffectKind::Blur), None).unwrap(),
            add_effect_scoped(p, VfxOwner::Clip(clip), fx(EffectKind::Blur), None).unwrap(),
        );
        assert_eq!(
            set_grade(p, seq, track, clip, Some(Grade::new())).unwrap(),
            set_grade_scoped(p, VfxOwner::Clip(clip), Some(Grade::new())).unwrap(),
        );
        // Wrong track for that clip is still rejected by the wrapper.
        let other = Track::new(TrackKind::Video, "V2");
        let other_id = other.id;
        let mut d = doc.clone();
        d.timeline
            .as_mut()
            .unwrap()
            .sequences
            .get_mut(&seq)
            .unwrap()
            .video_tracks
            .push(other);
        assert_eq!(
            add_effect(
                d.timeline.as_ref().unwrap(),
                seq,
                other_id,
                clip,
                fx(EffectKind::Blur),
                None
            ),
            Err(EditError::NoClip(clip))
        );
    }

    /// A scoped stack survives a save/load round-trip (additive serde, 35 §2).
    #[test]
    fn scoped_stacks_survive_serde_roundtrip() {
        let (mut doc, seq, track, clip, asset) = scoped_fixture();
        for owner in owners(seq, track, clip, asset) {
            let p = doc.timeline.as_ref().unwrap();
            let cmd = add_effect_scoped(p, owner, fx(EffectKind::Blur), None).unwrap();
            Command::Timeline(cmd).apply(&mut doc);
            let p = doc.timeline.as_ref().unwrap();
            let cmd = set_grade_scoped(p, owner, Some(Grade::new())).unwrap();
            Command::Timeline(cmd).apply(&mut doc);
        }
        let project = doc.timeline.as_ref().unwrap();
        let json = serde_json::to_string(project).unwrap();
        let back: TimelineProject = serde_json::from_str(&json).unwrap();
        assert_eq!(&back, project);
        for owner in owners(seq, track, clip, asset) {
            assert_eq!(effect_stack(&back, owner).unwrap().len(), 1, "{owner:?}");
            assert!(scope_grade(&back, owner).unwrap().is_some(), "{owner:?}");
        }
    }

    /// The commands themselves round-trip through serde (they cross the MCP
    /// boundary and the history journal as data).
    #[test]
    fn scoped_effect_commands_serde_roundtrip() {
        let (doc, seq, track, clip, asset) = scoped_fixture();
        let p = doc.timeline.as_ref().unwrap();
        for owner in owners(seq, track, clip, asset) {
            let cmd = add_effect_scoped(p, owner, fx(EffectKind::Blur), None).unwrap();
            let json = serde_json::to_string(&cmd).unwrap();
            let back: TimelineCmd = serde_json::from_str(&json).unwrap();
            assert_eq!(back, cmd, "{owner:?}");
        }
    }

    /// A slider drag on a stacked effect coalesces into ONE undo unit that
    /// keeps the gesture's original before-state; a different index or a
    /// different scope never merges.
    #[test]
    fn set_effect_coalesces_per_owner_and_index() {
        let (mut doc, _seq, track, clip, _asset) = scoped_fixture();
        for _ in 0..2 {
            let p = doc.timeline.as_ref().unwrap();
            let cmd =
                add_effect_scoped(p, VfxOwner::Track(track), fx(EffectKind::Blur), None).unwrap();
            Command::Timeline(cmd).apply(&mut doc);
        }
        let p = doc.timeline.as_ref().unwrap();
        let owner = VfxOwner::Track(track);

        let mut a = effect_stack(p, owner).unwrap()[0].clone();
        a.enabled = false;
        let first = set_effect_scoped(p, owner, 0, a).unwrap();
        let mut b = effect_stack(p, owner).unwrap()[0].clone();
        b.enabled = true;
        b.params.base.entries.push((
            PropPath::new("params.radius"),
            super::super::anim::PropValue::Float(0.25),
        ));
        let second = set_effect_scoped(p, owner, 0, b.clone()).unwrap();

        let merged = TimelineCmd::coalesce(&first, &second).expect("same owner+index merges");
        match (&merged, &first) {
            (
                TimelineCmd::SetEffect { old, new, .. },
                TimelineCmd::SetEffect { old: first_old, .. },
            ) => {
                assert_eq!(old, first_old, "the anchor's before-state is kept");
                assert_eq!(**new, b, "the incoming after-state is adopted");
            }
            _ => panic!("expected SetEffect"),
        }

        // Different index / different scope: no merge.
        let other_index = set_effect_scoped(p, owner, 1, fx(EffectKind::Blur)).unwrap();
        assert!(TimelineCmd::coalesce(&first, &other_index).is_none());
        let other_scope =
            set_effect_scoped(p, VfxOwner::Clip(clip), 0, fx(EffectKind::Blur)).unwrap_err();
        assert_eq!(other_scope, EditError::IndexOutOfRange);
    }

    #[test]
    fn set_effect_zone_writes_half_open_range_and_refuses_bad() {
        let (mut doc, _seq, _track, clip, _asset) = scoped_fixture();
        {
            let p = doc.timeline.as_ref().unwrap();
            let cmd =
                add_effect_scoped(p, VfxOwner::Clip(clip), fx(EffectKind::Blur), None).unwrap();
            Command::Timeline(cmd).apply(&mut doc);
        }
        let p = doc.timeline.as_ref().unwrap();
        assert!(matches!(
            set_effect_zone(p, VfxOwner::Clip(clip), 0, Some((Tick(10), Tick(10)))),
            Err(EditError::NonPositiveDuration)
        ));
        let cmd = set_effect_zone(p, VfxOwner::Clip(clip), 0, Some((Tick(10), Tick(40)))).unwrap();
        Command::Timeline(cmd).apply(&mut doc);
        let stack = effect_stack(doc.timeline.as_ref().unwrap(), VfxOwner::Clip(clip)).unwrap();
        assert_eq!(stack[0].zone, Some((Tick(10), Tick(40))));
        assert!(stack[0].active_at(Tick(10)));
        assert!(!stack[0].active_at(Tick(40)));
        // Clear.
        let p = doc.timeline.as_ref().unwrap();
        let clear = set_effect_zone(p, VfxOwner::Clip(clip), 0, None).unwrap();
        Command::Timeline(clear).apply(&mut doc);
        assert_eq!(
            effect_stack(doc.timeline.as_ref().unwrap(), VfxOwner::Clip(clip)).unwrap()[0].zone,
            None
        );
    }

    // ── Paste Attributes (26 §10 K-B15) ─────────────────────────────────────

    /// Two video tracks, four clips, and a source clip loaded with one of every
    /// pasteable family: two effects, a grade carrying a `Lut3d` asset ref, a
    /// non-default animated transform, a per-format `reframe` override (which
    /// must NOT travel), and
    /// clip audio whose `fade_in` is deliberately LONGER than one target clip.
    /// Every target's timing differs from the source's so "timing untouched" is
    /// a claim the assertions can actually falsify.
    #[allow(clippy::type_complexity)]
    fn paste_attr_fixture() -> (
        Document,
        SequenceId,
        (TrackId, TrackId),
        (ClipId, ClipId, ClipId, ClipId),
        AssetId,
    ) {
        use super::super::audio::{AudioFade, ChannelMap, ClipAudio, ClipAudioParams, FadeShape};
        use super::super::grade::{GradeOpKind, GradeOpParams, LutInterp};
        use super::super::media::{AssetKind, AssetSource, MediaAsset};

        let mut project = TimelineProject::new();
        let media = MediaAsset::new(
            AssetKind::Video,
            AssetSource::File {
                path: std::path::PathBuf::from("/a.mp4"),
                rel_path: None,
            },
        );
        let media_id = media.id;
        project.media.insert(media);
        let lut = MediaAsset::new(
            AssetKind::Video,
            AssetSource::File {
                path: std::path::PathBuf::from("/look.cube"),
                rel_path: None,
            },
        );
        let lut_id = lut.id;
        project.media.insert(lut);

        let src_of = |start: i64, dur: i64| {
            Clip::new(
                ClipSource::Asset { asset: media_id },
                Tick(start),
                Tick(dur),
            )
        };

        // Source: V1 @ 0..100, fully dressed.
        let mut src = src_of(0, 100);
        src.name = "source".into();
        src.effects = vec![fx(EffectKind::Blur), fx(EffectKind::Sharpen)];
        src.grade = Some(Grade {
            ops: vec![super::super::grade::GradeOp::new(
                GradeOpKind::Lut3d,
                GradeOpParams::Lut3d {
                    asset: lut_id,
                    intensity: 0.75,
                    interp: LutInterp::Tetrahedral,
                },
            )],
            bypass: false,
        });
        src.transform.base.opacity = 0.5;
        src.transform.base.scale_x = 2.0;
        let mut opacity_track = PropertyTrack::new("transform.opacity");
        opacity_track.keyframes.push(Keyframe::new(
            Tick(0),
            crate::timeline::PropValue::Float(0.5),
            Interp::Linear,
        ));
        src.transform.tracks.push(opacity_track);
        src.reframe.insert(
            1,
            ClipTransform {
                x: 42.0,
                ..ClipTransform::default()
            },
        );
        src.audio = Some(ClipAudio {
            params: AnimProps::new(ClipAudioParams { gain_db: -6.0 }),
            // 90 ticks: longer than target `b` below (80), shorter than the rest.
            fade_in: Some(AudioFade {
                duration: Tick(90),
                shape: FadeShape::EqualPower,
            }),
            fade_out: None,
            channel_map: ChannelMap::MonoDownmix,
            stream: None,
            offset: Tick::ZERO,
        });
        let src_id = src.id;

        // Targets: differing starts, durations and source_ins.
        let mut a = src_of(200, 150);
        a.name = "a".into();
        a.source_in = Tick(7);
        let a_id = a.id;
        let mut b = src_of(0, 80);
        b.name = "b".into();
        b.source_in = Tick(11);
        let b_id = b.id;
        let mut c = src_of(400, 60);
        c.name = "c".into();
        c.source_in = Tick(13);
        let c_id = c.id;

        let mut sequence = Sequence::new("Seq", FrameRate::FPS_30, 1920, 1080);
        sequence
            .formats
            .push(SequenceFormat::new("Vertical", 1080, 1920));
        let mut v1 = Track::new(TrackKind::Video, "V1");
        v1.clips.push(src);
        v1.clips.push(a);
        let v1_id = v1.id;
        let mut v2 = Track::new(TrackKind::Video, "V2");
        v2.clips.push(b);
        v2.clips.push(c);
        let v2_id = v2.id;
        sequence.video_tracks.push(v1);
        sequence.video_tracks.push(v2);
        let seq_id = sequence.id;
        project.insert_sequence(sequence);

        let mut doc = Document::new("t", 100.0, 100.0);
        doc.timeline = Some(project);
        (
            doc,
            seq_id,
            (v1_id, v2_id),
            (src_id, a_id, b_id, c_id),
            lut_id,
        )
    }

    fn clip_by_id(d: &Document, id: ClipId) -> Clip {
        let p = d.timeline.as_ref().unwrap();
        locate_clip_anywhere(p, id)
            .unwrap_or_else(|| panic!("clip {id} vanished"))
            .2
            .clone()
    }

    /// The headline: the look transfers in full, and NOTHING that identifies or
    /// times a clip moves. Each "unchanged" assertion is paired with a check
    /// that the source really did differ, so the test cannot pass vacuously.
    #[test]
    fn paste_attributes_carries_the_look_and_never_the_timing() {
        let (doc, _seq, _tracks, (src, a, b, c), _lut) = paste_attr_fixture();
        let source = clip_by_id(&doc, src);
        let before: Vec<Clip> = [a, b, c].iter().map(|&i| clip_by_id(&doc, i)).collect();

        // Fixture sanity: the source's timing genuinely differs from every
        // target's, so "timing untouched" below is falsifiable.
        for t in &before {
            assert_ne!(
                (t.start, t.duration, t.source_in),
                (source.start, source.duration, source.source_in),
                "fixture must give {} timing distinct from the source",
                t.name
            );
            assert_ne!(t.effects, source.effects, "fixture target already matches");
        }

        let attrs = clip_attributes(doc.timeline.as_ref().unwrap(), src).unwrap();
        let cmds = paste_clip_attributes(
            doc.timeline.as_ref().unwrap(),
            &attrs,
            &[a, b, c],
            AttrSelector::ALL,
        )
        .unwrap();
        assert_eq!(cmds.len(), 3, "one SetClipProp per target");

        let mut d = doc.clone();
        Command::Batch(cmds.into_iter().map(Command::Timeline).collect()).apply(&mut d);

        for (i, &id) in [a, b, c].iter().enumerate() {
            let after = clip_by_id(&d, id);
            let prev = &before[i];
            // Look: carried.
            assert_eq!(after.effects, source.effects, "{}: effects", after.name);
            assert_eq!(after.grade, source.grade, "{}: grade", after.name);
            assert_eq!(
                after.transform.base, source.transform.base,
                "{}: transform",
                after.name
            );
            assert_eq!(
                after.transform.tracks, source.transform.tracks,
                "{}: transform keyframes",
                after.name
            );
            // `reframe` is deliberately NOT in the pasted set — the target
            // keeps its own per-format overrides (see this section's comment).
            assert_eq!(after.reframe, prev.reframe, "{}: reframe", after.name);
            assert_ne!(
                after.reframe, source.reframe,
                "{}: fixture must give the source a reframe the target lacks, \
                 else the exclusion above is untested",
                after.name
            );
            assert_eq!(
                after.audio.as_ref().map(|x| x.params.base.gain_db),
                Some(-6.0),
                "{}: audio gain",
                after.name
            );
            // Identity and timing: untouched.
            assert_eq!(after.id, prev.id);
            assert_eq!(after.name, prev.name);
            assert_eq!(after.start, prev.start, "{}: start moved", after.name);
            assert_eq!(after.duration, prev.duration, "{}: duration", after.name);
            assert_eq!(after.source_in, prev.source_in, "{}: source_in", after.name);
            assert_eq!(after.source, prev.source);
            assert_eq!(after.speed, prev.speed, "{}: speed", after.name);
            assert_eq!(after.composition, prev.composition);
            assert_eq!(after.transition_in, prev.transition_in);
            assert_eq!(after.transition_out, prev.transition_out);
        }
        // The source itself was not a target and is unchanged.
        assert_eq!(clip_by_id(&d, src), source);
    }

    /// THE CRUX (26 §10 K-B15): pasting onto N clips is ONE undo step, and one
    /// undo restores all N.
    #[test]
    fn paste_attributes_is_one_undo_unit_across_a_multi_selection() {
        let (mut doc, _seq, _tracks, (src, a, b, c), _lut) = paste_attr_fixture();
        let before = doc.timeline.clone();

        let attrs = clip_attributes(doc.timeline.as_ref().unwrap(), src).unwrap();
        let cmds = paste_clip_attributes(
            doc.timeline.as_ref().unwrap(),
            &attrs,
            &[a, b, c],
            AttrSelector::ALL,
        )
        .unwrap();
        assert_eq!(cmds.len(), 3);

        let mut history = crate::history::CommandHistory::new(64);
        let depth_before = history.undo_depth();
        history.execute_discrete(
            Command::Batch(cmds.into_iter().map(Command::Timeline).collect()),
            &mut doc,
        );
        assert_eq!(
            history.undo_depth() - depth_before,
            1,
            "a 3-clip paste must record exactly ONE undo step, not three"
        );
        let after = doc.timeline.clone();
        assert_ne!(
            after, before,
            "the paste must actually have changed the doc"
        );

        assert!(history.undo(&mut doc));
        assert_eq!(
            doc.timeline, before,
            "a single undo must restore all three targets"
        );
        assert!(history.redo(&mut doc));
        assert_eq!(doc.timeline, after, "a single redo must re-apply all three");
    }

    /// The reason a `Command::Batch` is safe here at all: `TimelineCmd::apply`
    /// debug-asserts `Sequence::validate()` after EVERY member, so a multi-clip
    /// edit is only batchable if each intermediate state is valid. It is,
    /// because the pasted attribute set touches nothing `validate()` reads.
    ///
    /// The second half proves that is a real property of the chosen set, not
    /// luck: pasting `transition_out` too — the family deliberately excluded —
    /// DOES break the same invariant.
    #[test]
    fn paste_attributes_batch_never_breaks_the_sequence_invariant() {
        let (doc, seq, _tracks, (src, a, b, c), _lut) = paste_attr_fixture();
        let attrs = clip_attributes(doc.timeline.as_ref().unwrap(), src).unwrap();
        let cmds = paste_clip_attributes(
            doc.timeline.as_ref().unwrap(),
            &attrs,
            &[a, b, c],
            AttrSelector::ALL,
        )
        .unwrap();

        let mut d = doc.clone();
        for (i, cmd) in cmds.iter().enumerate() {
            Command::Timeline(cmd.clone()).apply(&mut d);
            let s = d.timeline.as_ref().unwrap().sequences.get(&seq).unwrap();
            assert!(
                s.validate().is_ok(),
                "invariant broken after batch member {i}: {:?}",
                s.validate()
            );
        }

        // Sensitivity: had the attribute set included transitions, the same
        // "just stamp the source's fields onto the target" move would break it.
        // `src` is followed on V1 by `a` at a hard cut in `before`, so a pasted
        // `transition_out` on `src` is exactly `TransitionOutAtCut`.
        let mut d2 = doc.clone();
        {
            let p = d2.timeline.as_mut().unwrap();
            let s = p.sequences.get_mut(&seq).unwrap();
            let t = &mut s.video_tracks[0];
            // Butt `a` up against `src` to form the hard cut, then stamp a
            // transition the way an over-broad paste would.
            let src_end = t.clips[0].end();
            t.clips[1].start = src_end;
            t.clips[0].transition_out = Some(super::super::clip::Transition::new(
                super::super::clip::TransitionKind::CrossDissolve,
                Tick(10),
            ));
            assert!(
                s.validate().is_err(),
                "control: pasting a transition at a hard cut must be the thing \
                 that breaks validate(), else this test proves nothing"
            );
        }
    }

    /// Each selector flag moves exactly its own family and leaves the other
    /// three alone (a `false` flag is "do not touch", not "reset to default").
    #[test]
    fn paste_attributes_selector_flags_are_independent() {
        let (doc, _seq, _tracks, (src, a, _b, _c), _lut) = paste_attr_fixture();
        let attrs = clip_attributes(doc.timeline.as_ref().unwrap(), src).unwrap();
        let source = clip_by_id(&doc, src);
        let base = clip_by_id(&doc, a);

        let only = |effects, grade, transform, audio| AttrSelector {
            effects,
            grade,
            transform,
            audio,
        };
        for (sel, name) in [
            (only(true, false, false, false), "effects"),
            (only(false, true, false, false), "grade"),
            (only(false, false, true, false), "transform"),
            (only(false, false, false, true), "audio"),
        ] {
            let cmds =
                paste_clip_attributes(doc.timeline.as_ref().unwrap(), &attrs, &[a], sel).unwrap();
            assert_eq!(cmds.len(), 1, "{name}: expected one command");
            let mut d = doc.clone();
            Command::Batch(cmds.into_iter().map(Command::Timeline).collect()).apply(&mut d);
            let after = clip_by_id(&d, a);

            let want_effects = if sel.effects { &source } else { &base };
            let want_grade = if sel.grade { &source } else { &base };
            let want_transform = if sel.transform { &source } else { &base };
            let want_audio = if sel.audio { &source } else { &base };
            assert_eq!(after.effects, want_effects.effects, "{name}: effects");
            assert_eq!(after.grade, want_grade.grade, "{name}: grade");
            assert_eq!(
                after.transform.base, want_transform.transform.base,
                "{name}: transform"
            );
            assert_eq!(after.reframe, base.reframe, "{name}: reframe excluded");
            assert_eq!(
                after.audio.is_some(),
                want_audio.audio.is_some(),
                "{name}: audio"
            );
        }

        // An all-false selector produces no commands at all.
        assert!(paste_clip_attributes(
            doc.timeline.as_ref().unwrap(),
            &attrs,
            &[a],
            only(false, false, false, false),
        )
        .unwrap()
        .is_empty());
    }

    /// An `AudioFade` longer than its clip never reaches unity gain (the mixer
    /// divides elapsed by `fade.duration`), so a pasted fade is clamped to the
    /// TARGET's duration, per target.
    #[test]
    fn paste_attributes_clamps_an_over_long_audio_fade() {
        let (doc, _seq, _tracks, (src, a, b, _c), _lut) = paste_attr_fixture();
        let attrs = clip_attributes(doc.timeline.as_ref().unwrap(), src).unwrap();
        let src_fade = attrs.audio.as_ref().unwrap().fade_in.unwrap().duration;
        let b_dur = clip_by_id(&doc, b).duration;
        let a_dur = clip_by_id(&doc, a).duration;
        // Fixture sanity — one target is shorter than the fade and one is not,
        // so the test exercises both the clamped and the unclamped branch.
        assert!(src_fade > b_dur, "fixture: b must be shorter than the fade");
        assert!(src_fade < a_dur, "fixture: a must be longer than the fade");

        let cmds = paste_clip_attributes(
            doc.timeline.as_ref().unwrap(),
            &attrs,
            &[a, b],
            AttrSelector::ALL,
        )
        .unwrap();
        let mut d = doc.clone();
        Command::Batch(cmds.into_iter().map(Command::Timeline).collect()).apply(&mut d);

        assert_eq!(
            clip_by_id(&d, b).audio.unwrap().fade_in.unwrap().duration,
            b_dur,
            "an over-long fade must clamp to the target's own duration"
        );
        assert_eq!(
            clip_by_id(&d, a).audio.unwrap().fade_in.unwrap().duration,
            src_fade,
            "a fade that fits must be carried verbatim"
        );
    }

    /// 26 §10 K-B15's watch-out: a grade carrying a `Lut3d` whose asset is not
    /// in this project is refused, and an unknown target id refuses the WHOLE
    /// paste rather than landing on the clips it could resolve.
    #[test]
    fn paste_attributes_refuses_a_missing_lut_and_an_unknown_target() {
        let (mut doc, _seq, _tracks, (src, a, b, _c), lut) = paste_attr_fixture();
        let attrs = clip_attributes(doc.timeline.as_ref().unwrap(), src).unwrap();

        // Control: with the LUT asset present the paste is accepted.
        assert!(paste_clip_attributes(
            doc.timeline.as_ref().unwrap(),
            &attrs,
            &[a],
            AttrSelector::ALL
        )
        .is_ok());

        // Drop the LUT the way `remove_asset` would, leaving a stale clipboard.
        doc.timeline.as_mut().unwrap().media.assets.remove(&lut);
        assert_eq!(
            paste_clip_attributes(
                doc.timeline.as_ref().unwrap(),
                &attrs,
                &[a],
                AttrSelector::ALL
            ),
            Err(EditError::NoAsset(lut))
        );
        // …but a paste that does not carry the grade is still fine.
        assert!(paste_clip_attributes(
            doc.timeline.as_ref().unwrap(),
            &attrs,
            &[a],
            AttrSelector::EFFECTS_ONLY
        )
        .is_ok());

        // An unknown target aborts the whole call — no partial paste.
        let ghost = ClipId::new();
        assert_eq!(
            paste_clip_attributes(
                doc.timeline.as_ref().unwrap(),
                &attrs,
                &[a, ghost, b],
                AttrSelector::EFFECTS_ONLY
            ),
            Err(EditError::NoClip(ghost))
        );
    }

    /// A target that already matches contributes no command, so "paste again"
    /// cannot push an empty undo step; a duplicated id is one paste, not two.
    #[test]
    fn paste_attributes_skips_no_op_targets() {
        let (doc, _seq, _tracks, (src, a, _b, _c), _lut) = paste_attr_fixture();
        let attrs = clip_attributes(doc.timeline.as_ref().unwrap(), src).unwrap();

        // Onto the source itself: nothing to do.
        assert!(paste_clip_attributes(
            doc.timeline.as_ref().unwrap(),
            &attrs,
            &[src],
            AttrSelector::ALL
        )
        .unwrap()
        .is_empty());

        // A duplicated target id collapses to one command…
        let cmds = paste_clip_attributes(
            doc.timeline.as_ref().unwrap(),
            &attrs,
            &[a, a],
            AttrSelector::ALL,
        )
        .unwrap();
        assert_eq!(cmds.len(), 1);

        // …and re-pasting the same attributes afterwards is a no-op.
        let mut d = doc.clone();
        Command::Batch(cmds.into_iter().map(Command::Timeline).collect()).apply(&mut d);
        assert!(paste_clip_attributes(
            d.timeline.as_ref().unwrap(),
            &attrs,
            &[a],
            AttrSelector::ALL
        )
        .unwrap()
        .is_empty());
    }

    /// Every command inverts cleanly (undo/redo identity, DoD §4) and the
    /// pasted state survives a save/load round-trip — no new document fields,
    /// so this is additive-in-v5 by construction (`SetClipProp` of existing
    /// `Clip` fields), and this pins that nothing pasted fails to serialize.
    #[test]
    fn paste_attributes_commands_invert_and_survive_serde() {
        let (mut doc, _seq, _tracks, (src, a, b, c), _lut) = paste_attr_fixture();
        let attrs = clip_attributes(doc.timeline.as_ref().unwrap(), src).unwrap();
        let cmds = paste_clip_attributes(
            doc.timeline.as_ref().unwrap(),
            &attrs,
            &[a, b, c],
            AttrSelector::ALL,
        )
        .unwrap();
        for cmd in &cmds {
            let json = serde_json::to_string(cmd).unwrap();
            let back: TimelineCmd = serde_json::from_str(&json).unwrap();
            assert_eq!(&back, cmd);
        }
        // Applied one at a time, each inverts to the exact prior state.
        for cmd in &cmds {
            assert_undo_roundtrip(&doc, cmd);
        }
        Command::Batch(cmds.into_iter().map(Command::Timeline).collect()).apply(&mut doc);

        let project = doc.timeline.as_ref().unwrap();
        let json = serde_json::to_string(project).unwrap();
        let back: TimelineProject = serde_json::from_str(&json).unwrap();
        assert_eq!(&back, project, "pasted attributes must round-trip");
        assert_eq!(
            back.sequences[&_seq]
                .tracks()
                .flat_map(|t| t.clips.iter())
                .filter(|cl| cl.effects.len() == 2 && cl.grade.is_some())
                .count(),
            4,
            "source + three targets all carry the stack after reload"
        );
    }

    // ── K-C6: batch relink ──────────────────────────────────────────────────

    use super::super::media::{AssetKind, AssetSource, MediaAsset, MediaProbe};

    /// A project holding `(path, hash)` assets, plus the ids in the same order.
    fn relink_fixture(rows: &[(&str, Option<&str>)]) -> (Document, Vec<AssetId>) {
        let mut project = TimelineProject::new();
        let mut ids = Vec::new();
        for (path, hash) in rows {
            let mut a = MediaAsset::from_file(AssetKind::Video, *path);
            a.content_hash = hash.map(|h| h.to_string());
            ids.push(a.id);
            project.media.insert(a);
        }
        let mut doc = Document::new("t", 100.0, 100.0);
        doc.timeline = Some(project);
        (doc, ids)
    }

    fn cand(path: &str, hash: Option<&str>) -> RelinkCandidate {
        RelinkCandidate {
            path: PathBuf::from(path),
            content_hash: hash.map(|h| h.to_string()),
        }
    }

    /// A hasher that knows what each file on the fake disk contains.
    fn faked<'a>(
        table: &'a [(&'a str, &'a str)],
    ) -> impl FnMut(Option<&str>, &std::path::Path) -> Option<String> + 'a {
        move |_stored, path| {
            table
                .iter()
                .find(|(p, _)| std::path::Path::new(p) == path)
                .map(|(_, h)| h.to_string())
        }
    }

    #[test]
    fn offline_assets_are_only_the_unreachable_file_backed_ones() {
        let (mut doc, ids) = relink_fixture(&[("/vol/a.mp4", None), ("/gone/b.mp4", None)]);
        let embedded = MediaAsset::new(
            AssetKind::VectorDoc,
            AssetSource::EmbeddedVector {
                root: super::super::media::VectorRef::WholeDocument,
            },
        );
        let embedded_id = embedded.id;
        doc.timeline.as_mut().unwrap().media.insert(embedded);

        let offline = offline_assets(doc.timeline.as_ref().unwrap(), |p| {
            p == std::path::Path::new("/vol/a.mp4")
        });
        assert_eq!(offline, vec_sorted(&[ids[1]]));
        // Non-vacuous: the other two assets exist in the pool and are excluded
        // for two different reasons (reachable file / no file at all).
        assert_eq!(doc.timeline.as_ref().unwrap().media.assets.len(), 3);
        assert!(!offline.contains(&ids[0]));
        assert!(!offline.contains(&embedded_id));
    }

    fn vec_sorted(ids: &[AssetId]) -> Vec<AssetId> {
        let mut v = ids.to_vec();
        v.sort();
        v
    }

    /// The relink identity is the hash, not the name: a scan holding both a
    /// same-named *different* file and a renamed *identical* one binds the
    /// identical bytes.
    #[test]
    fn plan_relink_prefers_content_hash_over_a_same_named_different_file() {
        let (doc, ids) = relink_fixture(&[("/old/a.mp4", Some("xxh:aaa"))]);
        let p = doc.timeline.as_ref().unwrap();
        let disk = [("/new/a.mp4", "xxh:zzz"), ("/new/take1.mp4", "xxh:aaa")];
        let candidates = [
            cand("/new/a.mp4", Some("xxh:zzz")),
            cand("/new/take1.mp4", Some("xxh:aaa")),
        ];

        let plan = plan_relink(p, &ids, &candidates, faked(&disk));
        assert_eq!(plan.entries.len(), 1);
        let e = &plan.entries[0];
        assert_eq!(e.new_path, PathBuf::from("/new/take1.mp4"));
        assert_eq!(e.matched_by, RelinkMatchKind::ContentHash);
        assert_eq!(e.hash, RelinkHashCheck::Match);

        // Sensitivity: drop the hash match and the SAME scan now binds the
        // same-named file — and flags it as different bytes.
        let plan = plan_relink(p, &ids, &candidates[..1], faked(&disk));
        let e = &plan.entries[0];
        assert_eq!(e.new_path, PathBuf::from("/new/a.mp4"));
        assert_eq!(e.matched_by, RelinkMatchKind::ExactName);
        assert_eq!(e.hash, RelinkHashCheck::Mismatch);
        assert_eq!(e.new_hash.as_deref(), Some("xxh:zzz"));
    }

    #[test]
    fn plan_relink_falls_back_from_exact_to_case_insensitive_name() {
        let (doc, ids) = relink_fixture(&[("/old/Clip_01.MOV", None)]);
        let p = doc.timeline.as_ref().unwrap();

        // Exact name wins when present.
        let plan = plan_relink(
            p,
            &ids,
            &[
                cand("/new/clip_01.mov", None),
                cand("/new/Clip_01.MOV", None),
            ],
            faked(&[]),
        );
        assert_eq!(plan.entries[0].new_path, PathBuf::from("/new/Clip_01.MOV"));
        assert_eq!(plan.entries[0].matched_by, RelinkMatchKind::ExactName);

        // Only the case-folded name survives the copy → still relinks.
        let plan = plan_relink(p, &ids, &[cand("/new/clip_01.mov", None)], faked(&[]));
        assert_eq!(plan.entries[0].new_path, PathBuf::from("/new/clip_01.mov"));
        assert_eq!(
            plan.entries[0].matched_by,
            RelinkMatchKind::CaseInsensitiveName
        );

        // Nothing resembling it → reported as unmatched, never guessed at.
        let plan = plan_relink(p, &ids, &[cand("/new/other.mov", None)], faked(&[]));
        assert!(plan.entries.is_empty());
        assert_eq!(plan.unmatched, ids);
    }

    /// An unverifiable hash must read `Unknown`, never `Mismatch` — a false
    /// mismatch would teach users to click through the one integrity guard.
    #[test]
    fn plan_relink_never_manufactures_a_mismatch_it_could_not_measure() {
        let (doc, ids) = relink_fixture(&[("/old/a.mp4", Some("siphash64:0011223344556677"))]);
        let p = doc.timeline.as_ref().unwrap();
        let candidates = [cand("/new/a.mp4", Some("xxh:zzz"))];

        // Caller cannot recompute the stored algorithm → None → Unknown.
        let plan = plan_relink(p, &ids, &candidates, |_stored, _path| None);
        assert_eq!(plan.entries[0].hash, RelinkHashCheck::Unknown);
        assert_eq!(plan.entries[0].new_hash, None);

        // Same fixture, a caller that CAN hash in the stored algorithm: the
        // difference is now measured and reported.
        let plan = plan_relink(p, &ids, &candidates, |_stored, _path| {
            Some("siphash64:ffffffffffffffff".to_string())
        });
        assert_eq!(plan.entries[0].hash, RelinkHashCheck::Mismatch);
    }

    #[test]
    fn plan_relink_is_deterministic_and_flags_an_ambiguous_scan() {
        let (doc, ids) = relink_fixture(&[("/old/a.mp4", None)]);
        let p = doc.timeline.as_ref().unwrap();
        let forwards = [cand("/new/b/a.mp4", None), cand("/new/a/a.mp4", None)];
        let backwards = [cand("/new/a/a.mp4", None), cand("/new/b/a.mp4", None)];

        let one = plan_relink(p, &ids, &forwards, faked(&[]));
        let two = plan_relink(p, &ids, &backwards, faked(&[]));
        assert_eq!(one, two, "scan order must not change the plan");
        assert_eq!(one.entries[0].new_path, PathBuf::from("/new/a/a.mp4"));
        assert!(
            one.entries[0].ambiguous,
            "two files matched the same rule — the UI must be able to say so"
        );

        // One candidate → not ambiguous (the flag means something).
        let single = plan_relink(p, &ids, &forwards[..1], faked(&[]));
        assert!(!single.entries[0].ambiguous);
    }

    /// A byte change is refused by default, and accepting it re-identifies the
    /// asset (new hash, stale probe dropped) inside the same batch.
    #[test]
    fn relink_plan_commands_gate_a_byte_change_and_re_identify_on_accept() {
        let (mut doc, ids) = relink_fixture(&[("/old/a.mp4", Some("xxh:aaa"))]);
        doc.timeline
            .as_mut()
            .unwrap()
            .media
            .assets
            .get_mut(&ids[0])
            .unwrap()
            .probe = Some(MediaProbe {
            duration: Tick(1000),
            video: None,
            audio: None,
            container: "mov".into(),
            codec: "h264".into(),
            is_vfr: false,
            pixel_format: None,
            has_alpha: false,
        });
        let p = doc.timeline.as_ref().unwrap();
        let disk = [("/new/a.mp4", "xxh:zzz")];
        let plan = plan_relink(p, &ids, &[cand("/new/a.mp4", None)], faked(&disk));
        assert_eq!(plan.entries[0].hash, RelinkHashCheck::Mismatch);

        assert!(
            relink_plan_commands(p, &plan.entries, false).is_empty(),
            "a wrong-take relink must not be committed without consent"
        );

        let cmds = relink_plan_commands(p, &plan.entries, true);
        assert_eq!(
            cmds.len(),
            2,
            "RelinkAsset + the re-identifying SetAssetMeta"
        );
        Command::Batch(cmds.into_iter().map(Command::Timeline).collect()).apply(&mut doc);
        let a = &doc.timeline.as_ref().unwrap().media.assets[&ids[0]];
        assert!(matches!(
            &a.source,
            AssetSource::File { path, .. } if path == std::path::Path::new("/new/a.mp4")
        ));
        assert_eq!(a.content_hash.as_deref(), Some("xxh:zzz"));
        assert!(
            a.probe.is_none(),
            "the probe described the old bytes and must not survive a byte change"
        );
    }

    /// A hash-verified relink leaves probe and hash exactly as they were — the
    /// re-identification above is specific to an accepted byte change.
    #[test]
    fn a_verified_relink_touches_only_the_path() {
        let (mut doc, ids) = relink_fixture(&[("/old/a.mp4", Some("xxh:aaa"))]);
        let p = doc.timeline.as_ref().unwrap();
        let disk = [("/new/a.mp4", "xxh:aaa")];
        let plan = plan_relink(
            p,
            &ids,
            &[cand("/new/a.mp4", Some("xxh:aaa"))],
            faked(&disk),
        );
        assert_eq!(plan.entries[0].hash, RelinkHashCheck::Match);
        let cmds = relink_plan_commands(p, &plan.entries, false);
        assert_eq!(cmds.len(), 1, "no metadata edit for an unchanged identity");
        Command::Batch(cmds.into_iter().map(Command::Timeline).collect()).apply(&mut doc);
        assert_eq!(
            doc.timeline.as_ref().unwrap().media.assets[&ids[0]]
                .content_hash
                .as_deref(),
            Some("xxh:aaa")
        );
    }

    /// DoD 4: a folder move is ONE undo unit — every asset returns to its old
    /// path on a single inverse, not one undo per clip.
    #[test]
    fn a_whole_folder_relink_is_one_undo_unit() {
        let rows = [
            ("/old/a.mp4", Some("h:a")),
            ("/old/b.mp4", Some("h:b")),
            ("/old/c.mp4", Some("h:c")),
        ];
        let (mut doc, ids) = relink_fixture(&rows);
        let p = doc.timeline.as_ref().unwrap();
        let offline = offline_assets(p, |_| false);
        assert_eq!(offline.len(), 3, "fixture must actually be all-offline");

        let candidates: Vec<RelinkCandidate> = ["a", "b", "c"]
            .iter()
            .map(|n| cand(&format!("/new/{n}.mp4"), Some(&format!("h:{n}"))))
            .collect();
        let plan = plan_relink(p, &offline, &candidates, faked(&[]));
        assert_eq!(plan.entries.len(), 3);
        assert!(plan.unmatched.is_empty());

        let cmds = relink_plan_commands(p, &plan.entries, false);
        let batch = Command::Batch(cmds.into_iter().map(Command::Timeline).collect());
        let inverse = batch.inverse(&doc).expect("batch inverts");
        batch.apply(&mut doc);
        let paths = |d: &Document| -> Vec<String> {
            let mut v: Vec<String> = ids
                .iter()
                .map(
                    |id| match &d.timeline.as_ref().unwrap().media.assets[id].source {
                        AssetSource::File { path, .. } => path.display().to_string(),
                        _ => String::new(),
                    },
                )
                .collect();
            v.sort();
            v
        };
        assert_eq!(
            paths(&doc),
            vec![
                "/new/a.mp4".to_string(),
                "/new/b.mp4".into(),
                "/new/c.mp4".into()
            ]
        );
        inverse.apply(&mut doc);
        assert_eq!(
            paths(&doc),
            vec![
                "/old/a.mp4".to_string(),
                "/old/b.mp4".into(),
                "/old/c.mp4".into()
            ],
            "one undo restores every relinked asset"
        );
    }

    // ── K-B11 keyframe interchange ─────────────────────────────────────────

    #[test]
    fn copy_paste_keyframes_identity_and_offset() {
        let (mut doc, _seq, _track, clip, _asset) = scoped_fixture();
        let target = AnimTarget::ClipTransform { clip };
        // Seed two opacity keys.
        {
            let p = doc.timeline.as_ref().unwrap();
            let cmds = [
                set_keyframe(
                    p,
                    target.clone(),
                    PropPath::new("transform.opacity"),
                    Keyframe::new(
                        Tick(10),
                        crate::timeline::PropValue::Float(0.0),
                        Interp::Linear,
                    ),
                ),
                set_keyframe(
                    p,
                    target.clone(),
                    PropPath::new("transform.opacity"),
                    Keyframe::new(
                        Tick(50),
                        crate::timeline::PropValue::Float(1.0),
                        Interp::Linear,
                    ),
                ),
            ];
            for c in cmds {
                Command::Timeline(c).apply(&mut doc);
            }
        }
        let p = doc.timeline.as_ref().unwrap();
        let clip_board = copy_keyframes(p, &target, None).unwrap();
        assert_eq!(clip_board.tracks.len(), 1);
        assert_eq!(clip_board.anchor, Tick(10));
        assert!(!clip_board.is_empty());

        // Paste with +20 tick offset onto the same path → keys at 30 and 70.
        let paste = paste_keyframes(p, target.clone(), &clip_board, &[], Tick(20)).unwrap();
        assert_eq!(paste.len(), 2);
        for c in paste {
            Command::Timeline(c).apply(&mut doc);
        }
        let p = doc.timeline.as_ref().unwrap();
        let tracks = read_tracks(p, &target).unwrap();
        let opac = tracks
            .iter()
            .find(|t| t.property.as_str() == "transform.opacity")
            .unwrap();
        // Original 10,50 plus pasted 30,70.
        let times: Vec<i64> = opac.keyframes.iter().map(|k| k.at.0).collect();
        assert!(times.contains(&10));
        assert!(times.contains(&50));
        assert!(times.contains(&30));
        assert!(times.contains(&70));
    }

    #[test]
    fn paste_keyframes_path_mapping_and_reanchor() {
        let (mut doc, _seq, _track, clip, _asset) = scoped_fixture();
        let target = AnimTarget::ClipTransform { clip };
        {
            let p = doc.timeline.as_ref().unwrap();
            let cmd = set_keyframe(
                p,
                target.clone(),
                PropPath::new("transform.x"),
                Keyframe::new(
                    Tick(100),
                    crate::timeline::PropValue::Float(42.0),
                    Interp::Hold,
                ),
            );
            Command::Timeline(cmd).apply(&mut doc);
        }
        let p = doc.timeline.as_ref().unwrap();
        let board = copy_keyframes(p, &target, Some(&[PropPath::new("transform.x")])).unwrap();
        assert_eq!(board.anchor, Tick(100));

        // Map transform.x → transform.y, re-anchor so key lands at 0.
        let paste = paste_keyframes_reanchored(
            p,
            target.clone(),
            &board,
            &[(PropPath::new("transform.x"), PropPath::new("transform.y"))],
            Tick(0),
        )
        .unwrap();
        assert_eq!(paste.len(), 1);
        for c in paste {
            Command::Timeline(c).apply(&mut doc);
        }
        let p = doc.timeline.as_ref().unwrap();
        let tracks = read_tracks(p, &target).unwrap();
        let y = tracks
            .iter()
            .find(|t| t.property.as_str() == "transform.y")
            .expect("mapped track");
        assert_eq!(y.keyframes.len(), 1);
        assert_eq!(y.keyframes[0].at, Tick(0));
        assert_eq!(
            y.keyframes[0].value,
            crate::timeline::PropValue::Float(42.0)
        );
    }
}
