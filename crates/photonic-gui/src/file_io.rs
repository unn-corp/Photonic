use std::io::Read;
use std::path::Path;

/// Read an SVG with the same byte ceiling enforced by the core importer.
///
/// The extra byte read makes the check safe if a file grows between metadata
/// inspection and opening it, while keeping the temporary buffer bounded.
pub(crate) fn read_svg_file(path: &Path) -> Result<String, String> {
    let max_bytes = photonic_core::MAX_SVG_INPUT_BYTES as u64;
    if std::fs::metadata(path)
        .map_err(|error| error.to_string())?
        .len()
        > max_bytes
    {
        return Err(format!(
            "SVG file exceeds the {}-byte import limit",
            photonic_core::MAX_SVG_INPUT_BYTES
        ));
    }

    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|error| error.to_string())?
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > photonic_core::MAX_SVG_INPUT_BYTES {
        return Err(format!(
            "SVG file exceeds the {}-byte import limit",
            photonic_core::MAX_SVG_INPUT_BYTES
        ));
    }
    String::from_utf8(bytes).map_err(|_| "SVG file is not valid UTF-8".to_string())
}

#[cfg(test)]
mod tests {
    use super::read_svg_file;

    #[test]
    fn rejects_oversized_svg_before_reading_the_whole_file() {
        let path = std::env::temp_dir().join(format!(
            "photonic-svg-import-limit-{}.svg",
            std::process::id()
        ));
        std::fs::write(&path, vec![b'x'; photonic_core::MAX_SVG_INPUT_BYTES + 1])
            .expect("write oversized SVG fixture");

        let error = read_svg_file(&path).expect_err("oversized SVG should be rejected");
        assert!(error.contains("import limit"), "unexpected error: {error}");
        std::fs::remove_file(path).expect("remove SVG fixture");
    }
}
