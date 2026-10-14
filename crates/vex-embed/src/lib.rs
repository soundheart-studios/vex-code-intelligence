//! Natively hosted embeddings (bge-small through statically linked ONNX Runtime) and the deterministic hash backend.
//!
//! Cortex's `indexer/embedder.py` minus its ladder: vex hosts one model,
//! bge-small-en-v1.5, in-process, and keeps the hash backend so hybrid
//! search stays deterministic and testable offline. The content-hash cache
//! lives with the store (6.2), not here.

pub mod batching;
mod bge;
mod hash;
mod idle;
pub mod weights;

pub use bge::{BGE_DIM, BGE_MODEL, BgeEmbedder, CONTEXT_TOKENS};
pub use hash::{HASH_DIM, HashEmbedder, hash_embed};
pub use idle::IdleSlot;

/// An embedding backend: a fixed model, a fixed width, a batch in and one
/// vector per text out, in order.
pub trait Embedder: Send + Sync {
    /// The name cache keys and index provenance record (Cortex's `model_name`).
    fn model_name(&self) -> &str;
    /// The width of every vector this backend produces.
    fn dim(&self) -> usize;
    /// The model's token window, None for a backend with no limit.
    fn context_tokens(&self) -> Option<usize>;
    /// How many texts a caller should hand `embed` at once: enough for
    /// every session a parallel backend runs.
    fn preferred_batch(&self) -> usize {
        64
    }
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError>;
}

#[derive(Debug)]
pub enum EmbedError {
    /// The weights could not be fetched or failed verification.
    Weights(weights::WeightsError),
    /// The model loaded badly or failed to run.
    Model(String),
}

impl std::fmt::Display for EmbedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EmbedError::Weights(e) => write!(f, "{e}"),
            EmbedError::Model(m) => write!(f, "embedding model failed: {m}"),
        }
    }
}

impl std::error::Error for EmbedError {}

impl From<weights::WeightsError> for EmbedError {
    fn from(e: weights::WeightsError) -> Self {
        EmbedError::Weights(e)
    }
}
