//! Declared scale budget targets (37 §3.2).
//!
//! These name the project sizes the editor is expected to stay responsive at.
//! They are **budget targets, not hard limits**: a project may exceed any of
//! them, and doing so degrades speed, never correctness. There is deliberately
//! no validation anywhere that rejects a project above a target — a later
//! engineer must NOT turn these into `assert!`s or reject-on-import checks in
//! the model. Their only jobs are to give "is this fast enough?" a concrete
//! answer in code and to anchor the scale-target acceptance tests.

/// Tracks per sequence.
pub const TARGET_TRACKS_PER_SEQUENCE: usize = 100;
/// Clips per sequence.
pub const TARGET_CLIPS_PER_SEQUENCE: usize = 10_000;
/// Media assets per project.
pub const TARGET_ASSETS_PER_PROJECT: usize = 5_000;
/// Keyframes on a single animated property track.
pub const TARGET_KEYFRAMES_PER_PROPERTY_TRACK: usize = 10_000;
/// Caption cues on a single caption track.
pub const TARGET_CUES_PER_CAPTION_TRACK: usize = 50_000;
/// Graph nodes in one composition.
pub const TARGET_GRAPH_NODES_PER_COMPOSITION: usize = 500;
/// Sequences per project.
pub const TARGET_SEQUENCES_PER_PROJECT: usize = 200;

#[cfg(test)]
mod tests {
    use super::*;

    /// The §3.2 table, pinned so an accidental edit to a target is caught. If a
    /// target legitimately changes, the spec table and this test change
    /// together — that is the point.
    #[test]
    fn targets_match_spec_table() {
        assert_eq!(TARGET_TRACKS_PER_SEQUENCE, 100);
        assert_eq!(TARGET_CLIPS_PER_SEQUENCE, 10_000);
        assert_eq!(TARGET_ASSETS_PER_PROJECT, 5_000);
        assert_eq!(TARGET_KEYFRAMES_PER_PROPERTY_TRACK, 10_000);
        assert_eq!(TARGET_CUES_PER_CAPTION_TRACK, 50_000);
        assert_eq!(TARGET_GRAPH_NODES_PER_COMPOSITION, 500);
        assert_eq!(TARGET_SEQUENCES_PER_PROJECT, 200);
    }
}
