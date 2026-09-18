//! Strongly-typed id newtypes for the timeline data model (01 §1).
//!
//! Every timeline entity gets its own `Uuid` newtype rather than sharing the
//! repo-wide `pub type NodeId = Uuid` alias, so a `ClipId` can never be passed
//! where a `TrackId` is expected. The [`id_newtype!`] macro stamps out the full
//! `Clone + Copy + Debug + PartialEq + Eq + Hash + Serialize + Deserialize`
//! derive set plus `new()`/`nil()` constructors and a `Display` that renders the
//! inner `Uuid`.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Declare one or more `Uuid` newtypes with the standard timeline derive set.
macro_rules! id_newtype {
    ($($(#[$meta:meta])* $name:ident),* $(,)?) => {
        $(
            $(#[$meta])*
            #[derive(
                Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash,
                Serialize, Deserialize,
            )]
            #[serde(transparent)]
            pub struct $name(pub Uuid);

            impl $name {
                /// A fresh random (v4) id.
                #[inline]
                pub fn new() -> Self {
                    Self(Uuid::new_v4())
                }

                /// The nil id (all-zero) — a stable sentinel for tests/defaults.
                #[inline]
                pub const fn nil() -> Self {
                    Self(Uuid::nil())
                }
            }

            impl Default for $name {
                #[inline]
                fn default() -> Self {
                    Self::new()
                }
            }

            impl std::fmt::Display for $name {
                fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    write!(f, "{}", self.0)
                }
            }

            impl From<Uuid> for $name {
                #[inline]
                fn from(u: Uuid) -> Self {
                    Self(u)
                }
            }
        )*
    };
}

id_newtype! {
    /// Identifies a [`Clip`](crate::timeline::Clip) within a track.
    ClipId,
    /// Identifies a [`Track`](crate::timeline::Track) within a sequence.
    TrackId,
    /// Identifies a [`Sequence`](crate::timeline::Sequence) within a project.
    SequenceId,
    /// Identifies a [`MediaAsset`](crate::timeline::MediaAsset) in the media pool.
    AssetId,
    /// Identifies a [`NodeGraph`](crate::timeline::NodeGraph) in the graph arena.
    GraphId,
    /// Identifies a [`GraphNode`](crate::timeline::GraphNode) within a graph.
    GraphNodeId,
    /// Identifies a [`Marker`](crate::timeline::Marker) on a sequence.
    MarkerId,
    /// Identifies a [`CaptionCue`](crate::timeline::CaptionCue) on a caption track.
    CueId,
    /// Identifies a [`GradeOp`](crate::timeline::GradeOp) within a grade stack (07 §1).
    GradeOpId,
    /// Identifies a media bin (folder) in the media pool.
    BinId,
    /// Identifies a [`MarkerCategory`](crate::timeline::MarkerCategory) in
    /// [`TimelineProject::marker_categories`](crate::timeline::TimelineProject).
    /// Referenced by stable id, never by index (35 §1.3).
    MarkerCategoryId,
    /// Identifies a [`GroupNode`](crate::timeline::GroupNode) in
    /// [`Sequence::groups`](crate::timeline::Sequence) (35 §3).
    GroupId,
    /// Identifies a media-pool tag in
    /// [`TimelineProject::media_tags`](crate::timeline::TimelineProject)
    /// (26 K-C2). Referenced by stable id, never by index — same taxonomy
    /// pattern as [`MarkerCategoryId`].
    TagId,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_distinct_types_but_same_repr() {
        // Two fresh ids differ; the nil sentinel is stable.
        assert_ne!(ClipId::new(), ClipId::new());
        assert_eq!(ClipId::nil(), ClipId::nil());
        // The markers/groups migration ids behave identically.
        assert_ne!(MarkerCategoryId::new(), MarkerCategoryId::new());
        assert_eq!(MarkerCategoryId::nil(), MarkerCategoryId::nil());
        assert_ne!(GroupId::new(), GroupId::new());
        assert_eq!(GroupId::nil(), GroupId::nil());
    }

    #[test]
    fn marker_category_and_group_ids_serde_transparent() {
        // `#[serde(transparent)]` → the JSON is the bare uuid string, mirroring
        // `id_serde_is_transparent_uuid`.
        let cat = MarkerCategoryId::new();
        let cat_json = serde_json::to_string(&cat).unwrap();
        let as_uuid: Uuid = serde_json::from_str(&cat_json).unwrap();
        assert_eq!(as_uuid, cat.0);
        let round: MarkerCategoryId = serde_json::from_str(&cat_json).unwrap();
        assert_eq!(round, cat);

        let grp = GroupId::new();
        let grp_json = serde_json::to_string(&grp).unwrap();
        let as_uuid: Uuid = serde_json::from_str(&grp_json).unwrap();
        assert_eq!(as_uuid, grp.0);
        let round: GroupId = serde_json::from_str(&grp_json).unwrap();
        assert_eq!(round, grp);
    }

    #[test]
    fn id_serde_is_transparent_uuid() {
        let id = TrackId::new();
        let json = serde_json::to_string(&id).unwrap();
        // `#[serde(transparent)]` → the JSON is just the bare uuid string.
        let as_uuid: Uuid = serde_json::from_str(&json).unwrap();
        assert_eq!(as_uuid, id.0);
        let round: TrackId = serde_json::from_str(&json).unwrap();
        assert_eq!(round, id);
    }
}
