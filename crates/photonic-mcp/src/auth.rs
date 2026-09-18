//! Session-token generation and storage for the MCP transport
//! (28-security-model.md §4 points 1–2).
//!
//! The MCP server binds loopback only, but loopback is not an authorization
//! boundary: any process running as any user on the machine can reach it. A
//! per-session bearer token, stored owner-only next to the other app-level
//! prefs, is what actually gates the endpoint.
//!
//! The token is generated fresh for every process launch (unless pinned with
//! `--mcp-secret`) and written to `<config>/mcp_token` so first-party clients
//! can discover it without the user copying anything.
//!
//! PLATFORM NOTE: on unix the file is chmod'd to `0o600` before the secret is
//! written, so it is never briefly world-readable. On Windows we rely on ACL
//! inheritance from `%APPDATA%\Photonic` rather than calling an ACL API
//! directly; that is accepted for v1.

use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Errors from reading or writing the session-token file.
///
/// Shaped after `photonic_video::export::presets::PresetStoreError` (same
/// `NoConfigDir` arm) so the two config-dir-backed stores read alike.
#[derive(Debug, thiserror::Error)]
pub enum TokenStoreError {
    #[error("could not resolve the app config directory")]
    NoConfigDir,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Generate a fresh 256-bit session token as lowercase hex.
///
/// Two v4 UUIDs concatenated — 32 hex chars each, 256 bits of `getrandom`
/// CSPRNG entropy in total. `uuid` is already a dependency, so this avoids
/// pulling `rand` in for one call.
pub fn generate_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

/// `<config>/mcp_token` — `None` when no config dir resolves.
///
/// Reuses `photonic_core::crash_dir`, the crate's single source of truth for
/// the app config directory (same choice `export::presets` documents).
pub fn token_path() -> Option<PathBuf> {
    photonic_core::crash_dir().map(|d| d.join("mcp_token"))
}

/// Write `token` to `path`, creating parents, truncating, and setting
/// owner-only permissions. Path-parameterized for tests.
pub fn write_token_to(path: &Path, token: &str) -> Result<(), TokenStoreError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Create/truncate first, tighten the mode, and only then write the secret,
    // so the token bytes never exist in a world-readable file.
    let file = std::fs::File::create(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    {
        use std::io::Write;
        let mut file = file;
        file.write_all(token.as_bytes())?;
        file.flush()?;
    }
    Ok(())
}

/// Convenience wrapper over [`token_path`] + [`write_token_to`], returning the
/// path written so the caller can log it (the path only — never the token).
pub fn write_token(token: &str) -> Result<PathBuf, TokenStoreError> {
    let path = token_path().ok_or(TokenStoreError::NoConfigDir)?;
    write_token_to(&path, token)?;
    Ok(path)
}

/// Read a token written by `write_token*`. Trims trailing whitespace/newline
/// so a hand-edited file with a trailing newline still works.
pub fn read_token_from(path: &Path) -> Result<String, TokenStoreError> {
    Ok(std::fs::read_to_string(path)?.trim().to_string())
}

/// Read the token from the resolved [`token_path`].
pub fn read_token() -> Result<String, TokenStoreError> {
    let path = token_path().ok_or(TokenStoreError::NoConfigDir)?;
    read_token_from(&path)
}

/// Constant-time equality — MUST be used for every token comparison so a
/// caller cannot recover the token a byte at a time by timing `==`.
pub fn token_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.bytes().zip(b.bytes()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Temp path unique per test run — never touches the real config dir.
    fn scratch_path() -> PathBuf {
        std::env::temp_dir().join(format!("photonic-mcp-token-{}", Uuid::new_v4()))
    }

    #[test]
    fn generate_token_is_64_hex_and_unique() {
        let a = generate_token();
        let b = generate_token();
        assert_ne!(a, b, "two generated tokens must differ");
        for tok in [&a, &b] {
            assert_eq!(tok.len(), 64, "token must be 64 hex chars (256 bits)");
            assert!(
                tok.chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "token must be lowercase hex: {tok}"
            );
        }
    }

    #[test]
    fn write_then_read_roundtrips() {
        let path = scratch_path();
        let token = generate_token();
        write_token_to(&path, &token).expect("write");
        let read = read_token_from(&path).expect("read");
        assert_eq!(read, token);
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn written_token_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let path = scratch_path();
        write_token_to(&path, &generate_token()).expect("write");
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "token file must be owner-only");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn token_eq_is_correct() {
        assert!(token_eq("abc", "abc"));
        assert!(token_eq("", ""));
        // Differing only in the last byte must still fail (no early return
        // on the first mismatch).
        assert!(!token_eq("abcd", "abce"));
        assert!(!token_eq("abc", "abcd"));
    }
}
