//! `.photon` file-format versioning and forward migration.
//!
//! Documents carry a `format_version` (see [`crate::document::CURRENT_FORMAT_VERSION`]).
//! Rather than deserialize straight into [`Document`](crate::document::Document) and
//! reject anything unexpected, the loader first migrates the raw JSON
//! [`serde_json::Value`] up to the current version through an ordered chain of
//! [`FormatMigration`] steps, then deserializes. This lets older documents open
//! cleanly after the model grows, and lets slightly-newer documents load
//! leniently (unknown fields dropped) within a compatibility window.

use crate::timeline::GroupId;
use serde_json::Value;

/// How many versions ahead of the current one a file may be and still load
/// (with unknown fields dropped) before the loader refuses it outright.
pub const COMPAT_WINDOW: u32 = 1;

/// An error raised while migrating a document forward.
#[derive(Debug, Clone)]
pub enum MigrationError {
    /// A migration step failed.
    Failed { from: u32, to: u32, reason: String },
    /// The file is too far ahead of this build to load safely.
    TooNew { file: u32, supported: u32 },
}

impl std::fmt::Display for MigrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MigrationError::Failed { from, to, reason } => {
                write!(f, "format migration {from}→{to} failed: {reason}")
            }
            MigrationError::TooNew { file, supported } => write!(
                f,
                "unsupported format version {file} (this build supports up to {supported})"
            ),
        }
    }
}

impl std::error::Error for MigrationError {}

/// Upgrade a raw document [`Value`] from one format version to the next.
///
/// Implementations operate on the JSON tree directly (adding new fields with
/// defaults, renaming moved fields, etc.) before struct deserialization, so a
/// migration never has to know about the in-memory types.
pub trait FormatMigration: Send + Sync {
    /// The version this migration upgrades *from*.
    fn from_version(&self) -> u32;
    /// The version this migration upgrades *to* (must be `from_version() + 1`).
    fn to_version(&self) -> u32;
    /// Mutate `value` in place to the target version.
    fn migrate(&self, value: &mut Value) -> Result<(), String>;
}

/// The ordered migration chain. Each entry upgrades version N → N+1.
pub fn migrations() -> Vec<Box<dyn FormatMigration>> {
    vec![
        Box::new(V1ToV2),
        Box::new(V2ToV3),
        Box::new(V3ToV4),
        Box::new(V4ToV5),
    ]
}

/// v1 → v2: the `Raster` node kind was added. The change is purely additive —
/// existing v1 documents contain no raster nodes — so this only stamps the new
/// version number; serde defaults supply any missing fields on load.
struct V1ToV2;
impl FormatMigration for V1ToV2 {
    fn from_version(&self) -> u32 {
        1
    }
    fn to_version(&self) -> u32 {
        2
    }
    fn migrate(&self, _value: &mut Value) -> Result<(), String> {
        Ok(())
    }
}

/// v2 → v3: introduced the video-editor `timeline` field (01 §2). Purely
/// additive — `timeline` is `Option` + `#[serde(default)]`, so existing v2
/// documents contain no timeline and load unchanged; this only stamps the new
/// version number.
struct V2ToV3;
impl FormatMigration for V2ToV3 {
    fn from_version(&self) -> u32 {
        2
    }
    fn to_version(&self) -> u32 {
        3
    }
    fn migrate(&self, _value: &mut Value) -> Result<(), String> {
        Ok(())
    }
}

/// v3 → v4: clip anchors became explicitly center-relative. Existing v3
/// values were authored as absolute output-frame pixels, so preserve their
/// numeric values and tag both base and per-format reframe transforms.
struct V3ToV4;
impl FormatMigration for V3ToV4 {
    fn from_version(&self) -> u32 {
        3
    }
    fn to_version(&self) -> u32 {
        4
    }
    fn migrate(&self, value: &mut Value) -> Result<(), String> {
        let root = value.as_object_mut().ok_or("document is not an object")?;
        let Some(sequences) = root
            .get_mut("timeline")
            .and_then(Value::as_object_mut)
            .and_then(|timeline| timeline.get_mut("sequences"))
            .and_then(Value::as_object_mut)
        else {
            return Ok(());
        };

        for sequence in sequences.values_mut().filter_map(Value::as_object_mut) {
            for track_key in ["video_tracks", "audio_tracks"] {
                let Some(tracks) = sequence.get_mut(track_key).and_then(Value::as_array_mut) else {
                    continue;
                };
                for clip in tracks
                    .iter_mut()
                    .filter_map(Value::as_object_mut)
                    .filter_map(|track| track.get_mut("clips"))
                    .filter_map(Value::as_array_mut)
                    .flatten()
                    .filter_map(Value::as_object_mut)
                {
                    if let Some(base) = clip
                        .get_mut("transform")
                        .and_then(Value::as_object_mut)
                        .and_then(|transform| transform.get_mut("base"))
                        .and_then(Value::as_object_mut)
                    {
                        base.entry("anchor_space")
                            .or_insert_with(|| Value::from("absolute"));
                    }
                    if let Some(reframes) = clip.get_mut("reframe").and_then(Value::as_object_mut) {
                        for transform in reframes.values_mut().filter_map(Value::as_object_mut) {
                            transform
                                .entry("anchor_space")
                                .or_insert_with(|| Value::from("absolute"));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// v4 → v5: the §35 scope / marker / group model, folded together with the six
/// sibling changes that share this format step (01 §9.1's nine-change
/// inventory). All but one are pure serde-default additions — markers gain
/// `duration`/`category`/`anchor`; tracks, sequence masters and media assets
/// gain effect+grade scopes; `MarkerCategory` gains a glyph; `ClipEffect` gains
/// `id`/`version`; captions and clip-audio gain fields; and the
/// unknown-preserving enum variants (39 §2.2) land — so their helpers are
/// no-ops: serde supplies the defaults on load, and the effect-identity backfill
/// happens in `finalize_load`, not here. The single tree-walking change is the
/// deprecated `link_group` → [`GroupKind::AvLink`](crate::timeline::GroupKind)
/// projection (35 §3).
///
/// The migration is deliberately ONE struct whose `migrate()` fans out to one
/// private helper per owning section, so the v5 version number means "all nine
/// changes"; a v5 file is readable across every section rather than only §35.
struct V4ToV5;

impl V4ToV5 {
    /// 39 §2.2 (lands FIRST, per 01 §9.1's non-negotiable ordering): the
    /// unknown-preserving variants (`MarkerAnchor::Unknown`, `GroupKind::Unknown`
    /// and the payload-carrying `*::Unknown` arms) exist in the type system from
    /// birth, so there is nothing to rewrite in the tree.
    fn migrate_unknown_variants(_value: &mut Value) {}

    /// 35 §1/§2: markers gain `duration`/`category`/`anchor`; tracks, sequence
    /// masters and assets gain effect+grade scopes. All additive (§1.6, §2.6) —
    /// serde defaults supply them.
    fn migrate_scopes_and_markers(_value: &mut Value) {}

    /// 41 §7: `MarkerCategory.glyph`. Additive (defaults to `Diamond`).
    fn migrate_marker_glyph(_value: &mut Value) {}

    /// 30 §10: `ClipEffect.id`/`.version`. Additive — an absent id is backfilled
    /// from `kind` in `finalize_load`, not in the JSON tree.
    fn migrate_effect_identity(_value: &mut Value) {}

    /// 42: `CaptionTrack.language` / `CaptionStyle.direction`. Additive.
    fn migrate_caption_fields(_value: &mut Value) {}

    /// 31 §7: `ClipAudio.stream` / `.offset`. Additive.
    fn migrate_clip_audio_fields(_value: &mut Value) {}

    /// 39 §1.6: `Track.height_px` moves to the UI sidecar. A stray in-tree
    /// `height_px` is an unknown field serde drops on load, so no rewrite is
    /// required at the JSON level here.
    fn migrate_height_sidecar(_value: &mut Value) {}

    /// 35 §3: project the deprecated per-clip `link_group` into a
    /// `GroupKind::AvLink` group tree — the only tree-walking change in v4 → v5.
    /// Each distinct `link_group` value in a sequence becomes one AvLink
    /// [`GroupNode`](crate::timeline::GroupNode) that its member clips point at
    /// via `group`; the original `link_group` is retained (deprecated for one
    /// format version). A singleton AvLink group is legal (an A/V pair mid-edit),
    /// so the projection never needs to prune.
    fn migrate_link_groups(value: &mut Value) {
        let Some(sequences) = value
            .as_object_mut()
            .and_then(|root| root.get_mut("timeline"))
            .and_then(Value::as_object_mut)
            .and_then(|timeline| timeline.get_mut("sequences"))
            .and_then(Value::as_object_mut)
        else {
            return;
        };

        for sequence in sequences.values_mut().filter_map(Value::as_object_mut) {
            // Pass 1: assign a fresh AvLink GroupId to each distinct link_group,
            // and bind each carrying clip to it.
            let mut remap: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            for track_key in ["video_tracks", "audio_tracks"] {
                let Some(tracks) = sequence.get_mut(track_key).and_then(Value::as_array_mut) else {
                    continue;
                };
                for clip in tracks
                    .iter_mut()
                    .filter_map(Value::as_object_mut)
                    .filter_map(|track| track.get_mut("clips"))
                    .filter_map(Value::as_array_mut)
                    .flatten()
                    .filter_map(Value::as_object_mut)
                {
                    let Some(link_group) = clip.get("link_group").and_then(Value::as_str) else {
                        continue;
                    };
                    let link_group = link_group.to_owned();
                    let gid = remap
                        .entry(link_group)
                        .or_insert_with(|| GroupId::new().to_string())
                        .clone();
                    // A clip already carrying an explicit `group` keeps it (v4 has
                    // none, but be defensive); otherwise bind it to its AvLink group.
                    clip.entry("group").or_insert_with(|| Value::from(gid));
                }
            }
            if remap.is_empty() {
                continue;
            }
            // Pass 2: materialise the AvLink GroupNodes into `sequence.groups`.
            let groups = sequence
                .entry("groups")
                .or_insert_with(|| Value::Object(serde_json::Map::new()));
            if let Some(groups) = groups.as_object_mut() {
                for gid in remap.values() {
                    let mut node = serde_json::Map::new();
                    node.insert("id".into(), Value::from(gid.clone()));
                    node.insert("kind".into(), Value::from("av_link"));
                    groups.insert(gid.clone(), Value::Object(node));
                }
            }
        }
    }
}

impl FormatMigration for V4ToV5 {
    fn from_version(&self) -> u32 {
        4
    }
    fn to_version(&self) -> u32 {
        5
    }
    fn migrate(&self, value: &mut Value) -> Result<(), String> {
        // 39 §2.2's unknown-preserving variants land first (01 §9.1 ordering).
        Self::migrate_unknown_variants(value);
        Self::migrate_scopes_and_markers(value);
        Self::migrate_marker_glyph(value);
        Self::migrate_effect_identity(value);
        Self::migrate_caption_fields(value);
        Self::migrate_clip_audio_fields(value);
        Self::migrate_height_sidecar(value);
        Self::migrate_link_groups(value);
        Ok(())
    }
}

/// Read `format_version` from a raw document value, defaulting to 1 when absent
/// (documents predating the field).
pub fn detect_version(value: &Value) -> u32 {
    value
        .get("format_version")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .unwrap_or(1)
}

/// Apply `chain` to bring `value` up to `target`, returning the resulting
/// version. Stops early (without error) once no migration advances further —
/// remaining gaps are filled by serde defaults at deserialization time.
pub fn run_migrations_with(
    value: &mut Value,
    chain: &[Box<dyn FormatMigration>],
    target: u32,
) -> Result<u32, MigrationError> {
    let mut version = detect_version(value);
    while version < target {
        let Some(m) = chain.iter().find(|m| m.from_version() == version) else {
            break;
        };
        m.migrate(value).map_err(|reason| MigrationError::Failed {
            from: m.from_version(),
            to: m.to_version(),
            reason,
        })?;
        version = m.to_version();
        if let Some(obj) = value.as_object_mut() {
            obj.insert("format_version".into(), Value::from(version));
        }
    }
    Ok(version)
}

/// Apply the built-in [`migrations`] chain up to `target`.
pub fn run_migrations(value: &mut Value, target: u32) -> Result<u32, MigrationError> {
    run_migrations_with(value, &migrations(), target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct AddField {
        from: u32,
        key: &'static str,
    }
    impl FormatMigration for AddField {
        fn from_version(&self) -> u32 {
            self.from
        }
        fn to_version(&self) -> u32 {
            self.from + 1
        }
        fn migrate(&self, value: &mut Value) -> Result<(), String> {
            value
                .as_object_mut()
                .ok_or("not an object")?
                .insert(self.key.into(), json!(true));
            Ok(())
        }
    }

    #[test]
    fn detect_version_defaults_to_one() {
        assert_eq!(detect_version(&json!({})), 1);
        assert_eq!(detect_version(&json!({"format_version": 3})), 3);
    }

    #[test]
    fn v3_to_v4_tags_all_clip_transforms_without_changing_values() {
        let clip = |anchor_x, keyed| {
            json!({
                "transform": {
                    "base": { "anchor_x": anchor_x, "anchor_y": -7.0 },
                    "tracks": [{ "path": "transform.anchor_x", "keys": keyed }]
                },
                "reframe": {
                    "0": { "anchor_x": anchor_x + 1.0, "anchor_y": 9.0 }
                }
            })
        };
        let keys = json!([{ "time": 12, "value": { "t": "float", "v": 42.0 } }]);
        let mut value = json!({
            "format_version": 3,
            "timeline": { "sequences": {
                "outer": {
                    "video_tracks": [{ "clips": [clip(10.0, keys.clone())] }],
                    "audio_tracks": [{ "clips": [clip(20.0, keys.clone())] }]
                },
                "nested": {
                    "video_tracks": [{ "clips": [clip(30.0, keys.clone())] }]
                }
            }}
        });

        let before_keys = value["timeline"]["sequences"]["outer"]["video_tracks"][0]["clips"][0]
            ["transform"]["tracks"]
            .clone();
        assert_eq!(run_migrations(&mut value, 4).unwrap(), 4);

        for (sequence, track, expected) in [
            ("outer", "video_tracks", 10.0),
            ("outer", "audio_tracks", 20.0),
            ("nested", "video_tracks", 30.0),
        ] {
            let clip = &value["timeline"]["sequences"][sequence][track][0]["clips"][0];
            assert_eq!(clip["transform"]["base"]["anchor_space"], "absolute");
            assert_eq!(clip["transform"]["base"]["anchor_x"], expected);
            assert_eq!(clip["reframe"]["0"]["anchor_space"], "absolute");
            assert_eq!(clip["reframe"]["0"]["anchor_x"], expected + 1.0);
        }
        assert_eq!(
            value["timeline"]["sequences"]["outer"]["video_tracks"][0]["clips"][0]["transform"]
                ["tracks"],
            before_keys
        );
    }

    #[test]
    fn v3_to_v4_preserves_explicit_anchor_space_and_tolerates_optional_shapes() {
        let mut value = json!({
            "format_version": 3,
            "timeline": { "sequences": {
                "valid": { "video_tracks": [{ "clips": [{
                    "transform": { "base": { "anchor_space": "center_offset" } },
                    "reframe": { "0": { "anchor_space": "center_offset" } }
                }]}]},
                "malformed_optional": { "video_tracks": "not-an-array" }
            }}
        });
        assert_eq!(run_migrations(&mut value, 4).unwrap(), 4);
        let clip = &value["timeline"]["sequences"]["valid"]["video_tracks"][0]["clips"][0];
        assert_eq!(clip["transform"]["base"]["anchor_space"], "center_offset");
        assert_eq!(clip["reframe"]["0"]["anchor_space"], "center_offset");
    }

    #[test]
    fn chain_applies_in_order_and_bumps_version() {
        let chain: Vec<Box<dyn FormatMigration>> = vec![
            Box::new(AddField { from: 1, key: "a" }),
            Box::new(AddField { from: 2, key: "b" }),
        ];
        let mut v = json!({ "format_version": 1 });
        let out = run_migrations_with(&mut v, &chain, 3).unwrap();
        assert_eq!(out, 3);
        assert_eq!(v["format_version"], 3);
        assert_eq!(v["a"], json!(true));
        assert_eq!(v["b"], json!(true));
    }

    #[test]
    fn stops_when_no_migration_advances() {
        // Chain can only reach v2; target is v3. Should stop at v2, no error.
        let chain: Vec<Box<dyn FormatMigration>> = vec![Box::new(AddField { from: 1, key: "a" })];
        let mut v = json!({ "format_version": 1 });
        let out = run_migrations_with(&mut v, &chain, 3).unwrap();
        assert_eq!(out, 2);
    }

    #[test]
    fn failing_migration_reports_error() {
        struct Boom;
        impl FormatMigration for Boom {
            fn from_version(&self) -> u32 {
                1
            }
            fn to_version(&self) -> u32 {
                2
            }
            fn migrate(&self, _v: &mut Value) -> Result<(), String> {
                Err("kaboom".into())
            }
        }
        let chain: Vec<Box<dyn FormatMigration>> = vec![Box::new(Boom)];
        let mut v = json!({ "format_version": 1 });
        let err = run_migrations_with(&mut v, &chain, 2).unwrap_err();
        assert!(matches!(err, MigrationError::Failed { from: 1, to: 2, .. }));
    }

    #[test]
    fn current_chain_is_a_noop_for_v1() {
        let mut v = json!({ "format_version": 1 });
        assert_eq!(run_migrations(&mut v, 1).unwrap(), 1);
    }

    // ── v4 → v5 (spec 35 + the 01 §9.1 sibling changes) ──────────────────

    #[test]
    fn v4_to_v5_is_a_noop_for_a_document_with_no_timeline() {
        // Mirrors `current_chain_is_a_noop_for_v1`: a document with no timeline
        // (all v5 changes are additive there) survives the step unchanged bar the
        // version stamp.
        let mut v = json!({ "format_version": 4 });
        assert_eq!(run_migrations(&mut v, 5).unwrap(), 5);
        assert_eq!(v["format_version"], 5);
        // No spurious timeline is fabricated.
        assert!(v.get("timeline").is_none());
    }

    #[test]
    fn v4_to_v5_stamps_the_version() {
        let mut v = json!({
            "format_version": 4,
            "timeline": { "sequences": {} }
        });
        assert_eq!(run_migrations(&mut v, 5).unwrap(), 5);
        assert_eq!(v["format_version"], 5);
    }

    #[test]
    fn v4_to_v5_projects_link_group_into_one_shared_avlink_group() {
        // Two clips (one video, one audio) sharing a link_group become one
        // AvLink group both point at (35 §3).
        let mut v = json!({
            "format_version": 4,
            "timeline": { "sequences": {
                "s": {
                    "video_tracks": [{ "clips": [{ "link_group": "lg-1" }] }],
                    "audio_tracks": [{ "clips": [{ "link_group": "lg-1" }] }]
                }
            }}
        });
        assert_eq!(run_migrations(&mut v, 5).unwrap(), 5);

        let seq = &v["timeline"]["sequences"]["s"];
        let vgroup = seq["video_tracks"][0]["clips"][0]["group"]
            .as_str()
            .unwrap();
        let agroup = seq["audio_tracks"][0]["clips"][0]["group"]
            .as_str()
            .unwrap();
        assert_eq!(
            vgroup, agroup,
            "both clips must bind to the same AvLink group"
        );

        let groups = seq["groups"].as_object().unwrap();
        assert_eq!(groups.len(), 1, "one distinct link_group → one group node");
        let node = &groups[vgroup];
        assert_eq!(node["kind"], "av_link");
        assert_eq!(node["id"].as_str().unwrap(), vgroup);
        // The deprecated link_group is retained for one format version.
        assert_eq!(seq["video_tracks"][0]["clips"][0]["link_group"], "lg-1");
    }

    #[test]
    fn v4_to_v5_distinct_link_groups_get_distinct_avlink_groups() {
        let mut v = json!({
            "format_version": 4,
            "timeline": { "sequences": { "s": {
                "video_tracks": [{ "clips": [
                    { "link_group": "a" },
                    { "link_group": "b" },
                    { }
                ] }]
            }}}
        });
        assert_eq!(run_migrations(&mut v, 5).unwrap(), 5);

        let seq = &v["timeline"]["sequences"]["s"];
        let g0 = seq["video_tracks"][0]["clips"][0]["group"]
            .as_str()
            .unwrap();
        let g1 = seq["video_tracks"][0]["clips"][1]["group"]
            .as_str()
            .unwrap();
        assert_ne!(
            g0, g1,
            "distinct link_groups must not collapse into one group"
        );
        // A clip with no link_group is left ungrouped.
        assert!(seq["video_tracks"][0]["clips"][2].get("group").is_none());
        assert_eq!(seq["groups"].as_object().unwrap().len(), 2);
    }
}
