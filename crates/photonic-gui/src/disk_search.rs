//! Background disk search for `.photon` documents under user-picked roots.
//!
//! Nothing is scanned until the user adds a root folder/drive. For each root we
//! prefer the OS file index for speed — `plocate`/`locate` (Linux) or `mdfind`
//! (macOS) — and fall back to a fresh pure-Rust recursive walk (also the Windows
//! path, and whenever an index isn't available). A "deep rescan" forces the walk
//! so newly-created files always show even if the OS index is stale.
//!
//! The worker streams results over a channel as they're found, tagged with a
//! generation so stale results from a superseded scan are ignored. Skips heavy
//! and system directories.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use photonic_core::PHOTON_FILE_EXTENSION;

const EXT: &str = PHOTON_FILE_EXTENSION;
const MAX_DEPTH: usize = 40;

/// Directory names skipped during a walk (heavy build/cache/system dirs).
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    ".git",
    ".svn",
    ".hg",
    ".cache",
    "__pycache__",
    ".gradle",
    ".cargo",
    ".rustup",
    "$RECYCLE.BIN",
    "System Volume Information",
    "proc",
    "sys",
    "dev",
    "run",
];

pub struct DiskFile {
    pub path: PathBuf,
    pub name: String,
    /// Modified time as seconds since the Unix epoch (0 if unknown).
    pub modified: u64,
}

struct Msg {
    gen: u64,
    found: Option<DiskFile>, // None == Done
}

struct Request {
    gen: u64,
    roots: Vec<PathBuf>,
    deep: bool,
}

pub struct DiskScanner {
    req_tx: Sender<Request>,
    res_rx: Receiver<Msg>,
    gen: Arc<AtomicU64>,
    pub files: Vec<DiskFile>,
    pub scanning: bool,
}

impl Default for DiskScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl DiskScanner {
    pub fn new() -> Self {
        let (req_tx, req_rx) = channel::<Request>();
        let (res_tx, res_rx) = channel::<Msg>();
        let gen = Arc::new(AtomicU64::new(0));
        let worker_gen = Arc::clone(&gen);
        std::thread::Builder::new()
            .name("photonic-disk".into())
            .spawn(move || {
                while let Ok(req) = req_rx.recv() {
                    // Skip if a newer request already superseded this one.
                    if worker_gen.load(Ordering::SeqCst) != req.gen {
                        continue;
                    }
                    scan_roots(&req.roots, req.deep, req.gen, &worker_gen, &res_tx);
                    let _ = res_tx.send(Msg {
                        gen: req.gen,
                        found: None,
                    });
                }
            })
            .ok();
        Self {
            req_tx,
            res_rx,
            gen,
            files: Vec::new(),
            scanning: false,
        }
    }

    /// Clear current results and start a new scan of `roots`. `deep` forces the
    /// recursive walk (ignoring the OS index) so fresh files are always found.
    pub fn rescan(&mut self, roots: Vec<PathBuf>, deep: bool) {
        let gen = self.gen.fetch_add(1, Ordering::SeqCst) + 1;
        self.files.clear();
        self.scanning = !roots.is_empty();
        if !roots.is_empty() {
            let _ = self.req_tx.send(Request { gen, roots, deep });
        }
    }

    /// Drain freshly-found files into `files`. Call once per frame.
    pub fn pump(&mut self) {
        let cur = self.gen.load(Ordering::SeqCst);
        while let Ok(msg) = self.res_rx.try_recv() {
            if msg.gen != cur {
                continue; // stale scan
            }
            match msg.found {
                Some(f) => self.files.push(f),
                None => {
                    self.scanning = false;
                    self.files.sort_by(|a, b| b.modified.cmp(&a.modified));
                }
            }
        }
    }
}

// ─── Worker-side scanning ───────────────────────────────────────────────────────

fn scan_roots(roots: &[PathBuf], deep: bool, gen: u64, cur: &AtomicU64, tx: &Sender<Msg>) {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    for root in roots {
        if cur.load(Ordering::SeqCst) != gen {
            return; // superseded
        }
        if !root.exists() {
            continue;
        }
        let indexed = if deep { None } else { os_index(root) };
        if let Some(paths) = indexed {
            for p in paths {
                if cur.load(Ordering::SeqCst) != gen {
                    return;
                }
                emit(p, gen, &mut seen, tx);
            }
        } else {
            walk(root, 0, gen, cur, &mut seen, tx);
        }
    }
}

fn emit(path: PathBuf, gen: u64, seen: &mut HashSet<PathBuf>, tx: &Sender<Msg>) {
    if path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case(EXT))
        != Some(true)
    {
        return;
    }
    let meta = match std::fs::metadata(&path) {
        Ok(m) if m.is_file() => m,
        _ => return, // missing / not a file (filters stale index entries)
    };
    if !seen.insert(path.clone()) {
        return;
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = tx.send(Msg {
        gen,
        found: Some(DiskFile {
            path,
            name,
            modified,
        }),
    });
}

fn walk(
    dir: &Path,
    depth: usize,
    gen: u64,
    cur: &AtomicU64,
    seen: &mut HashSet<PathBuf>,
    tx: &Sender<Msg>,
) {
    if depth > MAX_DEPTH || cur.load(Ordering::SeqCst) != gen {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if cur.load(Ordering::SeqCst) != gen {
            return;
        }
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if file_type.is_dir() {
            // Skip hidden (except the root itself, handled by caller), build,
            // cache and system directories.
            if name.starts_with('.') || SKIP_DIRS.iter().any(|s| s.eq_ignore_ascii_case(&name)) {
                continue;
            }
            walk(&path, depth + 1, gen, cur, seen, tx);
        } else if file_type.is_file() {
            emit(path, gen, seen, tx);
        }
    }
}

// ─── OS file-index acceleration ─────────────────────────────────────────────────

#[cfg(target_os = "macos")]
fn os_index(root: &Path) -> Option<Vec<PathBuf>> {
    let out = Command::new("mdfind")
        .arg("-onlyin")
        .arg(root)
        .arg(format!(r#"kMDItemFSName == "*.{EXT}"c"#))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let v: Vec<PathBuf> = s
        .lines()
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .collect();
    (!v.is_empty()).then_some(v)
}

#[cfg(target_os = "linux")]
fn os_index(root: &Path) -> Option<Vec<PathBuf>> {
    let root_str = root.to_string_lossy().into_owned();
    for cmd in ["plocate", "locate"] {
        if let Ok(out) = Command::new(cmd).arg("-i").arg(format!("*.{EXT}")).output() {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout);
                let v: Vec<PathBuf> = s
                    .lines()
                    .filter(|l| l.starts_with(&root_str))
                    .map(PathBuf::from)
                    .collect();
                return (!v.is_empty()).then_some(v);
            }
        }
    }
    None
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn os_index(_root: &Path) -> Option<Vec<PathBuf>> {
    None
}
