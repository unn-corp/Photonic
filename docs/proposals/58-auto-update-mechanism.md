# Auto-update mechanism (#58) — Design Proposal

> Status: design scaffold (not an implementation).

## Summary

Photonic ships binaries but has no mechanism to detect, download, or apply new releases.
Users must manually re-download and replace the binary, which slows delivery of bug fixes
and security patches. This proposal adds a background update checker and an opt-in
self-updater backed by GitHub Releases.

## Scope

**In:**
- Background version check against the GitHub Releases API (`https://api.github.com/repos/Quad-Kamatu/Photonic/releases/latest`).
- UI notification ("A new version is available — download?") in the toolbar or a status bar chip.
- Opt-in download + checksum verification before applying (using the existing `reqwest` dependency in `crates/photonic-app/Cargo.toml`).
- Settings toggle for auto-check (wired into `AppPreferences` in `crates/photonic-gui/src/preferences.rs`).
- "Check now" menu action.

**Out:**
- Delta/binary-patch updates (full binary replacement only for v1).
- Automatic silent updates without user consent.
- Flatpak / package-manager–driven updates (those channels self-update).

## Proposed approach

1. **Add `self_update` or manual HTTP crate**: The `self_update` crate (or equivalent hand-rolled code using the already-present `reqwest = "0.12"` in `crates/photonic-app/Cargo.toml`) fetches `/releases/latest` and compares `tag_name` against `env!("CARGO_PKG_VERSION")`.

2. **Background task**: Spawn a `tokio::task::spawn` in `crates/photonic-app/src/main.rs` after GUI init, so it does not block startup. Result is sent back via an `mpsc` channel already used for MCP→GUI communication.

3. **Persist user preference**: Add `check_for_updates: bool` (default `true`) to `AppPreferences` in `crates/photonic-gui/src/preferences.rs`. Persist in the existing `preferences.json`.

4. **UI notification**: In `crates/photonic-gui/src/app.rs`, if a pending update version is set, render a dismissible chip in the top bar. Clicking opens an egui modal that shows release notes excerpt and a download button.

5. **Download + verify**: Download the platform-appropriate binary asset, verify SHA256 against the release's `*.sha256` file, write to a temp path, then ask the OS to replace the running binary on next restart (on Linux: `rename(2)` to overwrite in place; on Windows: use a batch rename helper).

6. **Signature**: Stretch goal — GPG or minisign detached signature verification before applying. Requires adding a public key to the binary.

## Affected modules

- `crates/photonic-app/src/main.rs` — spawn background check task
- `crates/photonic-app/Cargo.toml` — add `self_update` or rely on existing `reqwest`
- `crates/photonic-gui/src/preferences.rs` — `AppPreferences::check_for_updates: bool`
- `crates/photonic-gui/src/app.rs` — update notification UI, "Check now" menu item
- `docs/architecture.md` — note the update subsystem

## Risks & open questions

- **Antivirus false positives**: Self-replacing binaries on Windows are commonly flagged; code signing is near-mandatory there.
- **Sandboxed installs**: Flatpak/Snap restrict network + filesystem access; this flow would need to be gated out on those distributions (detect via `FLATPAK_ID` env).
- **Release pipeline prerequisite**: This is blocked on a working release CI that publishes signed binaries; the current `ci.yml` only builds, not releases.
- **Rollback**: No rollback mechanism is proposed for v1; discuss for v2.
- Open Q: Should the updater restart the app automatically, or only on next launch?

## Acceptance criteria

- [ ] On startup (if `check_for_updates = true`), the app silently queries GitHub Releases without blocking the UI.
- [ ] When a newer version exists, a non-intrusive notification appears.
- [ ] User can trigger a download, checksum is verified before any write.
- [ ] The setting can be disabled; "Check now" works regardless.
- [ ] No crash or UI hang if the network is unavailable.

## Effort estimate

**M** — HTTP call + UI notification is small; safe binary replace + Windows handling + edge-case error UX is the majority of work.

## TLS trust and signed archive compatibility (October 2026)

The in-app updater uses `self_update` 1.3.1 with its blocking `ureq` client,
Rustls/WebPKI, ZIP 9 and zipsign-api 0.2.2. It still selects the Photonic release
for the current target/version and fixed executable name, verifies the archive
against `release/photonic-signing.pub` **before extraction**, and binds the
signature to the release filename. Windows ZIP and Linux/macOS tar.gz remain
supported. The release workflow pins the zipsign 0.2.2 crate and its SHA-256;
the signing key and release identity are unchanged. The updater retains public
WebPKI roots and environment proxy discovery. It has no application custom-CA
setting. The transport uses HTTP/1.1; HTTP/2 is not required for GitHub updates.

The Anthropic client uses the fixed public API endpoint. Model downloads use
fastembed's HuggingFace client, which honors `HF_ENDPOINT` and `HF_HOME`.
These application/model clients load OS roots through Rustls, preserving
enterprise roots and Unix `SSL_CERT_FILE`/`SSL_CERT_DIR` configuration while
verifying chains and hostnames with patched WebPKI. The application has no
separate CA-file preference. Reqwest retains system proxy discovery; the
HuggingFace client retains its environment proxy discovery. Local regression
fixtures exercise default clients with an untrusted CA, an explicitly configured
Unix CA, hostname mismatches and an owned HTTPS CONNECT proxy. They do not
contact Anthropic, HuggingFace or GitHub, or install an update.

Compromised eMudhra keys remain explicitly distrusted, including when presented
as trust anchors from a custom store. Invalid certificates and mismatched
hostnames are refused; adding a root does not bypass these checks. Root loading
preserves custom trust support, but Rustls/WebPKI performs verification rather
than the OS TLS engine, so this is not a claim of identical native-platform
certificate-policy semantics. Live enterprise proxy/platform acceptance and
installed release artifacts require separate validation.
