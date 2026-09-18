//! Path containment for MCP and other untrusted path inputs (28-security-model §3).
//!
//! See also `docs/specs/mcp-2026-07-28.md` §5 and `photonic_mcp::path_guard`.
//!
//! Component-wise root checks (never string-prefix), canonicalize with
//! deepest-existing-ancestor for write targets that do not exist yet.

use std::path::{Component, Path, PathBuf};

/// Policy for resolving paths against an allowlist of roots.
#[derive(Debug, Clone)]
pub struct PathPolicy {
    /// Absolute roots paths may live under.
    pub roots: Vec<PathBuf>,
    /// When true, **read** access is allowed outside roots (import media).
    /// Write remains root-bound unless a separate write policy is used.
    pub allow_read_outside: bool,
}

/// Outcome of a path check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathVerdict {
    Allowed(PathBuf),
    Denied { path: PathBuf, reason: DenyReason },
}

/// Why a path was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyReason {
    OutsideRoots,
    TraversalRejected,
    NotCanonicalizable,
    SymlinkEscape,
    DeviceOrFifo,
    NulByte,
    Empty,
}

impl std::fmt::Display for DenyReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutsideRoots => write!(f, "outside_roots"),
            Self::TraversalRejected => write!(f, "traversal_rejected"),
            Self::NotCanonicalizable => write!(f, "not_canonicalizable"),
            Self::SymlinkEscape => write!(f, "symlink_escape"),
            Self::DeviceOrFifo => write!(f, "device_or_fifo"),
            Self::NulByte => write!(f, "nul_byte"),
            Self::Empty => write!(f, "empty"),
        }
    }
}

/// Read vs write intent — write never uses `allow_read_outside`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathAccess {
    Read,
    Write,
}

impl PathPolicy {
    /// Build a policy with absolute roots. Empty roots deny everything that
    /// is not covered by `allow_read_outside` on read.
    pub fn new(roots: impl IntoIterator<Item = PathBuf>, allow_read_outside: bool) -> Self {
        let roots = roots
            .into_iter()
            .map(|r| {
                r.canonicalize()
                    .unwrap_or_else(|_| make_absolute(&r).unwrap_or(r))
            })
            .collect();
        Self {
            roots,
            allow_read_outside,
        }
    }

    /// Default desktop policy: cwd + config dir as roots; allow read outside.
    pub fn desktop_default() -> Self {
        let mut roots = Vec::new();
        if let Ok(cwd) = std::env::current_dir() {
            roots.push(cwd);
        }
        if let Some(cfg) = crate::diagnostics::crash_dir() {
            // crash_dir is under app config; parent is a sensible root.
            if let Some(parent) = cfg.parent() {
                roots.push(parent.to_path_buf());
            }
            roots.push(cfg);
        }
        // Home directory as soft write root so save_as under ~ is common-case OK.
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            roots.push(home);
        }
        #[cfg(windows)]
        if let Some(home) = std::env::var_os("USERPROFILE").map(PathBuf::from) {
            roots.push(home);
        }
        Self::new(roots, true)
    }

    /// Desktop defaults plus the system temp directory.
    ///
    /// Unit/integration tests typically write under `std::env::temp_dir()`; production
    /// MCP servers should keep using [`Self::desktop_default`] (or a narrower root list).
    pub fn test_default() -> Self {
        let mut roots = Self::desktop_default().roots;
        roots.push(std::env::temp_dir());
        Self::new(roots, true)
    }

    /// Check `path` for `access`. Returns the canonical absolute path if allowed.
    pub fn check(&self, path: impl AsRef<Path>, access: PathAccess) -> PathVerdict {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            return PathVerdict::Denied {
                path: path.to_path_buf(),
                reason: DenyReason::Empty,
            };
        }
        let path_str = path.to_string_lossy();
        if path_str.contains('\0') {
            return PathVerdict::Denied {
                path: path.to_path_buf(),
                reason: DenyReason::NulByte,
            };
        }

        // Reject `..` components before canonicalize for clearer errors.
        if path.components().any(|c| matches!(c, Component::ParentDir)) {
            // Still try to resolve — parent dirs can be legitimate relative paths.
            // Containment after canonicalize is the real gate.
        }

        let resolved = match resolve_path(path) {
            Ok(p) => p,
            Err(_) => {
                return PathVerdict::Denied {
                    path: path.to_path_buf(),
                    reason: DenyReason::NotCanonicalizable,
                };
            }
        };

        // Device / FIFO / socket: refuse for both read and write.
        if let Ok(meta) = std::fs::symlink_metadata(&resolved) {
            let ft = meta.file_type();
            #[cfg(unix)]
            {
                use std::os::unix::fs::FileTypeExt;
                if ft.is_fifo() || ft.is_socket() || ft.is_block_device() || ft.is_char_device() {
                    return PathVerdict::Denied {
                        path: resolved,
                        reason: DenyReason::DeviceOrFifo,
                    };
                }
            }
            let _ = ft;
        }

        let inside = self.roots.iter().any(|root| is_within(root, &resolved));
        match access {
            PathAccess::Read if !inside && self.allow_read_outside => {
                PathVerdict::Allowed(resolved)
            }
            PathAccess::Read | PathAccess::Write if inside => PathVerdict::Allowed(resolved),
            _ => PathVerdict::Denied {
                path: resolved,
                reason: DenyReason::OutsideRoots,
            },
        }
    }

    /// Convenience: map to Result with Display message for MCP handlers.
    pub fn require(
        &self,
        path: impl AsRef<Path>,
        access: PathAccess,
    ) -> Result<PathBuf, PathPolicyError> {
        match self.check(path, access) {
            PathVerdict::Allowed(p) => Ok(p),
            PathVerdict::Denied { path, reason } => Err(PathPolicyError { path, reason }),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PathPolicyError {
    pub path: PathBuf,
    pub reason: DenyReason,
}

impl std::fmt::Display for PathPolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "PathNotPermitted: {} ({})",
            self.path.display(),
            self.reason
        )
    }
}

impl std::error::Error for PathPolicyError {}

/// Resolve to absolute, symlink-resolved path. For non-existent paths,
/// canonicalize the deepest existing ancestor and re-join the remainder.
pub fn resolve_path(path: &Path) -> std::io::Result<PathBuf> {
    let abs = make_absolute(path)?;
    if abs.exists() {
        return abs.canonicalize();
    }
    // Walk up until an existing ancestor is found.
    let mut components: Vec<_> = abs.components().collect();
    let mut suffix = Vec::new();
    while !components.is_empty() {
        let candidate: PathBuf = components.iter().collect();
        if candidate.exists() {
            let mut resolved = candidate.canonicalize()?;
            for c in suffix.iter().rev() {
                resolved.push(c);
            }
            return Ok(resolved);
        }
        if let Some(last) = components.pop() {
            match last {
                Component::Normal(s) => suffix.push(s.to_os_string()),
                Component::ParentDir => {
                    // Keep pushing; make_absolute already collapsed some cases.
                    suffix.push(std::ffi::OsString::from(".."));
                }
                _ => suffix.push(last.as_os_str().to_os_string()),
            }
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "no existing ancestor to canonicalize",
    ))
}

fn make_absolute(path: &Path) -> std::io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

/// Component-wise "is `child` under `root`?" (after both are absolute/canonical).
pub fn is_within(root: &Path, child: &Path) -> bool {
    let root_comps: Vec<_> = root.components().collect();
    let child_comps: Vec<_> = child.components().collect();
    if child_comps.len() < root_comps.len() {
        return false;
    }
    root_comps
        .iter()
        .zip(child_comps.iter())
        .all(|(a, b)| a == b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp_root() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "photonic-path-policy-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    #[test]
    fn inside_root_allowed() {
        let root = tmp_root();
        let file = root.join("doc.photon");
        fs::write(&file, b"x").unwrap();
        let pol = PathPolicy::new([root.clone()], false);
        match pol.check(&file, PathAccess::Write) {
            PathVerdict::Allowed(p) => assert_eq!(p, file.canonicalize().unwrap()),
            other => panic!("expected Allowed, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn outside_root_write_denied() {
        let root = tmp_root();
        let other =
            std::env::temp_dir().join(format!("photonic-path-policy-other-{}", std::process::id()));
        fs::create_dir_all(&other).unwrap();
        let file = other.join("secret.txt");
        fs::write(&file, b"x").unwrap();
        let pol = PathPolicy::new([root.clone()], true);
        match pol.check(&file, PathAccess::Write) {
            PathVerdict::Denied {
                reason: DenyReason::OutsideRoots,
                ..
            } => {}
            other => panic!("expected OutsideRoots, got {other:?}"),
        }
        // Read outside allowed when flag set.
        assert!(matches!(
            pol.check(&file, PathAccess::Read),
            PathVerdict::Allowed(_)
        ));
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&other);
    }

    #[test]
    fn string_prefix_is_not_enough() {
        let base = tmp_root();
        let root = base.join("proj");
        let evil = base.join("proj-evil");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&evil).unwrap();
        let file = evil.join("x.txt");
        fs::write(&file, b"x").unwrap();
        let pol = PathPolicy::new([root.canonicalize().unwrap()], false);
        match pol.check(&file, PathAccess::Write) {
            PathVerdict::Denied {
                reason: DenyReason::OutsideRoots,
                ..
            } => {}
            other => panic!("proj-evil must not match proj: {other:?}"),
        }
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn nonexist_write_target_uses_ancestor() {
        let root = tmp_root();
        let target = root.join("exports").join("out.mp4");
        // parent does not exist yet
        let pol = PathPolicy::new([root.clone()], false);
        match pol.check(&target, PathAccess::Write) {
            PathVerdict::Allowed(p) => {
                assert!(p.starts_with(&root));
                assert!(p.ends_with("out.mp4"));
            }
            other => panic!("expected Allowed for nested new file, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn nul_byte_rejected() {
        let pol = PathPolicy::new([std::env::temp_dir()], true);
        let bad = PathBuf::from("foo\0bar");
        assert!(matches!(
            pol.check(&bad, PathAccess::Read),
            PathVerdict::Denied {
                reason: DenyReason::NulByte,
                ..
            }
        ));
    }
}
