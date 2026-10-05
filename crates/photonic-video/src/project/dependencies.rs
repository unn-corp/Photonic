//! Read-only project dependency inventory for conform and archive preflight.
//! File existence is checked for every pool asset; full-byte integrity is
//! checked where a LUT pin or captured-still hash exists.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use photonic_core::timeline::{AssetId, AssetKind, AssetSource};
use photonic_core::Document;
use serde::Serialize;

use super::archive::resolve_source;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyStatus {
    Available,
    Missing,
    Changed,
    Unreadable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FileDependency {
    pub asset: AssetId,
    pub kind: AssetKind,
    pub path: PathBuf,
    pub status: DependencyStatus,
    /// True only when a full-file LUT or reference-still pin was checked.
    pub integrity_checked: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MissingReference {
    pub sequence: photonic_core::timeline::SequenceId,
    pub name: String,
    pub image_asset: AssetId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DependencyReport {
    pub files: Vec<FileDependency>,
    pub missing_references: Vec<MissingReference>,
    pub ready: bool,
}

/// Inspect all file-backed pool entries. A file without an integrity pin is
/// reported as available but explicitly unverified, never as hash-verified.
/// This scans the whole project, including unused media, because portable
/// archives collect the whole pool rather than only the active sequence.
pub fn inspect_dependencies(document: &Document, project_file: Option<&Path>) -> DependencyReport {
    let Some(project) = document.timeline.as_ref() else {
        return DependencyReport {
            files: Vec::new(),
            missing_references: Vec::new(),
            ready: true,
        };
    };
    let mut still_hashes: HashMap<AssetId, Vec<&str>> = HashMap::new();
    let mut missing_references = Vec::new();
    for sequence in project.sequences.values() {
        for still in &sequence.reference_stills {
            if !project
                .media
                .assets
                .get(&still.image_asset)
                .is_some_and(|asset| matches!(asset.source, AssetSource::File { .. }))
            {
                missing_references.push(MissingReference {
                    sequence: sequence.id,
                    name: still.name.clone(),
                    image_asset: still.image_asset,
                });
            }
            still_hashes
                .entry(still.image_asset)
                .or_default()
                .push(&still.image_hash);
        }
    }
    let mut files = Vec::new();
    for asset in project.media.assets.values() {
        let AssetSource::File { path, rel_path } = &asset.source else {
            continue;
        };
        let resolved = resolve_source(path, rel_path.as_deref(), project_file);
        let selected = resolved.as_deref().unwrap_or(path);
        let pins = still_hashes.get(&asset.id);
        let has_pin = asset.lut_full_hash.is_some() || pins.is_some();
        let status = if resolved.is_none() {
            DependencyStatus::Missing
        } else if has_pin {
            match crate::media::full_content_hash(selected) {
                Ok(actual) => {
                    let lut_matches = asset
                        .lut_full_hash
                        .as_ref()
                        .is_none_or(|expected| expected == &actual);
                    let stills_match = pins.is_none_or(|hashes| {
                        hashes
                            .iter()
                            .all(|expected| !expected.is_empty() && *expected == actual)
                    });
                    if lut_matches && stills_match {
                        DependencyStatus::Available
                    } else {
                        DependencyStatus::Changed
                    }
                }
                Err(_) => DependencyStatus::Unreadable,
            }
        } else if std::fs::File::open(selected).is_ok() {
            DependencyStatus::Available
        } else {
            DependencyStatus::Unreadable
        };
        files.push(FileDependency {
            asset: asset.id,
            kind: asset.kind,
            path: selected.to_path_buf(),
            status,
            integrity_checked: has_pin && status == DependencyStatus::Available,
        });
    }
    files.sort_by_key(|file| file.asset);
    missing_references.sort_by_key(|reference| (reference.sequence, reference.image_asset));
    let ready = files
        .iter()
        .all(|file| file.status == DependencyStatus::Available)
        && missing_references.is_empty();
    DependencyReport {
        files,
        missing_references,
        ready,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::timeline::{MediaAsset, TimelineProject};

    #[test]
    fn inventory_distinguishes_available_changed_and_missing_files() {
        let root =
            std::env::temp_dir().join(format!("photonic-dependencies-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let lut = root.join("look.cube");
        let media = root.join("source.mov");
        std::fs::write(&lut, b"LUT_3D_SIZE 2\n").unwrap();
        std::fs::write(&media, b"video").unwrap();
        let mut project = TimelineProject::new();
        let mut lut_asset = MediaAsset::from_file(AssetKind::Lut3d, &lut);
        lut_asset.lut_full_hash = Some(crate::media::full_content_hash(&lut).unwrap());
        let lut_id = project.media.insert(lut_asset);
        let media_id = project
            .media
            .insert(MediaAsset::from_file(AssetKind::Video, &media));
        let mut doc = Document::new("dependencies", 16.0, 16.0);
        doc.timeline = Some(project);

        let available = inspect_dependencies(&doc, None);
        assert!(available.ready);
        assert!(
            available
                .files
                .iter()
                .find(|entry| entry.asset == lut_id)
                .unwrap()
                .integrity_checked
        );
        assert!(
            !available
                .files
                .iter()
                .find(|entry| entry.asset == media_id)
                .unwrap()
                .integrity_checked
        );

        std::fs::write(&lut, b"LUT_3D_SIZE 2\nchanged").unwrap();
        std::fs::remove_file(&media).unwrap();
        let failed = inspect_dependencies(&doc, None);
        assert!(!failed.ready);
        assert_eq!(
            failed
                .files
                .iter()
                .find(|entry| entry.asset == lut_id)
                .unwrap()
                .status,
            DependencyStatus::Changed
        );
        assert_eq!(
            failed
                .files
                .iter()
                .find(|entry| entry.asset == media_id)
                .unwrap()
                .status,
            DependencyStatus::Missing
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
