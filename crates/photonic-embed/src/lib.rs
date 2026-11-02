//! Local, offline text-embedding for semantic search.
//!
//! Wraps a small sentence-transformer (all-MiniLM-L6-v2, 384-dim) running fully
//! on-device via `fastembed` (ONNX/ort). The model is downloaded once to a cache
//! on first use, then works offline. No external AI service is required.

use anyhow::Result;
use fastembed::{
    EmbeddingModel, InitOptionsUserDefined, TextEmbedding, TokenizerFiles,
    UserDefinedEmbeddingModel,
};
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
        // Keep the synchronous public API usable even when its caller already
        // runs inside Tokio. The short-lived downloader owns its own runtime.
        let files = std::thread::Builder::new()
            .name("photonic-model-download".to_owned())
            .spawn(download_model)?
            .join()
            .map_err(|_| anyhow::anyhow!("model downloader thread failed"))??;
        let model = TextEmbedding::try_new_from_user_defined(files, InitOptionsUserDefined::new())?;
        Ok(Self { model })
    }

    /// Embed a batch of texts into L2-normalized 384-dim vectors.
    pub fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let docs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
        self.model.embed(docs, None)
    }
}

fn download_model() -> Result<UserDefinedEmbeddingModel> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let model_name = EmbeddingModel::AllMiniLML6V2;
            let info = TextEmbedding::get_model_info(&model_name)?;
            // Preserve fastembed's existing HF_HOME and HF_ENDPOINT overrides,
            // repository, file names, token lookup, and cache layout.
            let cache_dir = std::env::var("HF_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| model_cache_dir());
            let endpoint = std::env::var("HF_ENDPOINT")
                .unwrap_or_else(|_| "https://huggingface.co".to_owned());
            let api = hf_hub::api::tokio::ApiBuilder::new()
                .with_cache_dir(cache_dir)
                .with_endpoint(endpoint)
                .with_progress(false)
                .build()?;
            let repo = api.model(info.model_code.clone());
            let weights = std::fs::read(repo.get(&info.model_file).await?)?;
            let tokenizer_files = TokenizerFiles {
                tokenizer_file: std::fs::read(repo.get("tokenizer.json").await?)?,
                config_file: std::fs::read(repo.get("config.json").await?)?,
                special_tokens_map_file: std::fs::read(repo.get("special_tokens_map.json").await?)?,
                tokenizer_config_file: std::fs::read(repo.get("tokenizer_config.json").await?)?,
            };
            let mut model = UserDefinedEmbeddingModel::new(weights, tokenizer_files)
                .with_quantization(TextEmbedding::get_quantization_mode(&model_name));
            if let Some(pooling) = TextEmbedding::get_default_pooling_method(&model_name) {
                model = model.with_pooling(pooling);
            }
            Ok(model)
        })
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
