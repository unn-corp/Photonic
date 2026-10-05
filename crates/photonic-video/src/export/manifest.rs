//! Optional reproducibility record for a completed export. Full-byte hashes
//! describe the project pool before/after rendering and the published output;
//! they are integrity identifiers, not signatures or continuous-write locks.
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use photonic_core::timeline::{AssetId, AssetSource, SequenceId, TimelineProject};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{job::ResolvedExportJob, render_loop::ExportError};
use crate::{
    media::ffmpeg_locate::FfmpegTools,
    session::{ExportJob, RenderSnapshot},
};

const SCHEMA: &str = "photonic.render.v1";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ManifestSource {
    pub asset: AssetId,
    pub path: PathBuf,
    pub full_hash: Option<String>,
    pub status: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RenderManifest {
    pub schema: String,
    pub application_version: String,
    pub revision: Option<u64>,
    pub timeline_hash: String,
    pub timeline_snapshot: TimelineProject,
    pub embedded_document: Option<photonic_core::Document>,
    pub embedded_document_hash: Option<String>,
    pub sequence: SequenceId,
    pub request: Value,
    /// All file-backed project-pool assets, including unused/offline entries.
    pub project_pool_sources: Vec<ManifestSource>,
    pub source_verification: String,
    pub toolchain: Value,
    /// File name relative to this manifest, never an arbitrary read path.
    pub output_file: PathBuf,
    pub output_full_hash: String,
}

pub fn manifest_path(output: &Path) -> PathBuf {
    let mut path = output.as_os_str().to_os_string();
    path.push(".photonic-render.json");
    PathBuf::from(path)
}

fn failure(error: impl std::fmt::Display) -> ExportError {
    ExportError::Resolve(format!("render manifest: {error}"))
}

fn full_hash(path: &Path, cancel: &AtomicBool) -> Result<String, ExportError> {
    let mut file = std::fs::File::open(path).map_err(failure)?;
    let mut hasher = xxhash_rust::xxh3::Xxh3::new();
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(super::encoder::EncodeError::Cancelled.into());
        }
        let count = file.read(&mut buffer).map_err(failure)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:032x}", hasher.digest128()))
}

fn canonical_hash(value: Value) -> Result<String, ExportError> {
    fn ordered(value: Value) -> Value {
        match value {
            Value::Object(map) => {
                let sorted: std::collections::BTreeMap<_, _> = map.into_iter().collect();
                Value::Object(
                    sorted
                        .into_iter()
                        .map(|(key, value)| (key, ordered(value)))
                        .collect(),
                )
            }
            Value::Array(values) => Value::Array(values.into_iter().map(ordered).collect()),
            other => other,
        }
    }
    let bytes = serde_json::to_vec(&ordered(value)).map_err(failure)?;
    Ok(crate::media::full_content_hash_bytes(&bytes))
}

fn source_inventory(
    project: &TimelineProject,
    cancel: &AtomicBool,
) -> Result<Vec<ManifestSource>, ExportError> {
    let mut sources = Vec::new();
    for asset in project.media.assets.values() {
        let AssetSource::File { path, .. } = &asset.source else {
            continue;
        };
        if cancel.load(Ordering::Relaxed) {
            return Err(super::encoder::EncodeError::Cancelled.into());
        }
        let (hash, status) = if !path.exists() {
            (None, "missing")
        } else {
            match full_hash(path, cancel) {
                Ok(hash) => (Some(hash), "available"),
                Err(error) if cancel.load(Ordering::Relaxed) => return Err(error),
                Err(_) => (None, "unreadable"),
            }
        };
        sources.push(ManifestSource {
            asset: asset.id,
            path: path.clone(),
            full_hash: hash,
            status: status.into(),
        });
    }
    sources.sort_by_key(|source| source.asset);
    Ok(sources)
}

fn check_existing_sidecar(output: &Path) -> Result<(), ExportError> {
    let path = manifest_path(output);
    match std::fs::read(&path) {
        Ok(bytes) => {
            let previous: RenderManifest = serde_json::from_slice(&bytes).map_err(failure)?;
            if previous.schema != SCHEMA
                || Some(previous.output_file.as_os_str()) != output.file_name()
            {
                return Err(failure(
                    "destination sidecar belongs to another format or output",
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(failure(error)),
    }
    Ok(())
}

pub(crate) fn capture(
    project: &TimelineProject,
    snapshot: Option<&RenderSnapshot>,
    job: &ExportJob,
    resolved: &ResolvedExportJob,
    tools: &FfmpegTools,
    cancel: &AtomicBool,
) -> Result<RenderManifest, ExportError> {
    check_existing_sidecar(&job.output)?;
    let output_file = job
        .output
        .file_name()
        .ok_or_else(|| failure("output has no file name"))?
        .into();
    let version = std::process::Command::new(&tools.ffmpeg)
        .arg("-version")
        .output()
        .map_err(failure)?;
    if !version.status.success() {
        return Err(failure("cannot identify FFmpeg version"));
    }
    let ffmpeg_version = String::from_utf8_lossy(&version.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned();
    Ok(RenderManifest {
        schema: SCHEMA.into(),
        application_version: env!("CARGO_PKG_VERSION").into(),
        revision: snapshot.map(|snapshot| snapshot.revision),
        timeline_hash: canonical_hash(serde_json::to_value(project).map_err(failure)?)?,
        timeline_snapshot: project.clone(),
        embedded_document: snapshot
            .and_then(|snapshot| snapshot.document.as_ref())
            .map(|document| document.as_ref().clone()),
        embedded_document_hash: snapshot
            .and_then(|snapshot| snapshot.document.as_ref())
            .map(|document| {
                serde_json::to_value(document.as_ref())
                    .map_err(failure)
                    .and_then(canonical_hash)
            })
            .transpose()?,
        sequence: job.sequence,
        request: json!({
            "sequence_color": project.sequences.get(&job.sequence).ok_or_else(|| failure("sequence no longer exists in frozen project"))?.color,
            "format_index": resolved.format_index, "format_size": resolved.format_size,
            "output_size": resolved.out_size, "sequence_rate": resolved.seq_rate,
            "output_rate": resolved.out_rate, "start_tick": resolved.start.0,
            "end_tick": resolved.end.0, "total_frames": resolved.total_frames,
            "preset": resolved.preset, "options": job.options,
        }),
        project_pool_sources: source_inventory(project, cancel)?,
        source_verification: "full_bytes_before_and_after_render_project_pool".into(),
        toolchain: json!({"ffmpeg_version": ffmpeg_version, "ffmpeg_full_hash": full_hash(&tools.ffmpeg, cancel)?, "ffprobe_full_hash": full_hash(&tools.ffprobe, cancel)?}),
        output_file,
        output_full_hash: String::new(),
    })
}

pub(crate) fn publish(
    mut manifest: RenderManifest,
    project: &TimelineProject,
    output: &Path,
    cancel: &AtomicBool,
) -> Result<(), ExportError> {
    if source_inventory(project, cancel)? != manifest.project_pool_sources {
        return Err(failure("project-pool sources changed during render; output was encoded but no new manifest was published"));
    }
    check_existing_sidecar(output)?;
    manifest.output_full_hash = full_hash(output, cancel)?;
    let bytes = serde_json::to_vec_pretty(&manifest).map_err(failure)?;
    photonic_core::write_atomic_file(&manifest_path(output), &bytes).map_err(failure)
}

/// Read a sidecar and validate the recorded snapshot identities. Output bytes
/// are verified separately so hosts can apply their path policy first.
pub fn read_manifest(path: &Path) -> Result<RenderManifest, ExportError> {
    let manifest: RenderManifest =
        serde_json::from_slice(&std::fs::read(path).map_err(failure)?).map_err(failure)?;
    if manifest.schema != SCHEMA
        || manifest.output_file.components().count() != 1
        || !matches!(
            manifest.output_file.components().next(),
            Some(std::path::Component::Normal(_))
        )
    {
        return Err(failure("unknown schema or invalid adjacent output name"));
    }
    if canonical_hash(serde_json::to_value(&manifest.timeline_snapshot).map_err(failure)?)?
        != manifest.timeline_hash
    {
        return Err(failure("timeline snapshot identity does not match"));
    }
    let document_hash = manifest
        .embedded_document
        .as_ref()
        .map(|document| {
            serde_json::to_value(document)
                .map_err(failure)
                .and_then(canonical_hash)
        })
        .transpose()?;
    if document_hash != manifest.embedded_document_hash {
        return Err(failure("embedded document identity does not match"));
    }
    Ok(manifest)
}

/// Verify the adjacent output against a previously read, immutable manifest.
pub fn verify_manifest_output(manifest: &RenderManifest, path: &Path) -> Result<(), ExportError> {
    if manifest.output_file.components().count() != 1
        || !matches!(
            manifest.output_file.components().next(),
            Some(std::path::Component::Normal(_))
        )
    {
        return Err(failure("invalid adjacent output name"));
    }
    let output = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(&manifest.output_file);
    if full_hash(&output, &AtomicBool::new(false))? != manifest.output_full_hash {
        return Err(failure("output bytes no longer match this manifest"));
    }
    Ok(())
}

/// Validate a sidecar's snapshot and adjacent output's full-file identity.
pub fn verify_manifest(path: &Path) -> Result<RenderManifest, ExportError> {
    let manifest = read_manifest(path)?;
    verify_manifest_output(&manifest, path)?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::timeline::{AssetKind, MediaAsset};

    fn fixture(root: &Path, project: &TimelineProject) -> RenderManifest {
        RenderManifest {
            schema: SCHEMA.into(),
            application_version: "test".into(),
            revision: Some(7),
            timeline_hash: canonical_hash(serde_json::to_value(project).unwrap()).unwrap(),
            timeline_snapshot: project.clone(),
            embedded_document: None,
            embedded_document_hash: None,
            sequence: SequenceId::new(),
            request: json!({}),
            project_pool_sources: source_inventory(project, &AtomicBool::new(false)).unwrap(),
            source_verification: "full_bytes_before_and_after_render_project_pool".into(),
            toolchain: json!({}),
            output_file: root.join("delivery.mov").file_name().unwrap().into(),
            output_full_hash: String::new(),
        }
    }

    #[test]
    fn published_manifest_tracks_full_output_and_rejects_changed_source_or_foreign_sidecar() {
        let root = std::env::temp_dir().join(format!("photonic-manifest-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.mov");
        let output = root.join("delivery.mov");
        std::fs::write(&source, b"original media").unwrap();
        std::fs::write(&output, vec![5_u8; 256 * 1024]).unwrap();
        let mut project = TimelineProject::new();
        project
            .media
            .insert(MediaAsset::from_file(AssetKind::Video, &source));
        let original = fixture(&root, &project);
        let cancel = AtomicBool::new(false);
        publish(original.clone(), &project, &output, &cancel).unwrap();
        let path = manifest_path(&output);
        let recorded = verify_manifest(&path).unwrap();
        assert_eq!(recorded.revision, Some(7));
        assert_eq!(
            recorded.output_full_hash,
            crate::media::full_content_hash(&output).unwrap()
        );
        let previous = std::fs::read(&path).unwrap();
        std::fs::write(&source, b"different media").unwrap();
        assert!(publish(original, &project, &output, &cancel).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), previous);
        let mut altered_output = std::fs::read(&output).unwrap();
        altered_output[128 * 1024] = 9;
        std::fs::write(&output, altered_output).unwrap();
        assert!(verify_manifest(&path).is_err());
        std::fs::write(&path, b"unrelated user file").unwrap();
        assert!(publish(fixture(&root, &project), &project, &output, &cancel).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"unrelated user file");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn manifest_hash_is_independent_of_object_order_and_hashing_is_cancellable() {
        let first: Value = serde_json::from_str(r#"{"b":{"z":2,"a":1},"a":[1,2]}"#).unwrap();
        let second: Value = serde_json::from_str(r#"{"a":[1,2],"b":{"a":1,"z":2}}"#).unwrap();
        assert_eq!(
            canonical_hash(first).unwrap(),
            canonical_hash(second).unwrap()
        );
        let path =
            std::env::temp_dir().join(format!("photonic-manifest-cancel-{}", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"bytes").unwrap();
        assert!(matches!(
            full_hash(&path, &AtomicBool::new(true)),
            Err(ExportError::Encode(
                super::super::encoder::EncodeError::Cancelled
            ))
        ));
        std::fs::remove_file(path).unwrap();
    }
}
