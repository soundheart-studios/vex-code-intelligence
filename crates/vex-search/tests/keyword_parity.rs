//! Keyword retrieval against Cortex: the chunk rows of a Cortex index are
//! loaded into a vex index with their rowids (so FTS5 statistics match), and
//! every recorded query must give Cortex's slate, scores, channels and
//! diagnostics. Fixtures: tests/fixtures/keyword/, recorded from Cortex.

use serde_json::{Value, json};
use std::path::Path;
use vex_search::retriever::{self, Hit};
use vex_store::db::Index;
use vex_test_support::TestHome;

fn fixture(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/keyword")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

fn load(home: &TestHome, corpus: &Value) -> Index {
    let idx = Index::open(&home.vex_home().join("index.db")).unwrap();
    let tx = idx.conn.unchecked_transaction().unwrap();
    for r in corpus["rows"].as_array().unwrap() {
        tx.execute(
            "INSERT INTO chunks (rowid, id, file_path, start_line, end_line, kind, language, \
             content, token_est, symbols, full_chars, parent_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            rusqlite::params![
                r["rowid"].as_i64(),
                r["id"].as_str(),
                r["file_path"].as_str(),
                r["start_line"].as_i64(),
                r["end_line"].as_i64(),
                r["kind"].as_str(),
                r["language"].as_str(),
                r["content"].as_str(),
                r["token_est"].as_i64(),
                r["symbols"].as_str(),
                r["full_chars"].as_i64(),
                r["parent_id"].as_str(),
            ],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO chunks_fts (rowid, id, content, file_path, symbols) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                r["rowid"].as_i64(),
                r["id"].as_str(),
                r["content"].as_str(),
                r["file_path"].as_str(),
                r["symbols"].as_str(),
            ],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    idx
}

fn hit_json(h: &Hit) -> Value {
    let mut v = json!({
        "id": h.chunk.id,
        "file_path": h.chunk.file_path,
        "start_line": h.chunk.start_line,
        "end_line": h.chunk.end_line,
        "score": h.score,
        "channels": h.channels,
    });
    if !h.ranking_signals.is_empty() {
        v["ranking_signals"] = json!(h.ranking_signals);
    }
    v
}

fn check_corpus(name: &str) {
    let home = TestHome::new();
    let corpus = fixture(&format!("{name}.json"));
    let idx = load(&home, &corpus);
    let mut failures = Vec::new();
    let cases = corpus["cases"].as_array().unwrap();
    for case in cases {
        let query = case["query"].as_str().unwrap();
        let top_k = case["top_k"].as_i64().unwrap();
        let path = case["path"].as_str().unwrap();
        let label = format!("{name}: {query:?} top_k={top_k} path={path:?}");

        let scoped = !path.trim().trim_matches('/').is_empty();
        let fetch = retriever::fetch_depth(top_k.max(1) as usize, scoped);
        let fts: Vec<Value> = vex_search::fts::search(&idx.conn, query, fetch)
            .into_iter()
            .map(|(id, rank)| json!([id, rank]))
            .collect();
        let want_fts = case["fts"].as_array().unwrap();
        let ids = |v: &[Value]| v.iter().map(|p| p[0].clone()).collect::<Vec<_>>();
        if ids(&fts) != ids(want_fts) {
            failures.push(format!(
                "{label}: fts ids differ\n  got  {:?}\n  want {:?}",
                ids(&fts),
                ids(want_fts)
            ));
        }
        for (g, w) in fts.iter().zip(want_fts) {
            let (g, w) = (g[1].as_f64().unwrap(), w[1].as_f64().unwrap());
            // FTS5's bm25 calls the C library's log(): glibc and macOS can
            // differ in the last bits, so allow 4 ULP (order is checked above).
            if g.to_bits().abs_diff(w.to_bits()) > 4 {
                failures.push(format!("{label}: bm25 {g} != {w}"));
                break;
            }
        }

        let (hits, diag) = retriever::search(&idx.conn, Vec::new(), query, top_k, path).unwrap();
        let got: Vec<Value> = hits.iter().map(hit_json).collect();
        if Value::Array(got.clone()) != case["results"] {
            failures.push(format!(
                "{label}: results differ\n  got  {}\n  want {}",
                Value::Array(got),
                case["results"]
            ));
        }
        let got_diag = serde_json::to_value(&diag).unwrap();
        if got_diag != case["diagnostics"] {
            failures.push(format!(
                "{label}: diagnostics differ\n  got  {got_diag}\n  want {}",
                case["diagnostics"]
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} {name} cases differ:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}

#[test]
fn qa_corpus_matches_cortex() {
    check_corpus("qa");
}

#[test]
fn crafted_corpus_matches_cortex() {
    check_corpus("crafted");
}

#[test]
fn query_escaping_matches_cortex() {
    for pair in fixture("functions.json")["escape"].as_array().unwrap() {
        let (q, want) = (pair[0].as_str().unwrap(), pair[1].as_str().unwrap());
        assert_eq!(vex_search::fts::escape_query(q), want, "escape {q:?}");
    }
}

#[test]
fn symbol_candidates_match_cortex() {
    for pair in fixture("functions.json")["symbol_candidates"]
        .as_array()
        .unwrap()
    {
        let q = pair[0].as_str().unwrap();
        let want: Vec<String> = serde_json::from_value(pair[1].clone()).unwrap();
        assert_eq!(retriever::symbol_candidates(q), want, "candidates {q:?}");
    }
}

#[test]
fn fusion_matches_cortex_including_ties() {
    for case in fixture("functions.json")["fuse"].as_array().unwrap() {
        let channels: Vec<(String, Vec<String>)> =
            serde_json::from_value(case["channels"].clone()).unwrap();
        let weights: Vec<(String, f64)> = case["weights"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.as_f64().unwrap()))
            .collect();
        let refs: Vec<(&str, &[String])> = channels
            .iter()
            .map(|(n, k)| (n.as_str(), k.as_slice()))
            .collect();
        let wrefs: Vec<(&str, f64)> = weights.iter().map(|(n, w)| (n.as_str(), *w)).collect();
        let got: Vec<Value> = vex_search::fusion::fuse(&refs, &wrefs)
            .into_iter()
            .map(|f| json!([f.key, f.score, f.relative, f.channels]))
            .collect();
        assert_eq!(
            Value::Array(got),
            case["result"],
            "fuse {}",
            case["channels"]
        );
    }
}
