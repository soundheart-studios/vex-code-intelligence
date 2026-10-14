//! The vex schema is Cortex's minus the dropped tables (ADR 1), column for
//! column, against Cortex's own freshly created database
//! (tests/fixtures/cortex_schema.json, recorded from Cortex).

use rusqlite::Connection;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use vex_store::db::{Index, SCHEMA_VERSION};
use vex_test_support::TestHome;

/// Cortex tables vex deliberately does not create (memory, ledger, the
/// per-project embedding cache) and their FTS shadow tables.
fn dropped(table: &str) -> bool {
    matches!(
        table,
        "memory" | "memory_symbols" | "ledger" | "embedding_cache"
    ) || table.starts_with("memory_fts")
}

fn oracle() -> Value {
    serde_json::from_str(include_str!("fixtures/cortex_schema.json")).unwrap()
}

fn fresh(h: &TestHome) -> Index {
    Index::open(&h.vex_home().join("p/index.db")).unwrap()
}

fn table_info(conn: &Connection, table: &str) -> Value {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info('{table}')"))
        .unwrap();
    let rows: Vec<Value> = stmt
        .query_map([], |r| {
            Ok(json!([
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, i64>(5)?
            ]))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    Value::Array(rows)
}

fn names(conn: &Connection, sql: &str) -> BTreeSet<String> {
    let mut stmt = conn.prepare(sql).unwrap();
    stmt.query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn every_kept_cortex_table_has_identical_columns() {
    let h = TestHome::new();
    let idx = fresh(&h);
    let o = oracle();
    let mut kept = 0;
    for (table, cols) in o["tables"].as_object().unwrap() {
        if dropped(table) {
            continue;
        }
        kept += 1;
        assert_eq!(
            &table_info(&idx.conn, table),
            cols,
            "{table}: columns differ from Cortex"
        );
    }
    assert!(kept >= 15, "compared {kept} tables");
}

#[test]
fn the_table_set_is_cortexs_minus_the_dropped_ones_plus_vex_tables() {
    let h = TestHome::new();
    let idx = fresh(&h);
    let want: BTreeSet<String> = oracle()["tables"]
        .as_object()
        .unwrap()
        .keys()
        .filter(|t| !dropped(t))
        .cloned()
        .collect();
    let got: BTreeSet<String> = names(
        &idx.conn,
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
    )
    .into_iter()
    .filter(|t| !t.starts_with("vex_"))
    .collect();
    assert_eq!(got, want);
}

#[test]
fn indexes_and_the_fts5_definition_are_cortexs() {
    let h = TestHome::new();
    let idx = fresh(&h);
    let o = oracle();
    for (name, sql) in o["indexes"].as_object().unwrap() {
        if name.contains("memory") {
            continue;
        }
        let got: String = idx
            .conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name = ?1",
                [name],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(&json!(got), sql, "{name}");
    }
    let fts: String = idx
        .conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = 'chunks_fts'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(json!(fts), o["virtual"]["chunks_fts"]);
}

#[test]
fn pragmas_match_cortex() {
    let h = TestHome::new();
    let idx = fresh(&h);
    let o = oracle();
    for p in ["journal_mode", "synchronous", "foreign_keys"] {
        let got: Value = idx
            .conn
            .query_row(&format!("PRAGMA {p}"), [], |r| {
                Ok(match r.get_ref(0)? {
                    rusqlite::types::ValueRef::Integer(i) => json!(i),
                    v => json!(String::from_utf8_lossy(v.as_str().unwrap().as_bytes())),
                })
            })
            .unwrap();
        assert_eq!(got, o["pragmas"][p], "PRAGMA {p}");
    }
}

#[test]
fn the_schema_version_is_recorded_once() {
    let h = TestHome::new();
    let path = h.vex_home().join("p/index.db");
    assert_eq!(
        Index::open(&path).unwrap().schema_version().unwrap(),
        SCHEMA_VERSION
    );
    let again = Index::open(&path).unwrap();
    let n: i64 = again
        .conn
        .query_row("SELECT COUNT(*) FROM vex_schema", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn an_older_index_is_migrated_like_cortex_does() {
    // A chunks table from before symbols/full_chars/parent_id existed, and
    // symbols with no sites: Cortex adds the columns and backfills sites.
    let h = TestHome::new();
    let path = h.vex_home().join("old/index.db");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    {
        let c = Connection::open(&path).unwrap();
        c.execute_batch(
            "CREATE TABLE chunks (id TEXT PRIMARY KEY, file_path TEXT NOT NULL, start_line INTEGER NOT NULL, \
             end_line INTEGER NOT NULL, kind TEXT NOT NULL DEFAULT 'block', language TEXT NOT NULL DEFAULT '', \
             content TEXT NOT NULL, token_est INTEGER NOT NULL DEFAULT 0); \
             CREATE TABLE symbols (qualified_name TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL, \
             file_path TEXT NOT NULL, start_line INTEGER NOT NULL, end_line INTEGER NOT NULL); \
             INSERT INTO symbols VALUES ('a.py.f', 'f', 'function', 'a.py', 1, 2);",
        )
        .unwrap();
    }
    let idx = Index::open(&path).unwrap();
    let cols = idx.columns("chunks").unwrap();
    for c in ["symbols", "full_chars", "parent_id"] {
        assert!(cols.iter().any(|x| x == c), "{c} added");
    }
    let sites: i64 = idx
        .conn
        .query_row(
            "SELECT COUNT(*) FROM symbol_sites WHERE qualified_name = 'a.py.f'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(sites, 1);
}

#[test]
fn a_cortex_index_with_memory_tables_opens_and_keeps_them() {
    let h = TestHome::new();
    let path = h.vex_home().join("cortex/index.db");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    Connection::open(&path)
        .unwrap()
        .execute_batch("CREATE TABLE ledger (ts REAL NOT NULL); INSERT INTO ledger VALUES (1.0);")
        .unwrap();
    let idx = Index::open(&path).unwrap();
    let n: i64 = idx
        .conn
        .query_row("SELECT COUNT(*) FROM ledger", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1, "vex ignores tables it does not own");
}

#[test]
fn embedding_provenance_round_trips_through_meta() {
    let h = TestHome::new();
    let idx = fresh(&h);
    assert_eq!(idx.embedding_provenance().unwrap().dim, None);
    idx.record_embedding_provenance("fastembed", "BAAI/bge-small-en-v1.5", 384)
        .unwrap();
    let p = idx.embedding_provenance().unwrap();
    assert_eq!(
        (p.backend.as_deref(), p.model.as_deref(), p.expected_dim),
        (Some("fastembed"), Some("BAAI/bge-small-en-v1.5"), Some(384))
    );
    // Cortex keeps `embedding_dim` for "a chunks_vec of this width exists";
    // provenance never claims a table it did not create.
    assert_eq!(p.dim, None);
}

#[cfg(unix)]
#[test]
fn the_index_file_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let h = TestHome::new();
    let idx = fresh(&h);
    assert_eq!(
        std::fs::metadata(&idx.path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn fts5_is_compiled_into_the_bundled_sqlite() {
    let h = TestHome::new();
    let idx = fresh(&h);
    idx.conn.execute("INSERT INTO chunks_fts (id, content, file_path, symbols) VALUES ('a', 'verify session token', 'f', '')", []).unwrap();
    let n: i64 = idx
        .conn
        .query_row(
            "SELECT COUNT(*) FROM chunks_fts WHERE chunks_fts MATCH 'session'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}
