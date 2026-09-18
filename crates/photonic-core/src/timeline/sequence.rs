//! Sequences, tracks, and the top-level project container (01 §2, §4).

use super::audio::{MasterBus, TrackAudio};
use super::captions::CaptionTrack;
use super::clip::{Clip, ClipEffect};
use super::grade::Grade;
use super::graph::NodeGraph;
use super::ids::{
    ClipId, CueId, GraphId, GroupId, MarkerCategoryId, MarkerId, SequenceId, TagId, TrackId,
};
use super::media::{MediaPool, MediaTag};
use super::time::{FrameRate, Tick};
use crate::layer::BlendMode;
use crate::Color;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// The document-level timeline container (01 §2). `Document::timeline` is
/// `Option<TimelineProject>`; the first video-mode action creates it undoably.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TimelineProject {
    pub media: MediaPool,
    pub sequences: HashMap<SequenceId, Sequence>,
    /// UI ordering of sequences.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sequence_order: Vec<SequenceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_sequence: Option<SequenceId>,
    /// All user node graphs (per-clip + project), one arena (§8).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub graphs: HashMap<GraphId, NodeGraph>,
    /// Spliced after the active sequence output (02 §2 step 6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_graph: Option<GraphId>,
    #[serde(default)]
    pub settings: ProjectVideoSettings,
    /// Project-level marker categories (35 §1.3), referenced by stable
    /// [`MarkerCategoryId`], never by index. The vec order is display order and
    /// carries no semantics; ids are unique within the vec. Optional — a project
    /// carries none until the command layer seeds them on first use.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub marker_categories: Vec<MarkerCategory>,
    /// Project-level media-pool tag registry (26 K-C2). Same id-addressed
    /// taxonomy pattern as [`Self::marker_categories`]. Display order only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media_tags: Vec<MediaTag>,
}

impl TimelineProject {
    /// An empty project (no sequences yet).
    pub fn new() -> Self {
        TimelineProject {
            media: MediaPool::new(),
            sequences: HashMap::new(),
            sequence_order: Vec::new(),
            active_sequence: None,
            graphs: HashMap::new(),
            project_graph: None,
            settings: ProjectVideoSettings::default(),
            marker_categories: Vec::new(),
            media_tags: Vec::new(),
        }
    }

    /// Lookup a media tag by stable id.
    pub fn media_tag(&self, id: TagId) -> Option<&MediaTag> {
        self.media_tags.iter().find(|t| t.id == id)
    }

    /// Case-insensitive name match for registry upsert (K-C2).
    pub fn media_tag_by_name(&self, name: &str) -> Option<&MediaTag> {
        let needle = name.trim();
        if needle.is_empty() {
            return None;
        }
        self.media_tags
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(needle))
    }

    /// The category with this id, if present. Categories are addressed by
    /// stable id, never by index (35 §1.3); a marker pointing at a missing
    /// category renders neutral rather than being silently remapped.
    pub fn marker_category(&self, id: MarkerCategoryId) -> Option<&MarkerCategory> {
        self.marker_categories.iter().find(|c| c.id == id)
    }

    /// Display-order position of the category with this id. Used by the
    /// category commands so a delete/undo pair restores the original ordering;
    /// never used to *address* a category (35 §1.3).
    pub fn marker_category_index(&self, id: MarkerCategoryId) -> Option<usize> {
        self.marker_categories.iter().position(|c| c.id == id)
    }

    /// Every marker in the project that references `id`, in both scopes
    /// ([`Sequence::markers`] and [`Clip::markers`](super::clip::Clip)).
    /// The category-delete op uses this to build its retarget list, so no
    /// marker is left pointing at a category that is gone without the command
    /// recording the change (35 §1.3: never silently remapped).
    pub fn markers_in_category(&self, id: MarkerCategoryId) -> Vec<MarkerRef> {
        let mut out = Vec::new();
        // Deterministic order: sequences in UI order first, then any sequence
        // not in `sequence_order` (a load-time possibility) by id.
        let mut seq_ids: Vec<SequenceId> = self.sequence_order.clone();
        let mut rest: Vec<SequenceId> = self
            .sequences
            .keys()
            .copied()
            .filter(|s| !seq_ids.contains(s))
            .collect();
        rest.sort();
        seq_ids.append(&mut rest);
        for sid in seq_ids {
            let Some(s) = self.sequences.get(&sid) else {
                continue;
            };
            for m in &s.markers {
                if m.category == Some(id) {
                    out.push(MarkerRef::Sequence {
                        seq: sid,
                        marker: m.id,
                    });
                }
            }
            for t in s.tracks() {
                for c in &t.clips {
                    for m in &c.markers {
                        if m.category == Some(id) {
                            out.push(MarkerRef::Clip {
                                clip: c.id,
                                marker: m.id,
                            });
                        }
                    }
                }
            }
        }
        out
    }

    /// Insert a sequence, appending it to the UI order and making it active if
    /// none was.
    pub fn insert_sequence(&mut self, seq: Sequence) -> SequenceId {
        let id = seq.id;
        self.sequences.insert(id, seq);
        if !self.sequence_order.contains(&id) {
            self.sequence_order.push(id);
        }
        if self.active_sequence.is_none() {
            self.active_sequence = Some(id);
        }
        id
    }
}

impl Default for TimelineProject {
    fn default() -> Self {
        TimelineProject::new()
    }
}

/// Project-wide video settings (proxy prefs, cache limits, default rates).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectVideoSettings {
    /// Whether proxy media is generated on import.
    #[serde(default)]
    pub generate_proxies: bool,
    /// Sidecar cache budget in megabytes; `None` = unbounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_limit_mb: Option<u64>,
    /// Default frame rate for a new sequence.
    #[serde(default = "default_frame_rate")]
    pub default_frame_rate: FrameRate,
    /// Export/mix audio sample rate (Hz).
    #[serde(default = "default_audio_rate")]
    pub audio_sample_rate: u32,
}

fn default_frame_rate() -> FrameRate {
    FrameRate::FPS_30
}
fn default_audio_rate() -> u32 {
    48_000
}

impl Default for ProjectVideoSettings {
    fn default() -> Self {
        ProjectVideoSettings {
            // On by default: 4K60 high-bitrate phone/drone clips (DJI, etc.)
            // cannot real-time software-decode, so Draft/playback needs the
            // half-res all-intra proxy. Users can still turn this off per
            // project in the media-pool settings.
            generate_proxies: true,
            cache_limit_mb: None,
            default_frame_rate: default_frame_rate(),
            audio_sample_rate: default_audio_rate(),
        }
    }
}

/// User-declared region for cached playback. Rendered chunks remain sidecar
/// cache state and never enter document history.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewZone {
    pub start: Tick,
    pub end: Tick,
}

/// A sequence: a stack of tracks with a frame rate and one or more aspect formats.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sequence {
    pub id: SequenceId,
    pub name: String,
    pub frame_rate: FrameRate,
    /// Multi-aspect (CAP-012): ≥1 entries.
    pub formats: Vec<SequenceFormat>,
    #[serde(default)]
    pub active_format: usize,
    /// Index 0 = bottom of the composite stack.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub video_tracks: Vec<Track>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audio_tracks: Vec<Track>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub caption_tracks: Vec<CaptionTrack>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<Marker>,
    /// Group tree (35 §3): a parent-pointer forest of [`GroupNode`]s, keyed by
    /// [`GroupId`]. Clips point at their immediate group via
    /// [`Clip::group`](super::clip::Clip).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub groups: HashMap<GroupId, GroupNode>,
    #[serde(default)]
    pub audio_master: MasterBus,
    /// Master effect stack (35 §2): applied to the composited accumulator after
    /// the Adjustment stack and before the caption overlay. Empty = neutral, so
    /// a v4 sequence (no key) composites identically (§2.6).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub master_effects: Vec<ClipEffect>,
    /// Master grade (35 §2): applied to the accumulator after `master_effects`.
    /// `None` = neutral.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub master_grade: Option<Grade>,
    /// In/out for preview + export.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_range: Option<(Tick, Tick)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preview_zones: Vec<PreviewZone>,
    /// Sequence start timecode offset (K-A12). Display labels are
    /// `start_timecode + playhead`. Default zero (legacy files). Deliveries
    /// often use `01:00:00:00` (`frame_rate.frame_start(nominal_fps * 3600)`).
    #[serde(default, skip_serializing_if = "is_tick_zero")]
    pub start_timecode: Tick,
}

fn is_tick_zero(t: &Tick) -> bool {
    t.0 == 0
}

impl Sequence {
    /// A new sequence with one format and empty tracks.
    pub fn new(name: impl Into<String>, frame_rate: FrameRate, width: u32, height: u32) -> Self {
        Sequence {
            id: SequenceId::new(),
            name: name.into(),
            frame_rate,
            formats: vec![SequenceFormat::new("16:9", width, height)],
            active_format: 0,
            video_tracks: Vec::new(),
            audio_tracks: Vec::new(),
            caption_tracks: Vec::new(),
            markers: Vec::new(),
            groups: HashMap::new(),
            audio_master: MasterBus::new(),
            master_effects: Vec::new(),
            master_grade: None,
            work_range: None,
            preview_zones: Vec::new(),
            start_timecode: Tick::ZERO,
        }
    }

    /// The active format (clamped to a valid index).
    pub fn format(&self) -> &SequenceFormat {
        let i = self.active_format.min(self.formats.len().saturating_sub(1));
        &self.formats[i]
    }

    /// All video + audio tracks (captions handled separately).
    pub fn tracks(&self) -> impl Iterator<Item = &Track> {
        self.video_tracks.iter().chain(self.audio_tracks.iter())
    }

    /// The track vec a `kind` lives in. Video and Text share `video_tracks`
    /// (both are part of the visual composite); Audio uses `audio_tracks`.
    pub fn tracks_for(&self, kind: TrackKind) -> &Vec<Track> {
        match kind {
            TrackKind::Audio => &self.audio_tracks,
            TrackKind::Video | TrackKind::Text => &self.video_tracks,
        }
    }

    /// Mutable counterpart of [`tracks_for`](Self::tracks_for).
    pub fn tracks_for_mut(&mut self, kind: TrackKind) -> &mut Vec<Track> {
        match kind {
            TrackKind::Audio => &mut self.audio_tracks,
            TrackKind::Video | TrackKind::Text => &mut self.video_tracks,
        }
    }

    /// Find a track by id across video and audio lanes.
    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.tracks().find(|t| t.id == id)
    }

    pub fn track_mut(&mut self, id: TrackId) -> Option<&mut Track> {
        self.video_tracks
            .iter_mut()
            .chain(self.audio_tracks.iter_mut())
            .find(|t| t.id == id)
    }

    /// A structural copy of this sequence with brand-new identity — a fresh
    /// [`SequenceId`] plus fresh ids for every track, clip, marker, caption
    /// track and cue — so the duplicate addresses cleanly alongside the
    /// original (the command layer resolves clips/tracks by id across *all*
    /// sequences and would otherwise hit the first match). Link groups are
    /// remapped to fresh ids so a linked pair stays linked *within* the copy
    /// without tying it back to the original. Referenced assets, composition
    /// graphs and nested sequences are shared (their ids are left untouched).
    /// The caller renames the copy (see `ops::duplicate_sequence`).
    pub fn duplicate_with_fresh_ids(&self) -> Sequence {
        use super::clip::LinkGroupId;
        let mut dup = self.clone();
        dup.id = SequenceId::new();
        let mut link_remap: HashMap<LinkGroupId, LinkGroupId> = HashMap::new();
        // Groups are remapped to fresh ids for the same reason link groups are:
        // so a grouped set stays grouped *within* the copy without tying it back
        // to the original (ids resolve across sequences).
        let group_remap: HashMap<GroupId, GroupId> = dup
            .groups
            .keys()
            .map(|old| (*old, GroupId::new()))
            .collect();
        for track in dup
            .video_tracks
            .iter_mut()
            .chain(dup.audio_tracks.iter_mut())
        {
            track.id = TrackId::new();
            for clip in &mut track.clips {
                clip.id = ClipId::new();
                if let Some(old) = clip.link_group {
                    let fresh = *link_remap.entry(old).or_default();
                    clip.link_group = Some(fresh);
                }
                if let Some(old) = clip.group {
                    clip.group = group_remap.get(&old).copied();
                }
                for m in &mut clip.markers {
                    m.id = MarkerId::new();
                }
            }
        }
        if !group_remap.is_empty() {
            dup.groups = dup
                .groups
                .drain()
                .map(|(old, mut node)| {
                    let fresh = group_remap[&old];
                    node.id = fresh;
                    node.parent = node.parent.and_then(|p| group_remap.get(&p).copied());
                    (fresh, node)
                })
                .collect();
        }
        for marker in &mut dup.markers {
            marker.id = MarkerId::new();
        }
        for ct in &mut dup.caption_tracks {
            ct.id = TrackId::new();
            for cue in &mut ct.cues {
                cue.id = CueId::new();
            }
        }
        dup
    }

    /// The end tick of the last clip across all tracks (sequence content length).
    pub fn content_end(&self) -> Tick {
        self.tracks()
            .flat_map(|t| t.clips.iter())
            .map(|c| c.end())
            .max()
            .unwrap_or(Tick::ZERO)
    }

    /// The parent chain of a group, leaf→root, cycle-guarded (35 §3). A cycle or
    /// a dangling `parent` stops the walk (the returned prefix is still valid);
    /// [`validate`](Self::validate) is the authority that rejects such states.
    pub fn group_chain(&self, g: GroupId) -> Vec<GroupId> {
        let mut chain = Vec::new();
        let mut visited = std::collections::HashSet::new();
        let mut cur = Some(g);
        while let Some(id) = cur {
            if !visited.insert(id) {
                break; // cycle
            }
            let Some(node) = self.groups.get(&id) else {
                break; // dangling parent ref
            };
            chain.push(id);
            cur = node.parent;
        }
        chain
    }

    /// The root of a group's parent chain (35 §3), or `None` if `g` is unknown.
    pub fn topmost_group(&self, g: GroupId) -> Option<GroupId> {
        self.group_chain(g).last().copied()
    }

    /// Every clip in this sequence whose group chain contains `g` — the
    /// transitive members (35 §3).
    pub fn group_members(&self, g: GroupId) -> Vec<(TrackId, ClipId)> {
        let mut out = Vec::new();
        for track in self.tracks() {
            for clip in &track.clips {
                if let Some(cg) = clip.group {
                    if self.group_chain(cg).contains(&g) {
                        out.push((track.id, clip.id));
                    }
                }
            }
        }
        out
    }

    /// Clips whose *immediate* group is `g` (35 §3).
    pub fn direct_group_members(&self, g: GroupId) -> Vec<(TrackId, ClipId)> {
        let mut out = Vec::new();
        for track in self.tracks() {
            for clip in &track.clips {
                if clip.group == Some(g) {
                    out.push((track.id, clip.id));
                }
            }
        }
        out
    }

    /// Groups whose immediate `parent` is `g` (35 §3).
    pub fn child_groups(&self, g: GroupId) -> Vec<GroupId> {
        self.groups
            .values()
            .filter(|n| n.parent == Some(g))
            .map(|n| n.id)
            .collect()
    }

    /// Enforce the per-track invariants (01 §4): every clip has `duration > 0`,
    /// clips are sorted by `start`, and no two clips on one track overlap.
    /// Also enforces the group-tree invariants (35 §3): no cycles, referenced
    /// group ids exist in-sequence, no empty groups, and no single-member
    /// `Normal` groups. Debug-asserted after every edit op and checked on load.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.formats.is_empty() {
            return Err(ValidationError::NoFormats(self.id));
        }
        for track in self.tracks() {
            let mut prev_end: Option<Tick> = None;
            for clip in &track.clips {
                if clip.duration.0 <= 0 {
                    return Err(ValidationError::NonPositiveDuration {
                        track: track.id,
                        clip_start: clip.start,
                    });
                }
                if let Some(pe) = prev_end {
                    if clip.start < pe {
                        return Err(ValidationError::OverlapOrUnsorted {
                            track: track.id,
                            at: clip.start,
                        });
                    }
                }
                prev_end = Some(clip.end());
            }
        }
        self.validate_transitions()?;
        self.validate_groups()?;
        Ok(())
    }

    /// The transition pass of [`validate`](Self::validate) (01 §5): a
    /// `transition_out` is a fade to transparent into a gap/end. At a hard cut —
    /// the next clip on the track is adjacent (`next.start == clip.end()`, no
    /// gap) — the incoming clip's `transition_in` owns the blend, so an outgoing
    /// transition there is invalid. A `transition_out` on the last clip, or one
    /// whose next clip leaves a gap, fades into a gap/end and is accepted. Assumes
    /// clips are already sorted (the caller enforces that above).
    fn validate_transitions(&self) -> Result<(), ValidationError> {
        for track in self.tracks() {
            for (i, clip) in track.clips.iter().enumerate() {
                if clip.transition_out.is_some() {
                    if let Some(next) = track.clips.get(i + 1) {
                        if next.start == clip.end() {
                            return Err(ValidationError::TransitionOutAtCut {
                                track: track.id,
                                clip_start: clip.start,
                            });
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// The group-tree pass of [`validate`](Self::validate) (35 §3).
    fn validate_groups(&self) -> Result<(), ValidationError> {
        // Every `Clip.group` names a group that exists in-sequence.
        for track in self.tracks() {
            for clip in &track.clips {
                if let Some(g) = clip.group {
                    if !self.groups.contains_key(&g) {
                        return Err(ValidationError::UnknownGroup { group: g });
                    }
                }
            }
        }
        for node in self.groups.values() {
            // A named parent must exist in-sequence.
            if let Some(p) = node.parent {
                if !self.groups.contains_key(&p) {
                    return Err(ValidationError::UnknownGroup { group: p });
                }
            }
            // The parent chain must terminate (no cycle). Walk with a visited
            // set; a repeat means the chain from `node` re-enters itself.
            let mut visited = std::collections::HashSet::new();
            let mut cur = Some(node.id);
            while let Some(id) = cur {
                if !visited.insert(id) {
                    return Err(ValidationError::GroupCycle { group: node.id });
                }
                cur = self.groups.get(&id).and_then(|n| n.parent);
            }
        }
        for node in self.groups.values() {
            let direct = self.direct_group_members(node.id).len();
            let children = self.child_groups(node.id).len();
            // No empty groups: zero direct clip members and zero child groups.
            if direct == 0 && children == 0 {
                return Err(ValidationError::EmptyGroup { group: node.id });
            }
            // No single-member `Normal` group (a group of one is just a clip).
            // `AvLink`/`Unknown` are exempt: an A/V pair mid-edit may transiently
            // hold one member, and an unknown kind must never be dissolved.
            if node.kind == GroupKind::Normal && self.group_members(node.id).len() == 1 {
                return Err(ValidationError::SingletonNormalGroup { group: node.id });
            }
        }
        Ok(())
    }

    /// Remove degenerate groups after a membership change (35 §3) — the ops-layer
    /// counterpart to [`validate`](Self::validate)'s rejection. Removes empty
    /// groups and singleton `Normal` groups bottom-up, reparenting orphaned
    /// children to the removed group's parent, and returns the dissolved ids (so
    /// a command can capture them for its inverse). `AvLink` and `Unknown` groups
    /// are never dissolved.
    pub fn dissolve_degenerate_groups(&mut self) -> Vec<GroupId> {
        let mut dissolved = Vec::new();
        loop {
            // Find one degenerate group to remove this pass. Working one at a
            // time keeps membership/child counts consistent as we reparent.
            let victim = self.groups.values().find_map(|node| {
                if node.kind == GroupKind::AvLink || node.kind == GroupKind::Unknown {
                    return None;
                }
                let direct = self.direct_group_members(node.id).len();
                let children = self.child_groups(node.id).len();
                let empty = direct == 0 && children == 0;
                let singleton =
                    node.kind == GroupKind::Normal && self.group_members(node.id).len() == 1;
                if empty || singleton {
                    Some(node.id)
                } else {
                    None
                }
            });
            let Some(victim) = victim else { break };
            let parent = self.groups.get(&victim).and_then(|n| n.parent);
            // Reparent orphaned child groups and direct clip members to the
            // removed group's parent.
            let child_ids = self.child_groups(victim);
            for cid in child_ids {
                if let Some(child) = self.groups.get_mut(&cid) {
                    child.parent = parent;
                }
            }
            for track in self
                .video_tracks
                .iter_mut()
                .chain(self.audio_tracks.iter_mut())
            {
                for clip in &mut track.clips {
                    if clip.group == Some(victim) {
                        clip.group = parent;
                    }
                }
            }
            self.groups.remove(&victim);
            dissolved.push(victim);
        }
        dissolved
    }
}

/// A validation failure for a sequence's track invariants.
#[derive(Debug, Clone, PartialEq)]
pub enum ValidationError {
    NoFormats(SequenceId),
    NonPositiveDuration {
        track: TrackId,
        clip_start: Tick,
    },
    OverlapOrUnsorted {
        track: TrackId,
        at: Tick,
    },
    /// A group's parent chain does not terminate (35 §3).
    GroupCycle {
        group: GroupId,
    },
    /// A `Clip.group` or `GroupNode.parent` names a group absent from the
    /// sequence (35 §3).
    UnknownGroup {
        group: GroupId,
    },
    /// A group with zero direct clip members and zero child groups (35 §3).
    EmptyGroup {
        group: GroupId,
    },
    /// A `GroupKind::Normal` group with exactly one transitive member (35 §3).
    SingletonNormalGroup {
        group: GroupId,
    },
    /// A clip carries a `transition_out` while its outgoing edge is a hard cut —
    /// the next clip on the track is adjacent, no gap (01 §5). At a cut the
    /// incoming clip's `transition_in` owns the blend, so an outgoing fade is
    /// invalid. `clip_start` names the offending clip's timeline start.
    TransitionOutAtCut {
        track: TrackId,
        clip_start: Tick,
    },
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::NoFormats(s) => write!(f, "sequence {s} has no formats"),
            ValidationError::NonPositiveDuration { track, clip_start } => {
                write!(
                    f,
                    "clip at {clip_start:?} on track {track} has duration <= 0"
                )
            }
            ValidationError::OverlapOrUnsorted { track, at } => {
                write!(f, "clip at {at:?} on track {track} overlaps or is unsorted")
            }
            ValidationError::GroupCycle { group } => {
                write!(f, "group {group} parent chain does not terminate")
            }
            ValidationError::UnknownGroup { group } => {
                write!(f, "reference to unknown group {group}")
            }
            ValidationError::EmptyGroup { group } => write!(f, "group {group} is empty"),
            ValidationError::SingletonNormalGroup { group } => {
                write!(f, "normal group {group} has a single member")
            }
            ValidationError::TransitionOutAtCut { track, clip_start } => {
                write!(
                    f,
                    "clip at {clip_start:?} on track {track} has a transition_out at a hard cut"
                )
            }
        }
    }
}

impl std::error::Error for ValidationError {}

/// One aspect-ratio variant of a sequence (CAP-012).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SequenceFormat {
    pub name: String,
    pub width: u32,
    pub height: u32,
}

impl SequenceFormat {
    pub fn new(name: impl Into<String>, width: u32, height: u32) -> Self {
        SequenceFormat {
            name: name.into(),
            width,
            height,
        }
    }
}

/// A video or audio track. Clip vectors are the z/mix order — no separate order
/// table (01 §4).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub name: String,
    pub kind: TrackKind,
    /// Sorted by start, non-overlapping (invariant, enforced by edit ops).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clips: Vec<Clip>,
    /// Video: hidden; audio: muted.
    #[serde(default = "super::grade::default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub locked: bool,
    /// Sync lock (14 §M-9): when set, ripple/insert edits are meant to shift
    /// this track in lock-step with the other sync-locked tracks. This is the
    /// data + toggle half only; the ripple-propagation wiring across sync-locked
    /// tracks is a later GUI concern.
    #[serde(default)]
    pub sync_lock: bool,
    /// volume/pan/solo, fx chain, automation (09).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<TrackAudio>,
    /// UI-only but persisted (like existing panel prefs).
    #[serde(default = "default_track_height")]
    pub height_px: f32,
    /// Track-level effect stack (35 §2): applied to this track's own composited
    /// content before it merges into the accumulator — never to the accumulator
    /// itself. Empty = neutral (§2.6).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<ClipEffect>,
    /// Track-level grade (35 §2): applied to this track's own content after
    /// `effects`. `None` = neutral.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grade: Option<Grade>,
    /// How this track composites into the accumulator (35 §2; closes 26 K-A9).
    /// Defaults to [`BlendMode::Normal`] — the identity merge.
    #[serde(default)]
    pub blend: BlendMode,
    /// Track composite opacity in `[0, 1]` (35 §2; closes 26 K-A9). 1.0 = opaque
    /// (the identity for `Merge`).
    #[serde(default = "default_track_opacity")]
    pub opacity: f32,
}

fn default_track_height() -> f32 {
    64.0
}

fn default_track_opacity() -> f32 {
    1.0
}

impl Track {
    pub fn new(kind: TrackKind, name: impl Into<String>) -> Self {
        Track {
            id: TrackId::new(),
            name: name.into(),
            kind,
            clips: Vec::new(),
            enabled: true,
            locked: false,
            sync_lock: false,
            audio: if kind == TrackKind::Audio {
                Some(TrackAudio::new())
            } else {
                None
            },
            height_px: default_track_height(),
            effects: Vec::new(),
            grade: None,
            blend: BlendMode::default(),
            opacity: default_track_opacity(),
        }
    }

    /// Insertion index that keeps `clips` sorted by `start`.
    pub fn insert_index_for(&self, start: Tick) -> usize {
        self.clips.partition_point(|c| c.start < start)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    Video,
    Audio,
    /// A title/graphics layer. Composited through the same stack as video
    /// (stored in `video_tracks`), but only holds `ClipSource::Text` clips — a
    /// dedicated home for titles above the footage.
    Text,
}

impl TrackKind {
    /// Whether this kind is part of the visual composite (video + text tracks,
    /// both stored in `Sequence::video_tracks`).
    pub fn is_visual(self) -> bool {
        matches!(self, TrackKind::Video | TrackKind::Text)
    }
}

/// A project-level marker category (35 §1.3): a named, coloured, glyphed class
/// a [`Marker`] may reference by stable [`MarkerCategoryId`]. Categories are
/// optional and live on [`TimelineProject::marker_categories`]; a marker's own
/// `color` (when set) overrides the category colour.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MarkerCategory {
    pub id: MarkerCategoryId,
    pub name: String,
    pub color: Color,
    #[serde(default)]
    pub glyph: MarkerGlyph,
}

impl MarkerCategory {
    /// A category with a fresh id, the given name and colour, and the default
    /// [`MarkerGlyph::Diamond`].
    pub fn new(name: impl Into<String>, color: Color) -> Self {
        MarkerCategory {
            id: MarkerCategoryId::new(),
            name: name.into(),
            color,
            glyph: MarkerGlyph::default(),
        }
    }

    /// Canonical name for the bookmarks category (proposal 210). Not a second
    /// storage type — bookmarks *are* sequence markers in this category.
    pub const BOOKMARKS_CATEGORY_NAME: &'static str = "Bookmarks";

    /// A small ordered seed set of categories for a project's first use. Called
    /// lazily by the command layer, never by [`TimelineProject::new`] — §1.3
    /// categories are optional. Each entry has a fresh, distinct id and a
    /// distinct glyph.
    pub fn default_seed() -> Vec<MarkerCategory> {
        vec![
            MarkerCategory {
                id: MarkerCategoryId::new(),
                name: "Marker".to_string(),
                color: Color::rgb(0.90, 0.80, 0.10),
                glyph: MarkerGlyph::Diamond,
            },
            MarkerCategory {
                id: MarkerCategoryId::new(),
                name: "Cut".to_string(),
                color: Color::rgb(0.85, 0.20, 0.20),
                glyph: MarkerGlyph::Bar,
            },
            MarkerCategory {
                id: MarkerCategoryId::new(),
                name: "Note".to_string(),
                color: Color::rgb(0.20, 0.55, 0.90),
                glyph: MarkerGlyph::Circle,
            },
            MarkerCategory {
                id: MarkerCategoryId::new(),
                name: "Todo".to_string(),
                color: Color::rgb(0.95, 0.55, 0.10),
                glyph: MarkerGlyph::Flag,
            },
            MarkerCategory {
                id: MarkerCategoryId::new(),
                name: "Chapter".to_string(),
                color: Color::rgb(0.35, 0.75, 0.35),
                glyph: MarkerGlyph::Triangle,
            },
            // Proposal 210: CapCut-class "bookmark" verb = markers in this
            // category (not a parallel type — 35 §1).
            MarkerCategory {
                id: MarkerCategoryId::new(),
                name: Self::BOOKMARKS_CATEGORY_NAME.to_string(),
                color: Color::rgb(0.70, 0.45, 0.95),
                glyph: MarkerGlyph::Flag,
            },
        ]
    }
}

/// The rendered glyph of a marker category (41 §7; ships with this migration).
/// `Diamond` is the default because the ruler already paints diamonds.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkerGlyph {
    #[default]
    Diamond,
    Circle,
    Square,
    Triangle,
    Flag,
    Bar,
}

/// How a [`Marker`] is anchored (35 §1). `Timecode` markers sit at an absolute
/// sequence tick; `Content` markers are relative to their owning clip. Clip
/// markers are always `Content`; sequence markers default `Timecode`.
///
/// `Unknown` is the 39 §2.2 forward-compat arm: a v5 file written by a later
/// build may carry an anchor token this build does not know. It must exist from
/// birth or such a file would strand this loader. Every consumer treats
/// `Unknown` as `Timecode` (inert), and it is never dropped. `#[serde(other)]`
/// loses the original token — the smallest correct interpretation for a
/// unit-only enum, matching the historical `GradeOpKind` precedent; 39 §2.2's
/// verbatim-preservation task owns upgrading it to a payload-carrying form.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MarkerAnchor {
    #[default]
    Timecode,
    Content,
    #[serde(other)]
    Unknown,
}

/// A marker. Scope is implied by location: [`Sequence::markers`] holds sequence
/// markers, [`Clip::markers`](super::clip::Clip) holds clip markers. `duration`
/// of 0 is a point marker; `> 0` a ranged marker. `color`, when set, overrides
/// the category colour. `category` is referenced by stable id (35 §1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    pub id: MarkerId,
    pub at: Tick,
    /// 0 = point marker; `> 0` = ranged. Never `Option` (35 §1).
    #[serde(default)]
    pub duration: Tick,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// A [`MarkerCategory`] referenced by stable id, never index. A missing
    /// category renders neutral + flagged, never silently remapped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<MarkerCategoryId>,
    /// Per-marker override of the category colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<Color>,
    #[serde(default)]
    pub anchor: MarkerAnchor,
}

impl Marker {
    /// A point sequence marker (anchor [`MarkerAnchor::Timecode`], zero
    /// duration).
    pub fn new(at: Tick, name: impl Into<String>) -> Self {
        Marker {
            id: MarkerId::new(),
            at,
            duration: Tick::ZERO,
            name: name.into(),
            note: String::new(),
            category: None,
            color: None,
            anchor: MarkerAnchor::Timecode,
        }
    }

    /// A clip-scoped marker (anchor [`MarkerAnchor::Content`]). Clip markers are
    /// always `Content` (§1.5); `at` is clip-relative.
    pub fn clip_scoped(at: Tick, name: impl Into<String>) -> Self {
        Marker {
            anchor: MarkerAnchor::Content,
            ..Marker::new(at, name)
        }
    }

    /// The exclusive end tick (`at + duration`). A point marker's end equals its
    /// start. Every consumer writes `[m.at, m.end()]` — no point/range branch.
    #[inline]
    pub fn end(&self) -> Tick {
        self.at + self.duration
    }

    /// Whether this is a ranged marker (`duration > 0`).
    #[inline]
    pub fn is_range(&self) -> bool {
        self.duration.0 > 0
    }
}

/// A pointer to one marker in either scope (35 §1.5) — sequence markers live on
/// [`Sequence::markers`], clip markers on [`Clip::markers`](super::clip::Clip).
///
/// Exists so a marker-category command can name the markers it retargets
/// without caring which scope they are in: deleting a category must be able to
/// reassign *every* referencing marker inside one undo unit, and clip markers
/// are not reachable from a `(SequenceId, MarkerId)` pair.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "scope")]
pub enum MarkerRef {
    Sequence { seq: SequenceId, marker: MarkerId },
    Clip { clip: ClipId, marker: MarkerId },
}

impl MarkerRef {
    /// The marker's own id, whatever the scope.
    pub fn marker_id(self) -> MarkerId {
        match self {
            MarkerRef::Sequence { marker, .. } | MarkerRef::Clip { clip: _, marker } => marker,
        }
    }
}

/// One marker's category reassignment, recorded on a category command so the
/// command is exactly invertible: `apply` writes `new`, and the inverse command
/// carries the same entry with `old`/`new` swapped.
///
/// This is what makes 35 §1.3's "a missing category renders neutral and is
/// flagged, never silently remapped" rule enforceable at the *command* layer: a
/// delete either leaves the reference dangling (and the UI flags it) or
/// retargets it explicitly and reversibly. It never happens implicitly.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MarkerRetarget {
    pub marker: MarkerRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old: Option<MarkerCategoryId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new: Option<MarkerCategoryId>,
}

impl MarkerRetarget {
    /// The same retarget, undone: `old` and `new` swapped.
    pub fn flipped(&self) -> MarkerRetarget {
        MarkerRetarget {
            marker: self.marker,
            old: self.new,
            new: self.old,
        }
    }
}

/// A node in a sequence's group tree (35 §3). Groups form a parent-pointer
/// forest: each node names its immediate `parent` (or `None` at the root of a
/// group chain), and clips point at their immediate group via
/// [`Clip::group`](super::clip::Clip). Membership and topmost-group queries are
/// pure walks over `parent` — no project-wide scan.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GroupNode {
    pub id: GroupId,
    pub kind: GroupKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<GroupId>,
}

impl GroupNode {
    /// A root group of the given kind (no parent).
    pub fn new(kind: GroupKind) -> Self {
        GroupNode {
            id: GroupId::new(),
            kind,
            parent: None,
        }
    }
}

/// The kind of a [`GroupNode`] (35 §3). `AvLink` subsumes the deprecated
/// `link_group` (an A/V pair split from one import); `Normal` is a plain
/// editor group. Trim propagates only for `AvLink`.
///
/// `Unknown` is the 39 §2.2 forward-compat arm: treated as `Normal` for every
/// behaviour except that it must never be auto-dissolved (dissolving would
/// destroy data a newer build owns). It must exist from birth. `#[serde(other)]`
/// loses the original token — the smallest correct unit-only interpretation, per
/// the brief; 39 §2.2's verbatim-preservation task owns the payload form.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum GroupKind {
    #[default]
    Normal,
    AvLink,
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::super::clip::{Clip, ClipSource};
    use super::*;

    fn vid_track_with(clips: Vec<(i64, i64)>) -> Track {
        let mut t = Track::new(TrackKind::Video, "V1");
        for (start, dur) in clips {
            t.clips
                .push(Clip::new(ClipSource::Adjustment, Tick(start), Tick(dur)));
        }
        t
    }

    fn seq_with(track: Track) -> Sequence {
        let mut s = Sequence::new("seq", FrameRate::FPS_30, 1920, 1080);
        s.video_tracks.push(track);
        s
    }

    #[test]
    fn validate_accepts_sorted_non_overlapping() {
        let s = seq_with(vid_track_with(vec![(0, 100), (100, 50), (200, 10)]));
        assert!(s.validate().is_ok());
    }

    #[test]
    fn text_track_lives_in_video_lane_and_round_trips() {
        let mut s = Sequence::new("seq", FrameRate::FPS_30, 1920, 1080);
        s.video_tracks.push(Track::new(TrackKind::Video, "V1"));
        let text = Track::new(TrackKind::Text, "T1");
        let text_id = text.id;
        // A Text track is routed into the video (visual composite) lane.
        s.tracks_for_mut(TrackKind::Text).push(text);
        assert_eq!(s.video_tracks.len(), 2);
        assert!(s.tracks_for(TrackKind::Text)[1].kind == TrackKind::Text);
        assert!(TrackKind::Text.is_visual());

        // Serde round-trip preserves the kind (snake_case "text").
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"text\""));
        let back: Sequence = serde_json::from_str(&json).unwrap();
        assert_eq!(back.track(text_id).map(|t| t.kind), Some(TrackKind::Text));
    }

    #[test]
    fn validate_rejects_overlap() {
        let s = seq_with(vid_track_with(vec![(0, 100), (50, 100)]));
        assert!(matches!(
            s.validate(),
            Err(ValidationError::OverlapOrUnsorted { .. })
        ));
    }

    #[test]
    fn validate_rejects_zero_duration() {
        let s = seq_with(vid_track_with(vec![(0, 0)]));
        assert!(matches!(
            s.validate(),
            Err(ValidationError::NonPositiveDuration { .. })
        ));
    }

    #[test]
    fn validate_rejects_transition_out_at_a_cut() {
        use super::super::clip::{Transition, TransitionKind};
        // [0,100) then an adjacent [100,50) — a hard cut. The first clip's
        // transition_out has no gap to fade into, so it must be rejected.
        let mut t = vid_track_with(vec![(0, 100), (100, 50)]);
        t.clips[0].transition_out = Some(Transition::new(TransitionKind::CrossDissolve, Tick(10)));
        let s = seq_with(t);
        assert!(matches!(
            s.validate(),
            Err(ValidationError::TransitionOutAtCut { .. })
        ));
    }

    #[test]
    fn validate_accepts_transition_out_into_a_gap() {
        use super::super::clip::{Transition, TransitionKind};
        // [0,100) then [150,50) — a 50-tick gap after the first clip, so its
        // transition_out fades into the gap and is valid.
        let mut t = vid_track_with(vec![(0, 100), (150, 50)]);
        t.clips[0].transition_out = Some(Transition::new(TransitionKind::CrossDissolve, Tick(10)));
        let s = seq_with(t);
        assert!(s.validate().is_ok());
    }

    #[test]
    fn validate_accepts_transition_out_on_last_clip() {
        use super::super::clip::{Transition, TransitionKind};
        // The only (hence last) clip fades into the end of the timeline — valid.
        let mut t = vid_track_with(vec![(0, 100)]);
        t.clips[0].transition_out = Some(Transition::new(TransitionKind::CrossDissolve, Tick(10)));
        let s = seq_with(t);
        assert!(s.validate().is_ok());
    }

    #[test]
    fn content_end_is_max_clip_end() {
        let s = seq_with(vid_track_with(vec![(0, 100), (100, 50)]));
        assert_eq!(s.content_end(), Tick(150));
    }

    // ── Effect scopes (35 §2) ────────────────────────────────────────────

    #[test]
    fn track_new_scope_fields_default_to_neutral_and_are_absent_from_json() {
        // Additive discipline (§2.6): the new scope fields default to the
        // identity composite and are omitted from JSON when neutral, so a v4
        // track stays byte-shape-identical.
        let t = Track::new(TrackKind::Video, "V1");
        assert!(t.effects.is_empty());
        assert!(t.grade.is_none());
        assert_eq!(t.blend, BlendMode::Normal);
        assert_eq!(t.opacity, 1.0);
        // The empty stacks are omitted (skip_serializing_if). `blend`/`opacity`
        // are always-present live fields (26 K-A9), so they are NOT skipped —
        // only their values must be neutral, asserted above.
        let json = serde_json::to_string(&t).unwrap();
        assert!(!json.contains("effects"));
        assert!(!json.contains("grade"));
        // The Sequence master scope defaults are likewise absent.
        let s = Sequence::new("seq", FrameRate::FPS_30, 1920, 1080);
        assert!(s.master_effects.is_empty());
        assert!(s.master_grade.is_none());
        let sjson = serde_json::to_string(&s).unwrap();
        assert!(!sjson.contains("master_effects"));
        assert!(!sjson.contains("master_grade"));
    }

    #[test]
    fn v4_track_json_without_scope_fields_loads_with_normal_blend_and_opacity_one() {
        // A pre-scope (v4) serialized track carries none of the new keys; it
        // must load with the neutral composite so its output is unchanged.
        let json = r#"{"id":"00000000-0000-0000-0000-000000000001","name":"V1","kind":"video"}"#;
        let t: Track = serde_json::from_str(json).unwrap();
        assert!(t.effects.is_empty());
        assert!(t.grade.is_none());
        assert_eq!(t.blend, BlendMode::Normal);
        assert_eq!(t.opacity, 1.0);
    }

    #[test]
    fn track_serde_round_trips_including_sync_lock() {
        let mut t = Track::new(TrackKind::Video, "V1");
        t.sync_lock = true;
        t.clips
            .push(Clip::new(ClipSource::Adjustment, Tick(0), Tick(100)));
        let json = serde_json::to_string(&t).unwrap();
        let back: Track = serde_json::from_str(&json).unwrap();
        assert_eq!(back, t);
        assert!(back.sync_lock);
    }

    #[test]
    fn track_sync_lock_defaults_false_when_absent_in_json() {
        // A pre-sync-lock serialized track (no `sync_lock` key) still loads.
        let json = r#"{"id":"00000000-0000-0000-0000-000000000001","name":"V1","kind":"video"}"#;
        let t: Track = serde_json::from_str(json).unwrap();
        assert!(!t.sync_lock);
    }

    #[test]
    fn duplicate_with_fresh_ids_reassigns_all_structural_ids() {
        let mut s = seq_with(vid_track_with(vec![(0, 100), (100, 50)]));
        s.markers.push(Marker::new(Tick(10), "m"));
        // A clip-scoped marker must survive with a remapped id (35 §1.6).
        s.video_tracks[0].clips[0]
            .markers
            .push(Marker::clip_scoped(Tick(5), "cm"));
        let dup = s.duplicate_with_fresh_ids();

        // Fresh identity everywhere so the copy addresses cleanly alongside
        // the original.
        assert_ne!(dup.id, s.id);
        assert_ne!(dup.video_tracks[0].id, s.video_tracks[0].id);
        for (a, b) in dup.video_tracks[0]
            .clips
            .iter()
            .zip(&s.video_tracks[0].clips)
        {
            assert_ne!(a.id, b.id);
        }
        assert_ne!(dup.markers[0].id, s.markers[0].id);
        // The clip marker got a fresh id but kept its position and name.
        let dup_cm = &dup.video_tracks[0].clips[0].markers[0];
        let src_cm = &s.video_tracks[0].clips[0].markers[0];
        assert_ne!(dup_cm.id, src_cm.id);
        assert_eq!(dup_cm.at, src_cm.at);
        assert_eq!(dup_cm.name, src_cm.name);
        // Content (timing) is otherwise identical.
        let spans = |seq: &Sequence| {
            seq.video_tracks[0]
                .clips
                .iter()
                .map(|c| (c.start, c.duration))
                .collect::<Vec<_>>()
        };
        assert_eq!(spans(&dup), spans(&s));
    }

    #[test]
    fn project_insert_sequence_sets_active_and_order() {
        let mut p = TimelineProject::new();
        let id = p.insert_sequence(Sequence::new("s", FrameRate::FPS_30, 1920, 1080));
        assert_eq!(p.active_sequence, Some(id));
        assert_eq!(p.sequence_order, vec![id]);
    }

    // ── Marker categories (35 §1.3) ──────────────────────────────────────

    #[test]
    fn marker_categories_absent_from_json_when_empty() {
        // Additive discipline: a project with no categories omits the key, so
        // pre-migration documents stay shape-identical.
        let p = TimelineProject::new();
        assert!(p.marker_categories.is_empty());
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("marker_categories"));
    }

    #[test]
    fn marker_category_round_trips_with_glyph() {
        let mut cat = MarkerCategory::new("Chapter", Color::rgb(0.2, 0.6, 0.9));
        cat.glyph = MarkerGlyph::Triangle;
        let json = serde_json::to_string(&cat).unwrap();
        assert!(json.contains("\"triangle\""));
        let back: MarkerCategory = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cat);
        // The default glyph is Diamond (the ruler already paints diamonds).
        assert_eq!(
            MarkerCategory::new("Marker", Color::WHITE).glyph,
            MarkerGlyph::Diamond
        );
    }

    #[test]
    fn default_seed_ids_are_unique() {
        let seed = MarkerCategory::default_seed();
        assert!(!seed.is_empty());
        let mut ids: Vec<_> = seed.iter().map(|c| c.id).collect();
        let n = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), n, "seed category ids must be unique");
        // `default_seed` is lazy: `TimelineProject::new` never seeds.
        assert!(TimelineProject::new().marker_categories.is_empty());
    }

    // ── Marker duration / anchor (35 §1) ─────────────────────────────────

    #[test]
    fn marker_defaults_are_point_timecode() {
        let m = Marker::new(Tick(42), "m");
        assert_eq!(m.duration, Tick::ZERO);
        assert!(!m.is_range());
        assert_eq!(m.anchor, MarkerAnchor::Timecode);
        assert_eq!(m.category, None);
        // A clip-scoped marker is Content-anchored (§1.5).
        assert_eq!(
            Marker::clip_scoped(Tick(0), "c").anchor,
            MarkerAnchor::Content
        );
    }

    #[test]
    fn marker_v4_json_without_new_fields_loads() {
        // §1.6: additive, no-op for existing data — a v4 marker JSON with only
        // id/at/name loads with the new fields defaulted.
        let json = r#"{"id":"00000000-0000-0000-0000-000000000001","at":100,"name":"x"}"#;
        let m: Marker = serde_json::from_str(json).unwrap();
        assert_eq!(m.duration, Tick::ZERO);
        assert_eq!(m.anchor, MarkerAnchor::Timecode);
        assert_eq!(m.category, None);
        assert_eq!(m.color, None);
    }

    #[test]
    fn marker_end_is_at_plus_duration() {
        let mut m = Marker::new(Tick(100), "ranged");
        m.duration = Tick(50);
        assert_eq!(m.end(), Tick(150));
        assert!(m.is_range());
        // A point marker's end equals its start.
        assert_eq!(Marker::new(Tick(100), "pt").end(), Tick(100));
    }

    #[test]
    fn unknown_marker_anchor_loads_inert() {
        // A v5 file written by a later build may carry an anchor token this
        // build does not know; it must load as the inert `Unknown` arm, never
        // error (39 §2.2).
        let json = r#"{"id":"00000000-0000-0000-0000-000000000001","at":0,"name":"x","anchor":"content_relative_future"}"#;
        let m: Marker = serde_json::from_str(json).unwrap();
        assert_eq!(m.anchor, MarkerAnchor::Unknown);
    }

    // ── Group tree (35 §3) ───────────────────────────────────────────────

    /// A sequence with one video track holding `n` back-to-back clips; returns
    /// the sequence and the clip ids in order.
    fn seq_with_clips(n: usize) -> (Sequence, Vec<ClipId>) {
        let mut t = Track::new(TrackKind::Video, "V1");
        let mut ids = Vec::new();
        for i in 0..n {
            let c = Clip::new(ClipSource::Adjustment, Tick(i as i64 * 100), Tick(100));
            ids.push(c.id);
            t.clips.push(c);
        }
        let mut s = Sequence::new("seq", FrameRate::FPS_30, 1920, 1080);
        s.video_tracks.push(t);
        (s, ids)
    }

    fn set_clip_group(s: &mut Sequence, clip: ClipId, g: Option<GroupId>) {
        for track in s.video_tracks.iter_mut() {
            for c in &mut track.clips {
                if c.id == clip {
                    c.group = g;
                }
            }
        }
    }

    #[test]
    fn groups_absent_from_json_when_empty() {
        let (s, _) = seq_with_clips(1);
        assert!(s.groups.is_empty());
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("groups"));
    }

    #[test]
    fn validate_rejects_group_cycle() {
        let (mut s, ids) = seq_with_clips(2);
        let a = GroupNode::new(GroupKind::Normal);
        let b = GroupNode::new(GroupKind::Normal);
        let (aid, bid) = (a.id, b.id);
        // a → b → a is a cycle.
        let mut a = a;
        let mut b = b;
        a.parent = Some(bid);
        b.parent = Some(aid);
        s.groups.insert(aid, a);
        s.groups.insert(bid, b);
        set_clip_group(&mut s, ids[0], Some(aid));
        set_clip_group(&mut s, ids[1], Some(bid));
        assert!(matches!(
            s.validate(),
            Err(ValidationError::GroupCycle { .. })
        ));
    }

    #[test]
    fn validate_rejects_unknown_group_ref() {
        let (mut s, ids) = seq_with_clips(1);
        // A clip points at a group that does not exist in-sequence.
        set_clip_group(&mut s, ids[0], Some(GroupId::new()));
        assert!(matches!(
            s.validate(),
            Err(ValidationError::UnknownGroup { .. })
        ));
    }

    #[test]
    fn validate_rejects_empty_group() {
        let (mut s, _) = seq_with_clips(1);
        let g = GroupNode::new(GroupKind::Normal);
        let gid = g.id;
        s.groups.insert(gid, g); // no members, no children
        assert!(matches!(
            s.validate(),
            Err(ValidationError::EmptyGroup { .. })
        ));
    }

    #[test]
    fn validate_rejects_singleton_normal_group_but_allows_singleton_avlink() {
        let (mut s, ids) = seq_with_clips(2);
        // A Normal group with one member is rejected.
        let g = GroupNode::new(GroupKind::Normal);
        let gid = g.id;
        s.groups.insert(gid, g);
        set_clip_group(&mut s, ids[0], Some(gid));
        assert!(matches!(
            s.validate(),
            Err(ValidationError::SingletonNormalGroup { .. })
        ));
        // The same shape as an AvLink group is allowed (a pair mid-edit).
        if let Some(node) = s.groups.get_mut(&gid) {
            node.kind = GroupKind::AvLink;
        }
        assert!(s.validate().is_ok());
    }

    #[test]
    fn topmost_group_walks_to_root() {
        let (mut s, ids) = seq_with_clips(3);
        let root = GroupNode::new(GroupKind::Normal);
        let mut child = GroupNode::new(GroupKind::Normal);
        let (rid, cid) = (root.id, child.id);
        child.parent = Some(rid);
        s.groups.insert(rid, root);
        s.groups.insert(cid, child);
        // Two clips in the child (so it is not a singleton Normal group) and one
        // directly in the root, keeping the sequence valid.
        set_clip_group(&mut s, ids[0], Some(cid));
        set_clip_group(&mut s, ids[1], Some(cid));
        set_clip_group(&mut s, ids[2], Some(rid));
        assert_eq!(s.topmost_group(cid), Some(rid));
        assert_eq!(s.group_chain(cid), vec![cid, rid]);
        assert!(s.validate().is_ok());
    }

    #[test]
    fn group_members_is_transitive() {
        let (mut s, ids) = seq_with_clips(2);
        let root = GroupNode::new(GroupKind::Normal);
        let mut child = GroupNode::new(GroupKind::Normal);
        let (rid, cid) = (root.id, child.id);
        child.parent = Some(rid);
        s.groups.insert(rid, root);
        s.groups.insert(cid, child);
        // ids[0] is in the child; ids[1] directly in the root.
        set_clip_group(&mut s, ids[0], Some(cid));
        set_clip_group(&mut s, ids[1], Some(rid));
        // The root's transitive members include the child's clip.
        let members = s.group_members(rid);
        assert_eq!(members.len(), 2);
        assert!(members.iter().any(|(_, c)| *c == ids[0]));
        assert!(members.iter().any(|(_, c)| *c == ids[1]));
        // The child only sees its own.
        assert_eq!(s.group_members(cid).len(), 1);
        assert_eq!(s.direct_group_members(rid).len(), 1);
    }

    #[test]
    fn dissolve_degenerate_groups_reparents_children() {
        let (mut s, ids) = seq_with_clips(2);
        // root ← mid ← (clip0); root also directly holds clip1. `mid` has one
        // clip and one... actually make `mid` empty of direct clips but with a
        // child to force dissolution while its child survives.
        let root = GroupNode::new(GroupKind::Normal);
        let mut mid = GroupNode::new(GroupKind::Normal);
        let mut leaf = GroupNode::new(GroupKind::Normal);
        let (rid, mid_id, lid) = (root.id, mid.id, leaf.id);
        mid.parent = Some(rid);
        leaf.parent = Some(mid_id);
        s.groups.insert(rid, root);
        s.groups.insert(mid_id, mid);
        s.groups.insert(lid, leaf);
        // clip0 in leaf, clip1 directly in root. `mid` has zero direct clips and
        // one child (leaf) → not empty, but leaf is a singleton Normal → leaf
        // dissolves first, reparenting clip0 to mid; then mid holds one clip and
        // is itself a singleton Normal → dissolves, reparenting clip0 to root.
        set_clip_group(&mut s, ids[0], Some(lid));
        set_clip_group(&mut s, ids[1], Some(rid));

        let dissolved = s.dissolve_degenerate_groups();
        assert!(dissolved.contains(&lid));
        assert!(dissolved.contains(&mid_id));
        assert!(!s.groups.contains_key(&lid));
        assert!(!s.groups.contains_key(&mid_id));
        // clip0 reparented up to the surviving root.
        let clip0_group = s.video_tracks[0]
            .clips
            .iter()
            .find(|c| c.id == ids[0])
            .and_then(|c| c.group);
        assert_eq!(clip0_group, Some(rid));
        // The result is a valid sequence (root now has two members).
        assert!(s.validate().is_ok());
    }
}
