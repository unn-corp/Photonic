//! Embedded release notes for the in-app "What's New" popup.
//!
//! The repo `CHANGELOG.md` is compiled into the binary, so the notes always
//! match the running build and work offline. We parse its `## [version] - date`
//! sections (Keep a Changelog format) into ordered entries and surface the ones
//! a user hasn't seen yet — i.e. everything newer than the version they last ran.

/// The repo changelog, baked into the binary at build time.
const CHANGELOG: &str = include_str!("../../../CHANGELOG.md");

/// One released version's notes.
pub struct ReleaseNote {
    /// Version string, e.g. "0.2.0".
    pub version: String,
    /// Optional date as written in the heading (e.g. "2026-06-29").
    pub date: Option<String>,
    /// The section body (markdown between this heading and the next).
    pub body: String,
}

/// Parse `CHANGELOG.md` into versioned entries, newest first.
///
/// Only headings of the form `## [x.y.z] - date` are returned — the
/// `[Unreleased]` section (no version) is skipped, since it isn't a build a
/// user can be running.
pub fn all() -> Vec<ReleaseNote> {
    let mut out = Vec::new();
    let mut cur: Option<ReleaseNote> = None;

    for line in CHANGELOG.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            // Flush the previous section.
            if let Some(prev) = cur.take() {
                out.push(prev);
            }
            if let Some((version, date)) = parse_heading(rest) {
                cur = Some(ReleaseNote {
                    version,
                    date,
                    body: String::new(),
                });
            }
            // Non-version heading (e.g. "[Unreleased]") → cur stays None, lines drop.
            continue;
        }
        if let Some(note) = cur.as_mut() {
            note.body.push_str(line);
            note.body.push('\n');
        }
    }
    if let Some(prev) = cur.take() {
        out.push(prev);
    }

    for n in &mut out {
        n.body = n.body.trim().to_string();
    }
    out
}

/// Versions strictly newer than `last_seen`, newest first. An empty or
/// unparseable `last_seen` yields nothing (fresh installs shouldn't be nagged).
pub fn since(last_seen: &str) -> Vec<ReleaseNote> {
    let last = match parse_semver(last_seen) {
        Some(v) => v,
        None => return Vec::new(),
    };
    all()
        .into_iter()
        .filter(|n| parse_semver(&n.version).map(|v| v > last).unwrap_or(false))
        .collect()
}

/// Parse `[1.2.3] - 2026-06-29` (date optional) → ("1.2.3", Some("2026-06-29")).
fn parse_heading(s: &str) -> Option<(String, Option<String>)> {
    let s = s.trim();
    let close = s.find(']')?;
    if !s.starts_with('[') {
        return None;
    }
    let version = s[1..close].trim().to_string();
    // A bracketed token that isn't a version (e.g. "Unreleased") → reject.
    parse_semver(&version)?;
    let date = s[close + 1..]
        .trim_start_matches([' ', '-'])
        .trim()
        .to_string();
    Some((version, if date.is_empty() { None } else { Some(date) }))
}

/// Lenient semver → (major, minor, patch). Ignores any pre-release/build suffix.
fn parse_semver(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.trim().trim_start_matches('v');
    let core = core.split(['-', '+']).next().unwrap_or(core);
    let mut it = core.split('.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next().unwrap_or("0").parse().ok()?;
    let patch = it.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_versioned_sections() {
        let notes = all();
        assert!(notes.iter().any(|n| n.version == "0.1.0"));
        // The Unreleased section must never appear as a runnable version.
        assert!(notes.iter().all(|n| n.version != "Unreleased"));
        // 0.1.0 carries its date.
        let v010 = notes.iter().find(|n| n.version == "0.1.0").unwrap();
        assert_eq!(v010.date.as_deref(), Some("2026-06-29"));
        assert!(!v010.body.is_empty());
    }

    #[test]
    fn since_filters_older_and_equal() {
        // Nothing is newer than a very high version.
        assert!(since("999.0.0").is_empty());
        // Fresh install (empty) → no nag.
        assert!(since("").is_empty());
    }

    #[test]
    fn semver_ordering() {
        assert!(parse_semver("0.2.0") > parse_semver("0.1.9"));
        assert!(parse_semver("1.0.0") > parse_semver("0.99.99"));
        assert_eq!(parse_semver("v0.1.0"), parse_semver("0.1.0"));
    }
}
