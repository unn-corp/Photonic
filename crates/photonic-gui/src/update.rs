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
        let expected_binary = if cfg!(windows) {
            "photonic.exe"
        } else {
            "photonic"
        };
        assert_eq!(updater.bin_name(), expected_binary);
        assert_eq!(updater.bin_path_in_archive(), expected_binary);
        assert_eq!(updater.current_version(), env!("CARGO_PKG_VERSION"));
        assert_eq!(updater.target(), self_update::get_target());
        assert!(updater.no_confirm());
        assert!(!updater.show_download_progress());
    }

    struct ArchiveResponse {
        headers: self_update::http_client::HeaderMap,
        bytes: Vec<u8>,
    }

    impl self_update::http_client::HttpResponse for ArchiveResponse {
        fn headers(&self) -> &self_update::http_client::HeaderMap {
            &self.headers
        }

        fn body(self: Box<Self>) -> Box<dyn std::io::Read> {
            Box::new(std::io::Cursor::new(self.bytes))
        }
    }

    struct ArchiveClient(Vec<u8>);

    impl self_update::http_client::HttpClient for ArchiveClient {
        fn get(
            &self,
            url: &str,
            _headers: &self_update::http_client::HeaderMap,
            _timeout: Option<std::time::Duration>,
        ) -> self_update::Result<Box<dyn self_update::http_client::HttpResponse>> {
            let bytes = if url == "https://fixture.invalid/photonic-fixture.tar.gz" {
                self.0.clone()
            } else {
                assert!(url.starts_with("https://api.github.com/repos/unn-corp/Photonic/releases"));
                serde_json::to_vec(&serde_json::json!([{
                    "tag_name": "v1.0.1",
                    "created_at": "2026-11-02T00:00:00Z",
                    "name": "owned fixture",
                    "assets": [{
                        "name": "photonic-fixture.tar.gz",
                        "url": "https://fixture.invalid/photonic-fixture.tar.gz"
                    }]
                }]))
                .unwrap()
            };
            Ok(Box::new(ArchiveResponse {
                headers: self_update::http_client::HeaderMap::new(),
                bytes,
            }))
        }
    }

    struct UpdateFixture(std::path::PathBuf);

    impl Drop for UpdateFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn install_updater_enforces_signatures_before_replacing_a_binary() {
        let fixture_key = *include_bytes!("../tests/fixtures/update/fixture.pub");
        assert_ne!(&fixture_key, SIGNING_PUBKEY);
        let fixtures =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/update");
        for (archive, accept_fixture_key) in [
            ("photonic-fixture.tar.gz", false),
            ("unsigned-photonic-fixture.tar.gz", false),
            ("photonic-fixture.tar.gz", true),
        ] {
            let root =
                std::env::temp_dir().join(format!("photonic-install-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&root).unwrap();
            let fixture = UpdateFixture(root);
            let target = fixture.0.join("photonic");
            std::fs::write(&target, b"original-owned-target").unwrap();
            let client = ArchiveClient(std::fs::read(fixtures.join(archive)).unwrap());
            let mut builder = install_builder();
            builder
                .auth_token("")
                .current_version("1.0.0")
                .target("fixture")
                .bin_path_in_archive("photonic")
                .bin_install_path(&target)
                .show_output(false)
                .http_client(std::sync::Arc::new(client));
            if accept_fixture_key {
                // Positive control: the same actual update path must accept a
                // correctly signed fixture when its public key is authorized.
                builder.verifying_keys([fixture_key]);
            }
            let result = builder.build().unwrap().update();
            if accept_fixture_key {
                assert!(result.unwrap().is_updated());
                assert_eq!(std::fs::read(&target).unwrap(), b"owned-fixture-binary\n");
            } else {
                assert!(
                    matches!(
                        result,
                        Err(self_update::Error::Signature(_))
                            | Err(self_update::Error::NoSignatures(_))
                    ),
                    "{result:?}"
                );
                assert_eq!(std::fs::read(&target).unwrap(), b"original-owned-target");
            }
        }
    }
}
