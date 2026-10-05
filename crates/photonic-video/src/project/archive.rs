//! Collect file-backed timeline assets into a portable project folder.
//!
//! The archive deliberately omits undo history: historical document snapshots
//! may reference files that are no longer present in the current project.

use photonic_core::timeline::AssetSource;
use photonic_core::Document;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// A file needed to build an archive could not be read. Nothing is published.
#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("archive destination already exists: {0}")]
    DestinationExists(PathBuf),
    #[error("asset {asset} is offline: {path}")]
    Offline { asset: String, path: PathBuf },
    #[error("asset {asset} changed while copying: {path}")]
    Changed { asset: String, path: PathBuf },
    #[error("reference still {name} has a missing or changed image")]
    ReferenceChanged { name: String },
    #[error("invalid archive destination")]
    InvalidDestination,
    #[error("archive I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("archive serialization: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// Archive the current document as `destination/<name>.photon` plus `media/`.
/// Every file-backed pool asset, including LUTs and reference stills, is copied.
/// An offline asset aborts before publishing the directory. Existing destinations
/// are never overwritten. Cached proxies are omitted and rebuilt on demand.
pub fn archive_project(
    document: &Document,
    source_project: Option<&Path>,
    destination: &Path,
) -> Result<PathBuf, ArchiveError> {
    if destination.exists() {
        return Err(ArchiveError::DestinationExists(destination.to_path_buf()));
    }
    let name = destination
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or(ArchiveError::InvalidDestination)?;
    let parent = destination.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let staging = parent.join(format!(
        ".{}.{}.collecting",
        name.to_string_lossy(),
        uuid::Uuid::new_v4()
    ));
    fs::create_dir(&staging)?;
    let result =
        (|| {
            let mut archived = document.clone();
            let mut copied: HashMap<PathBuf, PathBuf> = HashMap::new();
            if let Some(project) = archived.timeline.as_mut() {
                let reference_hashes: Vec<_> = project
                    .sequences
                    .values()
                    .flat_map(|sequence| &sequence.reference_stills)
                    .map(|still| (still.image_asset, (&still.name, &still.image_hash)))
                    .collect();
                for (asset_id, (name, _)) in &reference_hashes {
                    if !project
                        .media
                        .assets
                        .get(asset_id)
                        .is_some_and(|asset| matches!(asset.source, AssetSource::File { .. }))
                    {
                        return Err(ArchiveError::ReferenceChanged {
                            name: (*name).clone(),
                        });
                    }
                }
                let media_dir = staging.join("media");
                fs::create_dir(&media_dir)?;
                for asset in project.media.assets.values_mut() {
                    let AssetSource::File { path, rel_path } = &mut asset.source else {
                        continue;
                    };
                    let source = resolve_source(path, rel_path.as_deref(), source_project)
                        .ok_or_else(|| ArchiveError::Offline {
                            asset: asset.id.to_string(),
                            path: path.clone(),
                        })?;
                    let canonical = fs::canonicalize(&source)?;
                    let stills: Vec<_> = reference_hashes
                        .iter()
                        .filter(|(image_asset, _)| *image_asset == asset.id)
                        .collect();
                    if !stills.is_empty() {
                        let source_hash = crate::media::full_content_hash(&source)?;
                        for (_, (name, expected)) in stills {
                            if expected.is_empty() || source_hash != expected.as_str() {
                                return Err(ArchiveError::ReferenceChanged {
                                    name: (*name).clone(),
                                });
                            }
                        }
                    }
                    let relative = if let Some(existing) = copied.get(&canonical) {
                        existing.clone()
                    } else {
                        let extension = source
                            .extension()
                            .and_then(|value| value.to_str())
                            .filter(|value| value.chars().all(|c| c.is_ascii_alphanumeric()))
                            .unwrap_or("bin");
                        let relative =
                            PathBuf::from("media").join(format!("{}.{}", asset.id, extension));
                        let target = staging.join(&relative);
                        let before = crate::media::full_content_hash(&source)?;
                        fs::copy(&source, &target)?;
                        let after = crate::media::full_content_hash(&source)?;
                        let copied_hash = crate::media::full_content_hash(&target)?;
                        if before != after || before != copied_hash {
                            return Err(ArchiveError::Changed {
                                asset: asset.id.to_string(),
                                path: source,
                            });
                        }
                        copied.insert(canonical, relative.clone());
                        relative
                    };
                    // Opening a moved archive resolves this fallback against its
                    // `.photon` location before the engine sees the document.
                    *rel_path = Some(relative);
                    *path = destination.join(rel_path.as_ref().unwrap());
                    asset.proxy = None;
                }
            }
            let project_file = staging.join(format!("{}.photon", name.to_string_lossy()));
            let json = photonic_core::save_photon(&archived, None)?;
            photonic_core::write_atomic_file(&project_file, json.as_bytes())?;
            publish_directory(&staging, destination)?;
            Ok(destination.join(project_file.file_name().unwrap()))
        })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

#[cfg(target_os = "linux")]
fn publish_directory(staging: &Path, destination: &Path) -> Result<(), ArchiveError> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let source = CString::new(staging.as_os_str().as_bytes())
        .map_err(|_| ArchiveError::InvalidDestination)?;
    let target = CString::new(destination.as_os_str().as_bytes())
        .map_err(|_| ArchiveError::InvalidDestination)?;
    // `RENAME_NOREPLACE` keeps a concurrently created destination intact.
    let status = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            target.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if status == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::AlreadyExists {
        Err(ArchiveError::DestinationExists(destination.to_path_buf()))
    } else {
        Err(ArchiveError::Io(error))
    }
}

#[cfg(not(target_os = "linux"))]
fn publish_directory(staging: &Path, destination: &Path) -> Result<(), ArchiveError> {
    if destination.exists() {
        return Err(ArchiveError::DestinationExists(destination.to_path_buf()));
    }
    fs::rename(staging, destination)?;
    Ok(())
}

pub(crate) fn resolve_source(
    path: &Path,
    rel_path: Option<&Path>,
    project: Option<&Path>,
) -> Option<PathBuf> {
    rel_path
        .filter(|relative| {
            relative
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_)))
        })
        .and_then(|relative| project.and_then(Path::parent).map(|dir| dir.join(relative)))
        .filter(|candidate| candidate.is_file())
        .or_else(|| path.is_file().then(|| path.to_path_buf()))
}

/// Resolve project-relative paths before creating a video engine. Prefer a
/// local copy when present, even if the original machine's absolute path also
/// exists. Never follow `..` or rooted paths outside the project directory.
pub fn resolve_project_relative_assets(document: &mut Document, project_file: &Path) {
    let Some(project_dir) = project_file.parent() else {
        return;
    };
    let Some(project) = document.timeline.as_mut() else {
        return;
    };
    for asset in project.media.assets.values_mut() {
        let AssetSource::File { path, rel_path } = &mut asset.source else {
            continue;
        };
        let Some(relative) = rel_path.as_ref() else {
            continue;
        };
        if !relative
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
        {
            continue;
        }
        let local = project_dir.join(relative);
        if local.is_file() {
            *path = local;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::timeline::{
        AssetKind, FrameRate, MediaAsset, ReferenceStill, Sequence, Tick, TimelineProject,
    };

    fn fixture() -> (PathBuf, Document, PathBuf) {
        let root = std::env::temp_dir().join(format!("photonic-archive-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let media = root.join("source.mov");
        fs::write(&media, b"frame bytes").unwrap();
        let lut = root.join("look.cube");
        fs::write(&lut, b"LUT_3D_SIZE 2\n").unwrap();
        let mut doc = Document::new("archive", 16.0, 16.0);
        let mut project = TimelineProject::new();
        project
            .media
            .insert(MediaAsset::from_file(AssetKind::Video, &media));
        project
            .media
            .insert(MediaAsset::from_file(AssetKind::Lut3d, &lut));
        doc.timeline = Some(project);
        (root, doc, media)
    }

    #[test]
    fn archive_collects_and_reopens_after_move() {
        let (root, doc, original_media) = fixture();
        let destination = root.join("deliverable");
        let project_file = archive_project(&doc, None, &destination).unwrap();
        let (mut loaded, history) =
            photonic_core::load_photon(&fs::read_to_string(&project_file).unwrap()).unwrap();
        assert!(history.is_none());
        fs::remove_file(original_media).unwrap();
        let moved = root.join("moved");
        fs::rename(&destination, &moved).unwrap();
        let moved_project = moved.join("deliverable.photon");
        resolve_project_relative_assets(&mut loaded, &moved_project);
        for asset in loaded.timeline.unwrap().media.assets.into_values() {
            let AssetSource::File { path, rel_path } = asset.source else {
                panic!("expected file-backed asset");
            };
            assert!(path.is_file());
            assert!(path.starts_with(&moved));
            assert!(rel_path.unwrap().starts_with("media"));
            assert!(asset.proxy.is_none());
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn offline_asset_and_existing_destination_are_rejected() {
        let (root, mut doc, media) = fixture();
        let destination = root.join("deliverable");
        fs::remove_file(media).unwrap();
        assert!(matches!(
            archive_project(&doc, None, &destination),
            Err(ArchiveError::Offline { .. })
        ));
        assert!(!destination.exists());
        fs::create_dir(&destination).unwrap();
        assert!(matches!(
            archive_project(&doc, None, &destination),
            Err(ArchiveError::DestinationExists(_))
        ));
        let asset = doc
            .timeline
            .as_mut()
            .unwrap()
            .media
            .assets
            .values_mut()
            .next()
            .unwrap();
        asset.source = AssetSource::File {
            path: PathBuf::from("missing"),
            rel_path: Some(PathBuf::from("../escape.mov")),
        };
        resolve_project_relative_assets(&mut doc, &destination.join("project.photon"));
        assert!(doc.timeline.unwrap().media.assets.values().any(|asset| {
            matches!(&asset.source, AssetSource::File { path, .. } if path == Path::new("missing"))
        }));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn changed_reference_still_aborts_without_publishing() {
        let (root, mut doc, _) = fixture();
        let still_path = root.join("still.png");
        fs::write(&still_path, b"original still").unwrap();
        let mut asset = MediaAsset::from_file(AssetKind::Image, root.join("old-still.png"));
        asset.source = AssetSource::File {
            path: root.join("old-still.png"),
            rel_path: Some(PathBuf::from("still.png")),
        };
        let image_asset = asset.id;
        let project = doc.timeline.as_mut().unwrap();
        project.media.insert(asset);
        let mut sequence = Sequence::new("cut", FrameRate::FPS_24, 16, 16);
        sequence.reference_stills.push(ReferenceStill {
            id: uuid::Uuid::new_v4(),
            name: "balance".into(),
            image_asset,
            image_hash: crate::media::full_content_hash(&still_path).unwrap(),
            source_clip: None,
            source_time: Tick::ZERO,
            grade_revision: 1,
            color: sequence.color.clone(),
            format_index: 0,
        });
        project.insert_sequence(sequence);
        fs::write(&still_path, b"changed still").unwrap();
        let destination = root.join("deliverable");
        let project_file = root.join("project.photon");
        assert!(matches!(
            archive_project(&doc, Some(&project_file), &destination),
            Err(ArchiveError::ReferenceChanged { .. })
        ));
        assert!(!destination.exists());
        fs::remove_dir_all(root).unwrap();
    }
}
