//! Local, offline text-embedding for semantic search.
//!
//! Wraps a small sentence-transformer (all-MiniLM-L6-v2, 384-dim) running fully
//! on-device via `fastembed` (ONNX/ort). The model is downloaded once to a cache
//! on first use, then works offline. No external AI service is required.

use anyhow::Result;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use std::path::PathBuf;

pub struct Embedder {
    model: TextEmbedding,
}

/// Stable per-user cache directory for the model, so it isn't recreated in the
/// process working directory: `$XDG_CACHE_HOME/Photonic/models`,
/// `%APPDATA%\Photonic\models`, or `~/.cache/Photonic/models`.
fn model_cache_dir() -> PathBuf {
    let base = std::env::var("XDG_CACHE_HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| std::env::var("APPDATA").ok().map(PathBuf::from))
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".cache"))
        })
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("Photonic").join("models")
}

impl Embedder {
    /// Load (downloading once if needed) the embedding model. Slow — call off
    /// the UI thread.
    pub fn new() -> Result<Self> {
        let model = TextEmbedding::try_new(
            InitOptions::new(EmbeddingModel::AllMiniLML6V2)
                .with_cache_dir(model_cache_dir())
                .with_show_download_progress(false),
        )?;
        Ok(Self { model })
    }

    /// Embed a batch of texts into L2-normalized 384-dim vectors.
    pub fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let docs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
        self.model.embed(docs, None)
    }
}

/// Cosine similarity of two equal-length vectors (assumes finite values).
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for i in 0..a.len().min(b.len()) {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}
