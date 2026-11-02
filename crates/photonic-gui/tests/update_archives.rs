//! Offline archive/signature compatibility; never invokes an update or installs a binary.
use std::{fs, path::PathBuf};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn signed_updates_keep_filename_key_and_selected_binary_boundaries() {
    let root =
        Fixture(std::env::temp_dir().join(format!("photonic-update-{}", uuid::Uuid::new_v4())));
    fs::create_dir(&root.0).unwrap();
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/update");
    let key = *include_bytes!("fixtures/update/fixture.pub");
    for (extension, binary) in [("zip", "photonic.exe"), ("tar.gz", "photonic")] {
        let filename = format!("photonic-fixture.{extension}");
        let archive = root.0.join(&filename);
        fs::copy(fixtures.join(&filename), &archive).unwrap();
        self_update::verify_signature(&archive, &[key]).unwrap();
        assert!(self_update::verify_signature(&archive, &[[0; 32]]).is_err());
        let renamed = root.0.join(format!("other-release.{extension}"));
        fs::copy(&archive, &renamed).unwrap();
        assert!(self_update::verify_signature(&renamed, &[key]).is_err());
        let unsigned = root.0.join(format!("unsigned-{filename}"));
        fs::copy(fixtures.join(format!("unsigned-{filename}")), &unsigned).unwrap();
        assert!(self_update::verify_signature(&unsigned, &[key]).is_err());
        let out = root.0.join(extension);
        fs::create_dir(&out).unwrap();
        self_update::Extract::from_source(&archive)
            .extract_file(&out, binary)
            .unwrap();
        assert_eq!(
            fs::read(out.join(binary)).unwrap(),
            b"owned-fixture-binary\n"
        );
        assert_eq!(fs::read_dir(&out).unwrap().count(), 1);
        assert!(!root.0.join("outside.txt").exists());
        assert!(!root.0.join("outside-target").exists());
        assert!(self_update::Extract::from_source(&archive)
            .extract_file(&out, "missing-binary")
            .is_err());
    }
}
