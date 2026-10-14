//! The index pipeline against Cortex's own database, table by table and row
//! by row (rowid order), through an initial index, an unchanged re-run, an
//! edit, a deletion and an addition (recorded from Cortex in tests/fixtures/index/).

use rusqlite::Connection;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use vex_store::db::Index;
use vex_test_support::TestHome;

fn sha(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}

fn round6(x: f64) -> Value {
    json!((x * 1e6).round() / 1e6)
}

fn rows(c: &Connection, sql: &str, f: impl Fn(&rusqlite::Row) -> rusqlite::Result<Value>) -> Value {
    let mut stmt = c.prepare(sql).unwrap();
    Value::Array(
        stmt.query_map([], |r| f(r))
            .unwrap()
            .map(Result::unwrap)
            .collect(),
    )
}

fn cols(r: &rusqlite::Row, n: usize) -> rusqlite::Result<Value> {
    let mut out = Vec::new();
    for i in 0..n {
        out.push(match r.get_ref(i)? {
            rusqlite::types::ValueRef::Integer(v) => json!(v),
            rusqlite::types::ValueRef::Real(v) => json!(v),
            rusqlite::types::ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
            rusqlite::types::ValueRef::Null => Value::Null,
            rusqlite::types::ValueRef::Blob(_) => json!("<blob>"),
        });
    }
    Ok(Value::Array(out))
}

/// The same dump the recorded Cortex fixtures take.
fn dump(c: &Connection) -> Value {
    let mut edges: Vec<Value> = rows(c, "SELECT * FROM edges", |r| {
        Ok(json!([
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            round6(r.get(3)?),
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?
        ]))
    })
    .as_array()
    .unwrap()
    .clone();
    edges.sort_by_key(|e| e.to_string());
    let mut meta: Vec<Value> = rows(c, "SELECT key, value FROM meta", |r| cols(r, 2))
        .as_array()
        .unwrap()
        .iter()
        .filter(|kv| {
            let k = kv[0].as_str().unwrap();
            !matches!(k, "index_version" | "workspace_signature") && !k.starts_with("embedding_")
        })
        .cloned()
        .collect();
    meta.sort_by_key(|e| e.to_string());
    json!({
        "files": rows(c, "SELECT path, hash, language FROM files ORDER BY rowid", |r| cols(r, 3)),
        "chunks": rows(c, "SELECT id, file_path, start_line, end_line, kind, language, content, token_est, symbols, full_chars, parent_id FROM chunks ORDER BY rowid", |r| {
            let mut v = cols(r, 11)?;
            v[6] = json!(sha(v[6].as_str().unwrap()));
            Ok(v)
        }),
        "chunks_fts": rows(c, "SELECT rowid, id, content, file_path, symbols FROM chunks_fts ORDER BY rowid", |r| {
            let mut v = cols(r, 5)?;
            v[2] = json!(sha(v[2].as_str().unwrap()));
            Ok(v)
        }),
        "chunk_rowids": rows(c, "SELECT rowid, id FROM chunks ORDER BY rowid", |r| cols(r, 2)),
        "symbols": rows(c, "SELECT * FROM symbols ORDER BY rowid", |r| cols(r, 6)),
        "symbol_sites": rows(c, "SELECT * FROM symbol_sites ORDER BY rowid", |r| cols(r, 7)),
        "refs": rows(c, "SELECT * FROM refs ORDER BY rowid", |r| cols(r, 3)),
        "import_bindings": rows(c, "SELECT * FROM import_bindings ORDER BY rowid", |r| cols(r, 4)),
        "calls": rows(c, "SELECT * FROM calls ORDER BY rowid", |r| cols(r, 4)),
        "member_types": rows(c, "SELECT * FROM member_types ORDER BY rowid", |r| cols(r, 4)),
        "edges": edges,
        "calls_edges_in_rowid_order": rows(c, "SELECT src, dst, confidence FROM edges WHERE type = 'CALLS' ORDER BY rowid", |r| {
            Ok(json!([r.get::<_, String>(0)?, r.get::<_, String>(1)?, round6(r.get(2)?)]))
        }),
        "meta": meta,
    })
}

fn oracle_dump(step: &Value) -> Value {
    let mut d = step["dump"].clone();
    let meta: Vec<Value> = d["meta"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|kv| !kv[0].as_str().unwrap().starts_with("embedding_"))
        .cloned()
        .collect();
    d["meta"] = json!(meta);
    d
}

fn corpora() -> Option<PathBuf> {
    let root = std::env::var_os("VEX_CORTEX_CORPORA")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Documents/cortex-corpora"))
        })?;
    root.is_dir().then_some(root)
}

fn compare(step: &str, got: &Value, want: &Value) -> Vec<String> {
    let mut diffs = Vec::new();
    for key in want.as_object().unwrap().keys() {
        if got[key] != want[key] {
            let (g, w) = (got[key].as_array().unwrap(), want[key].as_array().unwrap());
            let first = g
                .iter()
                .zip(w)
                .position(|(a, b)| a != b)
                .unwrap_or(g.len().min(w.len()));
            diffs.push(format!(
                "{step} / {key}: {} rows vs Cortex {}; first difference at {first}: vex {} vs Cortex {}",
                g.len(),
                w.len(),
                g.get(first).map_or("-".into(), |v| v.to_string()),
                w.get(first).map_or("-".into(), |v| v.to_string())
            ));
        }
    }
    diffs
}

#[test]
fn every_step_of_an_itsdangerous_index_equals_cortexs_database() {
    let Some(corpora) = corpora() else {
        if std::env::var_os("CI").is_some() {
            eprintln!("SKIPPED: the Cortex corpora are not available in CI");
            return;
        }
        panic!("the Cortex corpora are missing; set VEX_CORTEX_CORPORA");
    };
    let oracle: Value = serde_json::from_slice(
        &std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/index/itsdangerous_steps.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let h = TestHome::new();
    let root = h.vex_home().join("src");
    let package = corpora.join("graph/packages/itsdangerous");
    for rel in oracle["package_files"].as_array().unwrap() {
        let rel = rel.as_str().unwrap();
        let dst = root.join(rel);
        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
        std::fs::copy(package.join(rel), dst).unwrap();
    }
    let idx = Index::open(&h.vex_home().join("index.db")).unwrap();
    let steps = oracle["steps"].as_array().unwrap();
    let mut diffs = Vec::new();
    for step in steps {
        let name = step["step"].as_str().unwrap();
        match name {
            "edit encoding.py" => {
                let p = root.join("src/itsdangerous/encoding.py");
                let text = std::fs::read_to_string(&p).unwrap();
                std::fs::write(
                    &p,
                    format!("{text}\n\ndef added_helper(value):\n    return want_bytes(value)\n"),
                )
                .unwrap();
            }
            "delete exc.py" => std::fs::remove_file(root.join("src/itsdangerous/exc.py")).unwrap(),
            "add extra.py" => std::fs::write(
                root.join("src/itsdangerous/extra.py"),
                "from .encoding import added_helper\n\ndef use():\n    return added_helper(b'x')\n",
            )
            .unwrap(),
            _ => {}
        }
        let report = vex_app::index::run(&idx, &root, false).unwrap();
        let want = &step["report"];
        let got = json!({
            "files_scanned": report.files_scanned, "added": report.added, "modified": report.modified,
            "removed": report.removed, "chunks_ingested": report.chunks_ingested,
            "ref_edges": report.ref_edges, "call_edges": report.call_edges,
        });
        if &got != want {
            diffs.push(format!("{name} / report: vex {got} vs Cortex {want}"));
        }
        diffs.extend(compare(name, &dump(&idx.conn), &oracle_dump(step)));
    }
    assert_eq!(steps.len(), 5);
    assert!(diffs.is_empty(), "{}", diffs.join("\n"));
}

/// A crafted project shipped inside the fixture (so this also runs in CI):
/// non-ASCII content (characters vs bytes), a name defined in four files,
/// runtime-name references with and without an import, an interleaved
/// duplicate edge, chunks at exactly 400 and just over 400 tokens, and a
/// chunk that would form a single window.
#[test]
fn a_crafted_project_index_equals_cortexs_database() {
    let oracle: Value = serde_json::from_slice(
        &std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/index/crafted_project.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let h = TestHome::new();
    let root = h.vex_home().join("src");
    for (rel, text) in oracle["files"].as_object().unwrap() {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text.as_str().unwrap()).unwrap();
    }
    let idx = Index::open(&h.vex_home().join("index.db")).unwrap();
    let step = &oracle["steps"][0];
    let report = vex_app::index::run(&idx, &root, false).unwrap();
    let got = json!({
        "files_scanned": report.files_scanned, "added": report.added, "modified": report.modified,
        "removed": report.removed, "chunks_ingested": report.chunks_ingested,
        "ref_edges": report.ref_edges, "call_edges": report.call_edges,
    });
    let mut diffs = Vec::new();
    if got != step["report"] {
        diffs.push(format!("report: vex {got} vs Cortex {}", step["report"]));
    }
    diffs.extend(compare("crafted", &dump(&idx.conn), &oracle_dump(step)));
    assert!(diffs.is_empty(), "{}", diffs.join("\n"));
}
