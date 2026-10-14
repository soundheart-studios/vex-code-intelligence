//! The global embedding cache, VEX_HOME/cache/embeddings.db: Cortex's
//! `embedding_cache` table and key (sha256 of "model:text"), shared by every
//! project so identical content is embedded once per machine.

use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::Duration;

pub struct EmbeddingCache {
    conn: Connection,
}

/// Cortex `Embedder._key`.
pub fn key(model: &str, text: &str) -> String {
    Sha256::digest(format!("{model}:{text}").as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl EmbeddingCache {
    pub fn open(path: &Path) -> rusqlite::Result<EmbeddingCache> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(10))?;
        conn.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))?;
        conn.execute_batch(
            "PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS embedding_cache (
                 content_hash TEXT PRIMARY KEY,
                 dim          INTEGER NOT NULL,
                 embedding    BLOB NOT NULL
             );",
        )?;
        crate::db::private(path);
        Ok(EmbeddingCache { conn })
    }

    /// The cached vector for `key` if it has width `dim` (Cortex
    /// `_cache_get`: a row of another width is a stale miss).
    pub fn get(&self, key: &str, dim: usize) -> rusqlite::Result<Option<Vec<f32>>> {
        let row: Option<(i64, Vec<u8>)> = self
            .conn
            .prepare_cached("SELECT dim, embedding FROM embedding_cache WHERE content_hash = ?1")?
            .query_row([key], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        Ok(row
            .filter(|(d, blob)| *d == dim as i64 && blob.len() == dim * 4)
            .map(|(_, blob)| crate::vec::deserialize_f32(&blob)))
    }

    pub fn put_many(&self, rows: &[(String, &[f32])]) -> rusqlite::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt =
                tx.prepare_cached("INSERT OR REPLACE INTO embedding_cache VALUES (?1, ?2, ?3)")?;
            for (key, vec) in rows {
                stmt.execute(params![
                    key,
                    vec.len() as i64,
                    crate::vec::serialize_f32(vec)
                ])?;
            }
        }
        tx.commit()
    }

    pub fn len(&self) -> rusqlite::Result<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM embedding_cache", [], |r| r.get(0))
    }

    pub fn is_empty(&self) -> rusqlite::Result<bool> {
        Ok(self.len()? == 0)
    }
}
