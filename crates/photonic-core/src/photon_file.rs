//! `.photon` file container.
//!
//! Historically a `.photon` file was a `Document` serialized as JSON. To let a
//! project's full edit history (undo/redo, named checkpoints, branches) travel
//! with the file, the history is now written **alongside** the document as two
//! extra sibling keys rather than wrapping it:
//!
//! ```json
//! { "<all the document fields…>": …, "photon_format": 1, "photon_history": { … } }
//! ```
//!
//! The document stays the top-level object, which buys real backward AND forward
//! compatibility:
//! - **Older Photonic builds** (which read a `.photon` as a bare `Document`) open
//!   new-format files unchanged — `serde` ignores the two unknown keys. Saving
//!   is therefore *not* a one-way door.
//! - **History is best-effort and version-gated**: it is restored only when the
//!   document's own `format_version` matches this build's. A file written by an
//!   older/newer schema keeps opening (its document migrates as usual) but its
//!   embedded history is dropped rather than risk loading un-migrated nested
//!   documents. A malformed history payload is likewise dropped, never fatal.

use serde::Serialize;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::document::{Document, CURRENT_FORMAT_VERSION};
use crate::history::HistorySnapshot;

/// Version of the `.photon` history-container format (independent of the
/// document's own `format_version`). Bump only on breaking changes to how the
/// `photon_history` payload is laid out.
pub const PHOTON_FORMAT_VERSION: u32 = 1;

/// Canonical extension for native Photonic documents.
pub const PHOTON_FILE_EXTENSION: &str = "photon";

static ATOMIC_WRITE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write bytes to `path` without exposing a partially written destination.
///
/// The complete payload is written and synced to a uniquely named sibling before
/// the sibling replaces the destination. A failed write or replacement leaves an
/// existing destination untouched and removes the staging file.
pub fn write_atomic_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_atomic_file_with_replacer(path, bytes, None, replace_destination)
}

/// Write bytes atomically while applying a Unix permission mode to the staged
/// file before it replaces the destination.
///
/// On non-Unix platforms, access is governed by the platform's ACLs and
/// `mode` is ignored.
pub fn write_atomic_file_with_mode(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    write_atomic_file_with_replacer(path, bytes, Some(mode), replace_destination)
}

fn write_atomic_file_with_replacer<F>(
    path: &Path,
    bytes: &[u8],
    mode: Option<u32>,
    replace: F,
) -> io::Result<()>
where
    F: FnOnce(&Path, &Path) -> io::Result<()>,
{
    write_atomic_file_with_hooks(
        path,
        bytes,
        mode,
        |file, bytes| file.write_all(bytes),
        replace,
    )
}

fn write_atomic_file_with_hooks<W, F>(
    path: &Path,
    bytes: &[u8],
    mode: Option<u32>,
    write: W,
    replace: F,
) -> io::Result<()>
where
    W: FnOnce(&mut File, &[u8]) -> io::Result<()>,
    F: FnOnce(&Path, &Path) -> io::Result<()>,
{
    let staging = staging_path(path)?;
    let mut staging_created = false;
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);

        #[cfg(unix)]
        if let Some(mode) = mode {
            use std::os::unix::fs::OpenOptionsExt;

            options.mode(mode);
        }
        #[cfg(not(unix))]
        let _ = mode;

        let mut file = options.open(&staging)?;
        staging_created = true;

        #[cfg(unix)]
        if let Some(mode) = mode {
            use std::os::unix::fs::PermissionsExt;

            file.set_permissions(fs::Permissions::from_mode(mode))?;
        }

        write(&mut file, bytes)?;
        file.flush()?;
        file.sync_all()?;
        replace(&staging, path)
    })();

    if staging_created {
        let _ = fs::remove_file(&staging);
    }
    result
}

fn staging_path(path: &Path) -> io::Result<PathBuf> {
    use std::ffi::OsString;

    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "atomic file writes require a destination file name",
        )
    })?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let serial = ATOMIC_WRITE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut name = OsString::from(".");
    name.push(file_name);
    name.push(format!(".photonic-tmp-{}-{serial}", std::process::id()));
    Ok(parent.join(name))
}

#[cfg(not(windows))]
fn replace_destination(staging: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(staging, destination)
}

#[cfg(windows)]
fn replace_destination(staging: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x0000_0001;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    let staging = wide(staging);
    let destination = wide(destination);
    unsafe {
        if ReplaceFileW(
            destination.as_ptr(),
            staging.as_ptr(),
            ptr::null(),
            0,
            ptr::null_mut(),
            ptr::null_mut(),
        ) != 0
        {
            return Ok(());
        }

        let replace_error = io::Error::last_os_error();
        // `ReplaceFileW` is preferred because it preserves replacement
        // metadata, but it can reject valid writable files on hosted Windows
        // runners. `MoveFileExW` is the supported replacement fallback for
        // both existing and newly-created destinations.
        if MoveFileExW(
            staging.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        ) != 0
        {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::Other,
                format!(
                    "could not replace destination ({replace_error}); fallback failed: {}",
                    io::Error::last_os_error()
                ),
            ))
        }
    }
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn MoveFileExW(existing_file_name: *const u16, new_file_name: *const u16, flags: u32) -> i32;
    fn ReplaceFileW(
        replaced_file_name: *const u16,
        replacement_file_name: *const u16,
        backup_file_name: *const u16,
        replace_flags: u32,
        exclude: *mut std::ffi::c_void,
        reserved: *mut std::ffi::c_void,
    ) -> i32;
}

/// Serialize a document plus its history snapshot to a pretty-printed `.photon`
/// JSON string. Pass `None` for `history` to write a document-only file (the
/// `photon_history` key is then omitted entirely).
pub fn save_photon(
    document: &Document,
    history: Option<&HistorySnapshot>,
) -> Result<String, serde_json::Error> {
    // Flatten the document's fields to the top level and append our two sibling
    // keys. Borrowing avoids cloning the (potentially large) document/history.
    #[derive(Serialize)]
    struct PhotonOut<'a> {
        #[serde(flatten)]
        document: &'a Document,
        photon_format: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        photon_history: Option<&'a HistorySnapshot>,
    }
    serde_json::to_string_pretty(&PhotonOut {
        document,
        photon_format: PHOTON_FORMAT_VERSION,
        photon_history: history,
    })
}

/// Parse a `.photon` JSON string into a migrated [`Document`] and, when present
/// and schema-compatible, its [`HistorySnapshot`].
///
/// Accepts both the new format (document fields + `photon_history`) and legacy
/// bare-`Document` files. A document that fails to parse is a hard error; the
/// history is always best-effort — a missing, malformed, or schema-mismatched
/// payload yields `None` history while the document still opens.
pub fn load_photon(json: &str) -> Result<(Document, Option<HistorySnapshot>), serde_json::Error> {
    let mut value: serde_json::Value = serde_json::from_str(json)?;

    // Lift the two sibling keys out before the value is consumed by document
    // migration. (Document deserialization ignores unknown keys regardless, but
    // removing them keeps the migrated tree clean.)
    let (history_value, photon_format) = match value.as_object_mut() {
        Some(obj) => (
            obj.remove("photon_history"),
            obj.remove("photon_format").and_then(|v| v.as_u64()),
        ),
        None => (None, None),
    };

    // The document's own format version, read BEFORE migration. The embedded
    // history was written against this same version, so it is only safe to
    // restore when that version matches what this build serializes today.
    let doc_version = crate::migration::detect_version(&value);

    let document = Document::from_value(value)?;

    let history = if doc_version == CURRENT_FORMAT_VERSION
        && photon_format.is_none_or(|f| f <= PHOTON_FORMAT_VERSION as u64)
    {
        history_value
            .and_then(|h| match h {
                serde_json::Value::Null => None,
                v => serde_json::from_value::<HistorySnapshot>(v).ok(),
            })
            .map(|mut s| {
                s.normalize_nested();
                s
            })
    } else {
        None
    };

    Ok((document, history))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_doc() -> Document {
        Document::new("doc", 64.0, 64.0)
    }

    #[test]
    fn legacy_bare_document_still_loads() {
        // A pre-history file is just the document's own JSON.
        let bare = sample_doc().to_json().unwrap();
        let (doc, history) = load_photon(&bare).unwrap();
        assert_eq!(doc.name, "doc");
        assert!(history.is_none(), "legacy file must yield no history");
    }

    #[test]
    fn new_format_round_trips_document_and_history() {
        let doc = sample_doc();
        let mut snap = HistorySnapshot::default();
        snap.branches.insert("main".into(), sample_doc());
        let json = save_photon(&doc, Some(&snap)).unwrap();

        // New-format files carry the two sibling keys…
        assert!(json.contains("photon_format"));
        assert!(json.contains("photon_history"));
        // …with the document still flattened at the top level (back-compat).
        assert!(json.contains("\"name\""));

        let (rdoc, rhist) = load_photon(&json).unwrap();
        assert_eq!(rdoc.name, "doc");
        let rhist = rhist.expect("history should round-trip at the current version");
        assert!(rhist.branches.contains_key("main"));
    }

    #[test]
    fn new_format_opens_as_bare_document_in_old_loader() {
        // Simulates an older build / the thumbnailer: from_json must still parse
        // a new-format file, ignoring the extra keys.
        let json = save_photon(&sample_doc(), Some(&HistorySnapshot::default())).unwrap();
        let doc = Document::from_json(&json).expect("old loader must accept new format");
        assert_eq!(doc.name, "doc");
    }

    #[test]
    fn document_only_save_omits_history_key() {
        let json = save_photon(&sample_doc(), None).unwrap();
        assert!(
            !json.contains("photon_history"),
            "None history must be omitted"
        );
        let (_, hist) = load_photon(&json).unwrap();
        assert!(hist.is_none());
    }

    #[test]
    fn malformed_history_degrades_but_document_opens() {
        let mut v = serde_json::to_value(sample_doc()).unwrap();
        let obj = v.as_object_mut().unwrap();
        obj.insert(
            "photon_format".into(),
            serde_json::json!(PHOTON_FORMAT_VERSION),
        );
        obj.insert(
            "photon_history".into(),
            serde_json::json!("not-a-history-object"),
        );
        let (doc, hist) = load_photon(&serde_json::to_string(&v).unwrap()).unwrap();
        assert_eq!(doc.name, "doc");
        assert!(
            hist.is_none(),
            "malformed history must not block the document"
        );
    }

    #[test]
    fn v3_document_migrates_but_drops_embedded_history() {
        let mut value = serde_json::to_value(sample_doc()).unwrap();
        let obj = value.as_object_mut().unwrap();
        obj.insert("format_version".into(), serde_json::json!(3));
        obj.insert(
            "photon_format".into(),
            serde_json::json!(PHOTON_FORMAT_VERSION),
        );
        obj.insert(
            "photon_history".into(),
            serde_json::to_value(HistorySnapshot::default()).unwrap(),
        );

        let (doc, history) = load_photon(&serde_json::to_string(&value).unwrap()).unwrap();
        assert_eq!(doc.format_version, CURRENT_FORMAT_VERSION);
        assert!(
            history.is_none(),
            "v3 history contains unmigrated documents"
        );
        assert_eq!(
            PHOTON_FORMAT_VERSION, 1,
            "document migration is independent"
        );
    }

    #[test]
    fn malformed_raster_image_is_rejected_before_open() {
        let mut doc = sample_doc();
        let layer_id = doc.layer_order[0];
        let node = crate::node::SceneNode::new(
            "image",
            layer_id,
            crate::node::SceneNodeKind::Raster(crate::node::RasterNode::new(
                crate::raster::RasterImage::filled(1, 1, [1, 2, 3, 255]),
            )),
        );
        doc.add_node(node, Some(layer_id));

        let mut value = serde_json::to_value(&doc).unwrap();
        let node = value["nodes"]
            .as_object_mut()
            .unwrap()
            .values_mut()
            .next()
            .unwrap();
        node["kind"]["image"]["width"] = serde_json::json!(2);

        let err = load_photon(&serde_json::to_string(&value).unwrap()).unwrap_err();
        assert!(err.to_string().contains("do not match decoded dimensions"));

        let mut value = serde_json::to_value(&doc).unwrap();
        let node = value["nodes"]
            .as_object_mut()
            .unwrap()
            .values_mut()
            .next()
            .unwrap();
        node["kind"]["mask"] = serde_json::json!({
            "width": 0,
            "height": 1,
            "data": []
        });

        let err = load_photon(&serde_json::to_string(&value).unwrap()).unwrap_err();
        assert!(err.to_string().contains("mask dimensions must be non-zero"));
    }

    fn test_dir(label: &str) -> PathBuf {
        let serial = ATOMIC_WRITE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "photonic-photon-file-{label}-{}-{serial}",
            std::process::id()
        ));
        fs::create_dir(&dir).unwrap();
        dir
    }

    fn staging_files(dir: &Path) -> Vec<PathBuf> {
        fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().contains(".photonic-tmp-"))
            })
            .collect()
    }

    #[test]
    fn atomic_write_replaces_with_exact_bytes_and_leaves_no_staging_file() {
        let dir = test_dir("success");
        let path = dir.join("project.photon");
        let bytes = b"fully serialized photon bytes\n\0";

        write_atomic_file(&path, bytes).unwrap();

        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(staging_files(&dir).is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn injected_replace_failure_preserves_destination_and_cleans_staging() {
        let dir = test_dir("injected-failure");
        let path = dir.join("project.photon");
        let original = b"old project bytes";
        fs::write(&path, original).unwrap();

        let error = write_atomic_file_with_replacer(
            &path,
            b"new project bytes",
            Some(0o600),
            |_staging, _| {
                Err(io::Error::new(
                    io::ErrorKind::Other,
                    "injected replacement failure",
                ))
            },
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(fs::read(&path).unwrap(), original);
        assert!(staging_files(&dir).is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn injected_write_failure_preserves_destination_and_cleans_staging() {
        let dir = test_dir("write-failure");
        let path = dir.join("project.photon");
        let original = b"old project bytes";
        fs::write(&path, original).unwrap();

        let error = write_atomic_file_with_hooks(
            &path,
            b"new project bytes",
            Some(0o600),
            |_file, _bytes| {
                Err(io::Error::new(
                    io::ErrorKind::Other,
                    "injected write failure",
                ))
            },
            |_staging, _| panic!("replacement must not run after a write failure"),
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(fs::read(&path).unwrap(), original);
        assert!(staging_files(&dir).is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_with_mode_sets_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = test_dir("private");
        let path = dir.join("claude.json");

        write_atomic_file_with_mode(&path, b"private settings", 0o600).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        write_atomic_file_with_mode(&path, b"updated private settings", 0o600).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(fs::read(&path).unwrap(), b"updated private settings");
        assert!(staging_files(&dir).is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn structural_replace_failure_preserves_existing_destination_and_cleans_staging() {
        let dir = test_dir("structural-failure");
        let path = dir.join("project.photon");
        fs::create_dir(&path).unwrap();

        assert!(write_atomic_file(&path, b"cannot replace a directory").is_err());
        assert!(path.is_dir());
        assert!(staging_files(&dir).is_empty());
        fs::remove_dir_all(dir).unwrap();
    }
}
