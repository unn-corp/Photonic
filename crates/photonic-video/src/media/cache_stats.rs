//! K-C5 cache-data summary — sizes of the project sidecar cache categories.
//!
//! Pure filesystem walk over `<project>.photon.cache/` (and the global proxy
//! fallback). Used by the media-pool cache pane; no deletion here.

use std::path::{Path, PathBuf};

use super::{cache_dir_for_project, proxy_cache_dir};

/// One cache category with total bytes and file count.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CacheCategory {
    pub name: &'static str,
    pub bytes: u64,
    pub files: u64,
}

/// Full report for a project (or global caches when `project_path` is `None`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CacheReport {
    pub root: PathBuf,
    pub categories: Vec<CacheCategory>,
    pub total_bytes: u64,
}

impl CacheReport {
    pub fn total_mb(&self) -> f64 {
        self.total_bytes as f64 / (1024.0 * 1024.0)
    }
}

/// Scan known cache filename suffixes under the project sidecar dir.
pub fn summarize_cache(project_path: Option<&Path>) -> CacheReport {
    let root = match project_path {
        Some(p) => cache_dir_for_project(p),
        None => proxy_cache_dir(None),
    };
    let mut categories = vec![
        CacheCategory {
            name: "proxies",
            bytes: 0,
            files: 0,
        },
        CacheCategory {
            name: "posters",
            bytes: 0,
            files: 0,
        },
        CacheCategory {
            name: "keyframes",
            bytes: 0,
            files: 0,
        },
        CacheCategory {
            name: "waveforms",
            bytes: 0,
            files: 0,
        },
        CacheCategory {
            name: "other",
            bytes: 0,
            files: 0,
        },
    ];
    if !root.is_dir() {
        return CacheReport {
            root,
            categories,
            total_bytes: 0,
        };
    }
    let mut total = 0u64;
    for (path, size) in walkdir_shallow(&root) {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let idx = if name.contains("proxy") || name.ends_with(".proxy.mp4") {
            0
        } else if name.contains("poster") || name.ends_with(".poster.png") {
            1
        } else if name.contains("keyframe") || name.ends_with(".kfi") {
            2
        } else if name.contains("wave") || name.ends_with(".wfp") {
            3
        } else {
            4
        };
        categories[idx].bytes = categories[idx].bytes.saturating_add(size);
        categories[idx].files = categories[idx].files.saturating_add(1);
        total = total.saturating_add(size);
    }
    CacheReport {
        root,
        categories,
        total_bytes: total,
    }
}

/// Non-recursive + one level of subdirs: enough for the flat sidecar layout.
fn walkdir_shallow(root: &Path) -> Vec<(PathBuf, u64)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_file() {
            if let Ok(meta) = entry.metadata() {
                out.push((path, meta.len()));
            }
        } else if path.is_dir() {
            if let Ok(sub) = std::fs::read_dir(&path) {
                for e in sub.flatten() {
                    let p = e.path();
                    if p.is_file() {
                        if let Ok(meta) = e.metadata() {
                            out.push((p, meta.len()));
                        }
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_classifies_proxy_and_poster() {
        let dir = std::env::temp_dir().join(format!("photonic-cache-stats-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let project = dir.join("movie.photon");
        let cache = cache_dir_for_project(&project);
        let _ = std::fs::remove_dir_all(&cache);
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("abc.proxy.mp4"), [0u8; 1000]).unwrap();
        std::fs::write(cache.join("abc.poster.png"), [0u8; 200]).unwrap();

        let report = summarize_cache(Some(&project));
        assert!(report.total_bytes >= 1200);
        let proxies = report
            .categories
            .iter()
            .find(|c| c.name == "proxies")
            .unwrap();
        let posters = report
            .categories
            .iter()
            .find(|c| c.name == "posters")
            .unwrap();
        assert_eq!(proxies.files, 1);
        assert_eq!(posters.files, 1);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&cache);
    }
}
