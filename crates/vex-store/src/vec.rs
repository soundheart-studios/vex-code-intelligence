//! Vectors: sqlite-vec 0.1.9 linked statically and registered for every
//! connection, the lazily created `chunks_vec` table, and the width guard.
//! A port of Cortex `Database.ensure_vec_table`, `assert_embedding_compatible`
//! and the vector half of `ChunkStore` (`vector_search`, `pending`,
//! `set_embeddings`), with the dimension-mismatch failure naming both widths
//! and the rebuild command.

use crate::db::Index;
use rusqlite::{Connection, OptionalExtension, params};
use std::sync::Once;

unsafe extern "C" {
    /// sqlite-vec's entry point (vendor/sqlite-vec, built by build.rs).
    fn sqlite3_vec_init(
        db: *mut rusqlite::ffi::sqlite3,
        err: *mut *mut std::os::raw::c_char,
        api: *const rusqlite::ffi::sqlite3_api_routines,
    ) -> std::os::raw::c_int;
}

/// The build flags of the linked sqlite-vec (`vec_debug()`'s last line),
/// e.g. "neon" on aarch64 as the official wheels are built.
pub fn build_flags(conn: &Connection) -> rusqlite::Result<String> {
    let debug: String = conn.query_row("SELECT vec_debug()", [], |r| r.get(0))?;
    Ok(debug
        .lines()
        .find_map(|l| l.strip_prefix("Build flags:"))
        .unwrap_or("")
        .trim()
        .to_string())
}

/// Register sqlite-vec as an auto extension: every connection opened after
/// this has `vec0` and `vec_version()`. Idempotent.
pub fn register() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        // SAFETY: sqlite3_vec_init has exactly the extension entry-point
        // signature sqlite3_auto_extension expects.
        rusqlite::ffi::sqlite3_auto_extension(Some(sqlite3_vec_init));
    });
}

/// The linked sqlite-vec's version string, e.g. "v0.1.9".
pub fn version(conn: &Connection) -> rusqlite::Result<String> {
    conn.query_row("SELECT vec_version()", [], |r| r.get(0))
}

/// Cortex `serialize_f32`: packed native-endian float32.
pub fn serialize_f32(vec: &[f32]) -> Vec<u8> {
    vec.iter().flat_map(|x| x.to_ne_bytes()).collect()
}

pub fn deserialize_f32(blob: &[u8]) -> Vec<f32> {
    blob.as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_ne_bytes(*b))
        .collect()
}

#[derive(Debug)]
pub enum VecError {
    /// The index stores vectors of one width and the embedder produces another.
    Width {
        stored: i64,
        produced: usize,
        model: Option<String>,
        backend: Option<String>,
        rebuild_refused: bool,
    },
    Sql(rusqlite::Error),
}

impl std::fmt::Display for VecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VecError::Width {
                stored,
                produced,
                model,
                backend,
                rebuild_refused,
            } => {
                let model = model.as_deref().unwrap_or("unknown");
                let backend = backend.as_deref().unwrap_or("unknown");
                if *rebuild_refused {
                    write!(
                        f,
                        "refusing to rebuild this index from {stored} to {produced} dimensions: \
                         it was built with {stored}-dimension vectors (backend {backend}, model \
                         {model}) and a writer produced {produced}-dimension ones. Rebuilding here \
                         would delete every chunk. Check `vex doctor`, then rebuild on purpose \
                         with: vex index --force"
                    )
                } else {
                    write!(
                        f,
                        "this index stores {stored}-dimension vectors (backend {backend}, model \
                         {model}) but the embedding backend produces {produced}-dimension vectors. \
                         The index was built by a different backend. Rebuild with: vex index --force"
                    )
                }
            }
            VecError::Sql(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for VecError {}

impl From<rusqlite::Error> for VecError {
    fn from(e: rusqlite::Error) -> Self {
        VecError::Sql(e)
    }
}

/// The width of `chunks_vec`, None before it exists (Cortex `Database.dim`:
/// re-read every time, so a rebuild by another process is seen).
pub fn dim(conn: &Connection) -> rusqlite::Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'embedding_dim'",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .and_then(|v| v.parse().ok()))
}

fn width_error(idx: &Index, stored: i64, produced: usize, rebuild_refused: bool) -> VecError {
    let prov = idx.embedding_provenance().unwrap_or_default();
    VecError::Width {
        stored,
        produced,
        model: prov.model,
        backend: prov.backend,
        rebuild_refused,
    }
}

/// Cortex `assert_embedding_compatible`: refuse an index whose stored width
/// differs from the embedder's, before anything is written.
pub fn assert_compatible(idx: &Index, width: usize) -> Result<(), VecError> {
    match dim(&idx.conn)? {
        Some(stored) if stored != width as i64 => Err(width_error(idx, stored, width, false)),
        _ => Ok(()),
    }
}

/// Cortex `ensure_vec_table`: create `chunks_vec` at `width`. A different
/// existing width is rebuilt (dropping every chunk, as Cortex does) only
/// when `allow_rebuild`; otherwise it is refused.
pub fn ensure_table(idx: &Index, width: usize, allow_rebuild: bool) -> Result<(), VecError> {
    let current = dim(&idx.conn)?;
    if current == Some(width as i64) {
        return Ok(());
    }
    if let Some(stored) = current {
        if !allow_rebuild {
            return Err(width_error(idx, stored, width, true));
        }
        idx.conn.execute_batch(
            "DROP TABLE IF EXISTS chunks_vec; DELETE FROM chunks; DELETE FROM chunks_fts;",
        )?;
    }
    idx.conn.execute_batch(&format!(
        "CREATE VIRTUAL TABLE IF NOT EXISTS chunks_vec USING vec0(embedding float[{width}])"
    ))?;
    idx.conn.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('embedding_dim', ?1)",
        [width.to_string()],
    )?;
    Ok(())
}

/// Store one chunk's vector (Cortex ingest: delete, then insert).
pub fn put(conn: &Connection, rowid: i64, vec: &[f32]) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM chunks_vec WHERE rowid = ?1", [rowid])?;
    conn.execute(
        "INSERT INTO chunks_vec (rowid, embedding) VALUES (?1, ?2)",
        params![rowid, serialize_f32(vec)],
    )?;
    Ok(())
}

/// Cortex `set_embeddings`: fill vectors for existing chunks.
pub fn set_embeddings(idx: &Index, rows: &[(i64, Vec<f32>)]) -> Result<(), VecError> {
    let Some((_, first)) = rows.first() else {
        return Ok(());
    };
    ensure_table(idx, first.len(), false)?;
    let tx = idx.conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT OR REPLACE INTO chunks_vec (rowid, embedding) VALUES (?1, ?2)",
        )?;
        for (rowid, vec) in rows {
            stmt.execute(params![rowid, serialize_f32(vec)])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Cortex `count_pending`: chunks with no vector yet (all of them before
/// `chunks_vec` exists).
pub fn count_pending(conn: &Connection) -> rusqlite::Result<i64> {
    if dim(conn)?.is_none() {
        return conn.query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0));
    }
    conn.query_row(
        "SELECT COUNT(*) FROM chunks WHERE rowid NOT IN (SELECT rowid FROM chunks_vec)",
        [],
        |r| r.get(0),
    )
}

/// Cortex `pending`: up to `limit` (rowid, content) awaiting vectors.
pub fn pending(conn: &Connection, limit: usize) -> rusqlite::Result<Vec<(i64, String)>> {
    let sql = if dim(conn)?.is_none() {
        "SELECT rowid, content FROM chunks LIMIT ?1"
    } else {
        "SELECT rowid, content FROM chunks WHERE rowid NOT IN (SELECT rowid FROM chunks_vec) LIMIT ?1"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([limit as i64], |r| Ok((r.get(0)?, r.get(1)?)))?;
    rows.collect()
}

/// `pending`, longest content first: what `vex embed` drains, so each
/// model batch holds texts of like length and pads least.
pub fn pending_longest(conn: &Connection, limit: usize) -> rusqlite::Result<Vec<(i64, String)>> {
    let sql = if dim(conn)?.is_none() {
        "SELECT rowid, content FROM chunks ORDER BY length(content) DESC, rowid LIMIT ?1"
    } else {
        "SELECT rowid, content FROM chunks WHERE rowid NOT IN (SELECT rowid FROM chunks_vec) \
         ORDER BY length(content) DESC, rowid LIMIT ?1"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([limit as i64], |r| Ok((r.get(0)?, r.get(1)?)))?;
    rows.collect()
}

/// The pending chunks' total length, each capped at `cap` characters:
/// what is left to embed, in the units a timed sample is measured in.
pub fn pending_cost(conn: &Connection, cap: i64) -> rusqlite::Result<i64> {
    let sql = if dim(conn)?.is_none() {
        "SELECT COALESCE(SUM(MIN(length(content), ?1)), 0) FROM chunks"
    } else {
        "SELECT COALESCE(SUM(MIN(length(content), ?1)), 0) FROM chunks \
         WHERE rowid NOT IN (SELECT rowid FROM chunks_vec)"
    };
    conn.query_row(sql, [cap], |r| r.get(0))
}

/// Cortex `vector_search`: the `k` nearest chunk rowids by distance, best
/// first. A query of the wrong width is refused, naming both widths; an
/// index without vectors answers nothing.
pub fn search(idx: &Index, embedding: &[f32], k: usize) -> Result<Vec<(i64, f64)>, VecError> {
    let Some(stored) = dim(&idx.conn)? else {
        return Ok(Vec::new());
    };
    if stored != embedding.len() as i64 {
        return Err(width_error(idx, stored, embedding.len(), false));
    }
    let mut stmt = idx.conn.prepare_cached(
        "SELECT rowid, distance FROM chunks_vec WHERE embedding MATCH ?1 AND k = ?2 ORDER BY distance",
    )?;
    let rows = stmt.query_map(params![serialize_f32(embedding), k as i64], |r| {
        Ok((r.get(0)?, r.get(1)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}
