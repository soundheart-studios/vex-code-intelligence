//! One project's SQLite index: a port of Cortex's `store/db.py` connection
//! handling (WAL, pragmas, schema, additive migrations), without the memory
//! and ledger tables (ADR 1).
//!
//! The schema is Cortex's own DDL, so vex can open an index Cortex built and
//! Cortex can open one vex built.

use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};

/// The DDL (Cortex's schema.sql prefix plus vex_ tables).
pub const SCHEMA_SQL: &str = include_str!("schema.sql");

/// The vex schema version recorded in `vex_schema`.
pub const SCHEMA_VERSION: i64 = 1;

/// Cortex's additive column migrations (table, column, ADD COLUMN clause),
/// applied when an older index is opened. Entries for tables vex does not
/// create are skipped exactly as Cortex skips absent tables.
const MIGRATIONS: &[(&str, &str, &str)] = &[
    ("chunks", "symbols", "symbols TEXT NOT NULL DEFAULT ''"),
    (
        "chunks",
        "full_chars",
        "full_chars INTEGER NOT NULL DEFAULT 0",
    ),
    ("chunks", "parent_id", "parent_id TEXT NOT NULL DEFAULT ''"),
];

const FTS_ROWID_KEY: &str = "fts_rowid_aligned";

/// Which backend, model and width wrote the stored vectors (Cortex
/// `embedding_provenance`). `dim` is the width `chunks_vec` was created at
/// (`embedding_dim`, owned by `vec::ensure_table`); `expected_dim` is the
/// width the recording backend declares, written even before any vector.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Provenance {
    pub backend: Option<String>,
    pub model: Option<String>,
    pub dim: Option<i64>,
    pub expected_dim: Option<i64>,
}

pub struct Index {
    pub conn: Connection,
    pub path: PathBuf,
}

impl Index {
    /// Open (creating if needed) the index at `path`.
    pub fn open(path: &Path) -> rusqlite::Result<Index> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        crate::vec::register();
        let conn = Connection::open(path)?;
        // journal_mode returns its new value as a row.
        conn.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))?;
        conn.execute_batch("PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;")?;
        conn.execute_batch(SCHEMA_SQL)?;
        let index = Index {
            conn,
            path: path.to_path_buf(),
        };
        index.migrate()?;
        private(path);
        Ok(index)
    }

    /// Bring an existing database up to the current schema (Cortex
    /// `_migrate`, minus the drops of Cortex-internal tables vex never had).
    fn migrate(&self) -> rusqlite::Result<()> {
        for (table, column, clause) in MIGRATIONS {
            let cols = self.columns(table)?;
            if cols.is_empty() || cols.iter().any(|c| c == column) {
                continue;
            }
            self.conn
                .execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {clause}"))?;
        }
        // ADR 19: an index built before symbol_sites existed gets one site
        // per symbols row, once.
        let sites_empty = self
            .conn
            .query_row("SELECT 1 FROM symbol_sites LIMIT 1", [], |_| Ok(()))
            .optional()?
            .is_none();
        let symbols_any = self
            .conn
            .query_row("SELECT 1 FROM symbols LIMIT 1", [], |_| Ok(()))
            .optional()?
            .is_some();
        if sites_empty && symbols_any {
            self.conn.execute_batch(
                "INSERT OR IGNORE INTO symbol_sites \
                 SELECT qualified_name, name, kind, file_path, start_line, end_line, 0 FROM symbols",
            )?;
        }
        self.align_fts_rowids()?;
        let version: Option<i64> = self
            .conn
            .query_row("SELECT version FROM vex_schema LIMIT 1", [], |r| r.get(0))
            .optional()?;
        if version.is_none() {
            self.conn.execute(
                "INSERT INTO vex_schema (version) VALUES (?1)",
                params![SCHEMA_VERSION],
            )?;
        }
        Ok(())
    }

    /// Cortex `_align_fts_rowids`: re-key chunks_fts to chunks.rowid once per
    /// database, so deletes by rowid hit the right row.
    fn align_fts_rowids(&self) -> rusqlite::Result<()> {
        let done: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                [FTS_ROWID_KEY],
                |r| r.get(0),
            )
            .optional()?;
        if done.is_some() {
            return Ok(());
        }
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))?;
        let skewed: bool = n > 0
            && self.conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM chunks c JOIN chunks_fts f ON f.rowid = c.rowid WHERE f.id <> c.id) \
                 OR (SELECT COUNT(*) FROM chunks_fts) <> (SELECT COUNT(*) FROM chunks)",
                [],
                |r| r.get(0),
            )?;
        if skewed {
            self.conn.execute_batch(
                "DELETE FROM chunks_fts; \
                 INSERT INTO chunks_fts (rowid, id, content, file_path, symbols) \
                 SELECT rowid, id, content, file_path, symbols FROM chunks;",
            )?;
        }
        self.conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, '1')",
            [FTS_ROWID_KEY],
        )?;
        Ok(())
    }

    pub fn columns(&self, table: &str) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare(&format!("PRAGMA table_info('{table}')"))?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
        rows.collect()
    }

    pub fn schema_version(&self) -> rusqlite::Result<i64> {
        self.conn
            .query_row("SELECT version FROM vex_schema LIMIT 1", [], |r| r.get(0))
    }

    /// Record which backend, model and width write this index's vectors
    /// (Cortex `record_embedding_provenance`). Deliberately not
    /// `embedding_dim`: that key means a `chunks_vec` of that width exists.
    pub fn record_embedding_provenance(
        &self,
        backend: &str,
        model: &str,
        dim: i64,
    ) -> rusqlite::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for (k, v) in [
            ("embedding_backend", backend.to_string()),
            ("embedding_model", model.to_string()),
            ("embedding_expected_dim", dim.to_string()),
        ] {
            tx.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
                params![k, v],
            )?;
        }
        tx.commit()
    }

    pub fn embedding_provenance(&self) -> rusqlite::Result<Provenance> {
        let get = |k: &str| -> rusqlite::Result<Option<String>> {
            self.conn
                .query_row("SELECT value FROM meta WHERE key = ?1", [k], |r| r.get(0))
                .optional()
        };
        Ok(Provenance {
            backend: get("embedding_backend")?,
            model: get("embedding_model")?,
            dim: get("embedding_dim")?.and_then(|d| d.parse().ok()),
            expected_dim: get("embedding_expected_dim")?.and_then(|d| d.parse().ok()),
        })
    }
}

/// The index holds derived source code: owner-only (0600).
pub(crate) fn private(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}
