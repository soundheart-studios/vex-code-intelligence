//! Cortex `Embedder._hash_embed`: feature hashing over identifiers.
//! Deterministic and offline; NOT semantically meaningful.

use crate::{EmbedError, Embedder};
use md5::{Digest, Md5};

pub const HASH_DIM: usize = 256;

/// The vector Cortex computes, in f64 as Python holds it: `text.lower()`,
/// every `[A-Za-z_][A-Za-z0-9_]+` run, `int(md5(tok).hexdigest()[:8], 16)
/// % 256` incremented, then divided by the L2 norm (1.0 when empty).
pub fn hash_embed(text: &str) -> Vec<f64> {
    let mut vec = vec![0f64; HASH_DIM];
    // Python's str.lower() is full Unicode lowercasing; a few non-ASCII
    // letters lower into ASCII (U+0130 -> "i\u{307}", U+212A -> "k").
    let lowered = text.to_lowercase();
    let bytes = lowered.as_bytes();
    let head = |b: u8| b.is_ascii_alphabetic() || b == b'_';
    let tail = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut i = 0;
    while i < bytes.len() {
        if !head(bytes[i]) {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < bytes.len() && tail(bytes[j]) {
            j += 1;
        }
        if j - i < 2 {
            i += 1;
            continue;
        }
        let digest = Md5::digest(&bytes[i..j]);
        let h = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]);
        vec[h as usize % HASH_DIM] += 1.0;
        i = j;
    }
    // Components are whole counts, so the sum of squares is exact in any
    // order (Python 3.12's compensated sum() included).
    let sum: f64 = vec.iter().map(|x| x * x).sum();
    let norm = if sum == 0.0 { 1.0 } else { sum.sqrt() };
    vec.iter().map(|x| x / norm).collect()
}

/// The hash backend behind the Embedder trait: Cortex's `hash-256`.
pub struct HashEmbedder;

impl Embedder for HashEmbedder {
    fn model_name(&self) -> &str {
        "hash-256"
    }
    fn dim(&self) -> usize {
        HASH_DIM
    }
    fn context_tokens(&self) -> Option<usize> {
        None
    }
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        Ok(texts
            .iter()
            .map(|t| hash_embed(t).into_iter().map(|x| x as f32).collect())
            .collect())
    }
}
