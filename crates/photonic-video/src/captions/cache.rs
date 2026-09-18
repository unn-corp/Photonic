//! TTS result caching (06 §6): `hash(provider_id, voice_id, params, text)` →
//! `<project>.photon.cache/ai/tts/<hash>.wav`.
//!
//! Same xxh3 approach as `01 §3`'s `content_hash` (see
//! `media::probe::content_hash`) for a stable, deterministic key — but hashed
//! over the *request*, not file bytes, since there's no file yet on a cache
//! miss.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use xxhash_rust::xxh3::Xxh3;

/// Deterministic cache key for a TTS request. `params` is a `HashMap`, so
/// keys are sorted before hashing — iteration order must never leak into the
/// key, or identical requests could miss the cache depending on hash-map
/// internals.
pub fn tts_cache_key(
    provider_id: &str,
    voice_id: &str,
    params: &HashMap<String, f32>,
    text: &str,
) -> String {
    let mut hasher = Xxh3::new();
    hasher.update(provider_id.as_bytes());
    hasher.update(&[0]);
    hasher.update(voice_id.as_bytes());
    hasher.update(&[0]);

    let mut keys: Vec<&String> = params.keys().collect();
    keys.sort();
    for k in keys {
        hasher.update(k.as_bytes());
        hasher.update(b"=");
        hasher.update(&params[k].to_le_bytes());
        hasher.update(&[0]);
    }
    hasher.update(&[0xff]); // separator between the params block and text
    hasher.update(text.as_bytes());

    format!("{:016x}", hasher.digest())
}

/// `<project>.photon.cache/ai/tts/<cache_key>.wav` (06 §6).
pub fn tts_cache_path(project_path: &Path, cache_key: &str) -> PathBuf {
    crate::media::cache_dir_for_project(project_path)
        .join("ai")
        .join("tts")
        .join(format!("{cache_key}.wav"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_requests_produce_the_same_key() {
        let mut p1 = HashMap::new();
        p1.insert("speed".to_string(), 1.0f32);
        p1.insert("pitch".to_string(), 0.5f32);
        let mut p2 = HashMap::new();
        p2.insert("pitch".to_string(), 0.5f32);
        p2.insert("speed".to_string(), 1.0f32);

        let k1 = tts_cache_key("hosted", "voice-a", &p1, "hello world");
        let k2 = tts_cache_key("hosted", "voice-a", &p2, "hello world");
        assert_eq!(k1, k2, "insertion order must not affect the cache key");
    }

    #[test]
    fn different_inputs_produce_different_keys() {
        let params = HashMap::new();
        let base = tts_cache_key("hosted", "voice-a", &params, "hello");
        assert_ne!(base, tts_cache_key("hosted", "voice-b", &params, "hello"));
        assert_ne!(base, tts_cache_key("hosted", "voice-a", &params, "goodbye"));
        assert_ne!(base, tts_cache_key("other", "voice-a", &params, "hello"));

        let mut p2 = HashMap::new();
        p2.insert("speed".to_string(), 1.5f32);
        assert_ne!(base, tts_cache_key("hosted", "voice-a", &p2, "hello"));
    }

    #[test]
    fn cache_key_is_stable_hex_and_fixed_length() {
        let params = HashMap::new();
        let key = tts_cache_key("hosted", "voice-a", &params, "hi");
        assert_eq!(key.len(), 16);
        assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(key, tts_cache_key("hosted", "voice-a", &params, "hi"));
    }

    #[test]
    fn cache_path_matches_the_sidecar_convention() {
        let project = Path::new("/projects/movie.photon");
        let path = tts_cache_path(project, "abcdef0123456789");
        assert_eq!(
            path,
            PathBuf::from("/projects/movie.photon.cache/ai/tts/abcdef0123456789.wav")
        );
    }
}
