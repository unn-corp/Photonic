//! In-app self-update from GitHub Releases (unn-corp/Photonic).
//!
//! Pulls the latest release's binary asset, verifies it, and replaces the
//! running executable (applied on next launch). No server required — GitHub
//! Releases is both the host and the manifest. Network + file replacement runs
//! on a background thread; the UI polls the returned channel.
//!
//! Integrity: downloads are over TLS from GitHub. For tamper-proof updates,
//! sign release archives with `zipsign` (ed25519) and pass the public key to
//! `.verifying_keys(...)` below — a release-pipeline step (see `PUBLIC_KEY`).

use std::sync::mpsc::{channel, Receiver};

pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

pub const REPO_OWNER: &str = "unn-corp";
pub const REPO_NAME: &str = "Photonic";
const BIN_NAME: &str = "photonic";

/// ed25519 public key (zipsign) that release archives are signed with. The
/// matching private key is a GitHub Actions secret; CI signs every release, and
/// an update is only applied if its archive verifies against this key.
const SIGNING_PUBKEY: &[u8; 32] = include_bytes!("../../../release/photonic-signing.pub");

#[derive(Clone, Debug)]
pub enum UpdateStatus {
    UpToDate(String),
    Updated(String),
    Error(String),
}

/// Result of a lightweight check (no download/install).
#[derive(Clone, Debug)]
pub enum UpdateCheck {
    UpToDate,
    Available(String),
    Error(String),
}

/// Check (off-thread) whether a newer release exists, without downloading it.
pub fn check_latest() -> Receiver<UpdateCheck> {
    let (tx, rx) = channel();
    std::thread::Builder::new()
        .name("photonic-update-check".into())
        .spawn(move || {
            let _ = tx.send(check());
        })
        .ok();
    rx
}

fn check() -> UpdateCheck {
    let updater = match configured_builder().build() {
        Ok(u) => u,
        Err(e) => return UpdateCheck::Error(e.to_string()),
    };
    let releases = match updater.get_latest_release() {
        Ok(r) => r,
        Err(e) => return UpdateCheck::Error(e.to_string()),
    };
    let Some(release) = releases.latest() else {
        return UpdateCheck::Error("No release found".into());
    };
    match self_update::version::bump_is_greater(CURRENT_VERSION, release.version()) {
        Ok(true) => UpdateCheck::Available(release.version().to_owned()),
        Ok(false) => UpdateCheck::UpToDate,
        Err(e) => UpdateCheck::Error(e.to_string()),
    }
}

/// Start a check-and-update on a background thread; poll the receiver each frame.
pub fn check_and_update() -> Receiver<UpdateStatus> {
    let (tx, rx) = channel();
    std::thread::Builder::new()
        .name("photonic-update".into())
        .spawn(move || {
            let _ = tx.send(run());
        })
        .ok();
    rx
}

fn configured_builder() -> self_update::backends::github::UpdateBuilder {
    let mut builder = self_update::backends::github::Update::configure();
    builder
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        .bin_name(BIN_NAME)
        .current_version(CURRENT_VERSION);
    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        if !token.is_empty() {
            builder.auth_token(&token);
        }
    }
    builder
}

fn install_builder() -> self_update::backends::github::UpdateBuilder {
    let mut builder = configured_builder();
    builder
        .show_download_progress(false)
        .no_confirm(true)
        // Only apply updates whose archive is signed by our release key.
        .verifying_keys([*SIGNING_PUBKEY]);
    builder
}

fn run() -> UpdateStatus {
    let builder = install_builder();
    let updater = match builder.build() {
        Ok(u) => u,
        Err(e) => return UpdateStatus::Error(e.to_string()),
    };
    match updater.update() {
        Ok(status) => {
            let version = status.version().to_owned();
            if status.is_updated() {
                UpdateStatus::Updated(version)
            } else {
                UpdateStatus::UpToDate(version)
            }
        }
        Err(e) => UpdateStatus::Error(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use self_update::UpdateConfig;

    #[test]
    fn updater_keeps_version_target_and_ui_settings() {
        // Building a config performs no HTTP request or installation.
        let updater = install_builder().build().unwrap();
        assert_eq!(updater.bin_name(), "photonic");
        assert_eq!(updater.current_version(), env!("CARGO_PKG_VERSION"));
        assert_eq!(updater.target(), self_update::get_target());
        assert!(updater.no_confirm());
        assert!(!updater.show_download_progress());
    }
}
