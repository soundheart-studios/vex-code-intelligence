//! BM25 and vector retrieval, RRF fusion, the repomap and the rules-v1 selector.

pub mod fts;
pub mod fusion;
pub mod paths;
pub mod repomap;
pub mod retriever;
pub mod vector;

/// Python `round(x, 3)`: both sides round the exact binary value half to
/// even, so formatting to three places and parsing back agrees bit for bit.
pub fn round3(x: f64) -> f64 {
    format!("{x:.3}").parse().unwrap_or(x)
}
