//! Bounded, content-addressed timeline playback previews (33).
//!
//! Cache entries are an optional playback source. Native exports never receive
//! this cache and continue to render from their frozen original-media snapshot.

mod cache;
mod key;
mod runtime;
mod worker;

pub use cache::{
    CacheConfig, CacheStats, ChunkState, ChunkStatus, PreviewCache, PreviewChunk,
    PreviewStatusSnapshot,
};
pub use key::{
    chunks_for_range, ChunkKey, ChunkSignature, ChunkSpan, ChunkSpans, PreviewCodec,
    PreviewContext, PreviewProfile, PreviewScale,
};
pub use runtime::{PreviewRuntime, SourceDependency};
pub use worker::{PlaybackPriorityGate, PreviewChunkRequest, PreviewJobId, PreviewWorker};

#[derive(Debug, thiserror::Error)]
pub enum PreviewError {
    #[error("invalid preview request: {0}")]
    Invalid(String),
    #[error("preview cache I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid preview manifest: {0}")]
    Manifest(#[from] serde_json::Error),
    #[error("preview render failed: {0}")]
    Render(#[from] crate::export::render_loop::ExportError),
    #[error("preview cancelled")]
    Cancelled,
    #[error("preview cache budget exhausted")]
    BudgetExhausted,
    #[error("preview worker queue is full")]
    QueueFull,
    #[error("preview worker has stopped")]
    Stopped,
}

#[cfg(test)]
mod tests;
