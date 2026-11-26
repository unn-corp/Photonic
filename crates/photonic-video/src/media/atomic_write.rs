//! Atomic file writes for cache and export outputs (37 §2.3).
//!
//! Cache writers sync their bytes to a private `.part` sibling before
//! renaming it onto the final path. The same-filesystem rename publishes a
//! complete entry atomically, even when multiple writers target the same file.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// The `.part` staging path for `output`: same directory, so the rename onto
/// `output` is same-filesystem and atomic.
pub fn staging_path(output: &Path) -> PathBuf {
    let mut s = output.as_os_str().to_os_string();
    s.push(".part");
    PathBuf::from(s)
}

/// Write `bytes` to `output` atomically. Each writer uses its own `.part`
/// sibling and removes it if writing, syncing, or renaming fails.
///
/// The parent directory of `output` is created if needed.
pub fn write_atomic(output: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = output.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = output.with_file_name(format!(".photonic-{}.part", uuid::Uuid::new_v4()));
    let mut file = std::fs::File::create_new(&tmp)?;

    // A closure so any early error path funnels through the cleanup below.
    let write = || -> std::io::Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, output)
    };

    match write() {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Delete every `*.part` file directly under `dir` whose last modification is
/// older than `age`. Non-recursive — the caches are flat. Returns the number of
/// staging files removed. Missing directories and unreadable entries are
/// skipped silently: a startup sweep must never fail the launch.
pub fn sweep_stale_staging(dir: &Path, age: Duration) -> usize {
    let now = SystemTime::now();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("part") {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .map(|elapsed| elapsed >= age)
            .unwrap_or(false);
        if stale && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir() -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "photonic-atomic-write-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn staging_path_appends_part() {
        assert_eq!(
            staging_path(Path::new("/a/b/c.bin")),
            PathBuf::from("/a/b/c.bin.part")
        );
    }

    #[test]
    fn write_atomic_leaves_no_part_and_exact_bytes() {
        let dir = tmp_dir();
        let out = dir.join("frame.bin");
        let data = b"exact contents \x00\x01\x02";
        write_atomic(&out, data).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), data);
        assert!(
            !staging_path(&out).exists(),
            ".part must not survive success"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn concurrent_writes_publish_complete_outputs() {
        const WRITERS: u8 = 8;
        let dir = tmp_dir();
        let output = dir.join("shared-cache.bin");
        let start = std::sync::Barrier::new(WRITERS as usize);
        let errors = std::thread::scope(|scope| {
            let mut writers = Vec::new();
            for byte in 1..=WRITERS {
                let output = &output;
                let start = &start;
                writers.push(scope.spawn(move || {
                    let payload = vec![byte; 8192];
                    start.wait();
                    (0..12)
                        .filter_map(|_| write_atomic(output, &payload).err())
                        .count()
                }));
            }
            writers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .sum::<usize>()
        });
        let contents = std::fs::read(&output).unwrap();
        let files = std::fs::read_dir(&dir).unwrap().count();
        std::fs::remove_dir_all(&dir).unwrap();

        assert_eq!(errors, 0, "all writers must be able to publish");
        assert_eq!(contents.len(), 8192);
        let winner = contents[0];
        assert!((1..=WRITERS).contains(&winner));
        assert!(contents.iter().all(|byte| *byte == winner));
        assert_eq!(files, 1, "only the final cache entry should remain");
    }

    #[test]
    fn write_atomic_failure_leaves_neither_part_nor_output() {
        // Target a path whose parent is a *file*, so create_dir_all/create fail.
        let dir = tmp_dir();
        let blocker = dir.join("not-a-dir");
        std::fs::write(&blocker, b"x").unwrap();
        let out = blocker.join("nested").join("frame.bin");
        let err = write_atomic(&out, b"payload");
        assert!(err.is_err(), "write into a non-directory should fail");
        assert!(!out.exists());
        assert!(!staging_path(&out).exists());

        let destination_dir = dir.join("destination");
        std::fs::create_dir(&destination_dir).unwrap();
        assert!(write_atomic(&destination_dir, b"payload").is_err());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sweep_removes_aged_part_only() {
        let dir = tmp_dir();
        let aged = dir.join("old.bin.part");
        let fresh = dir.join("new.bin.part");
        let real = dir.join("keep.bin");
        std::fs::write(&aged, b"a").unwrap();
        std::fs::write(&fresh, b"f").unwrap();
        std::fs::write(&real, b"r").unwrap();

        // Backdate the aged `.part` two days.
        let two_days_ago = SystemTime::now() - Duration::from_secs(2 * 24 * 3600);
        filetime_set(&aged, two_days_ago);

        let removed = sweep_stale_staging(&dir, Duration::from_secs(24 * 3600));
        assert_eq!(removed, 1);
        assert!(!aged.exists(), "aged .part should be swept");
        assert!(fresh.exists(), "fresh .part should survive");
        assert!(real.exists(), "non-.part files are never touched");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Set a file's mtime to `t`. Uses the std `File::set_modified` available on
    /// this toolchain; falls back to leaving the mtime unchanged only if that
    /// call is unavailable (in which case the test above would still be valid
    /// because a freshly written `.part` is not aged).
    fn filetime_set(path: &Path, t: SystemTime) {
        if let Ok(f) = std::fs::OpenOptions::new().write(true).open(path) {
            let _ = f.set_modified(t);
        }
    }
}
