//! The vector channel (Cortex `ChunkStore.vector_search`): the `k` chunks
//! nearest a query embedding, best first, as the retriever's semantic
//! input. The width guard is the store's; any other failure answers
//! nothing, as Cortex logs and returns [] on a broken vec table.

use crate::retriever::{ChunkRow, chunks_by_rowids};
use vex_store::db::Index;
use vex_store::vec::{self, VecError};

/// Nearest chunks to `embedding`, best first. Err only for a width that
/// differs from the index's.
pub fn search(idx: &Index, embedding: &[f32], k: usize) -> Result<Vec<ChunkRow>, VecError> {
    let ranked = match vec::search(idx, embedding, k) {
        Ok(r) => r,
        Err(e @ VecError::Width { .. }) => return Err(e),
        Err(VecError::Sql(_)) => return Ok(Vec::new()),
    };
    let rowids: Vec<i64> = ranked.iter().map(|(r, _)| *r).collect();
    Ok(chunks_by_rowids(&idx.conn, &rowids).unwrap_or_default())
}
