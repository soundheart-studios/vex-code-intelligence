//! `vex search` against Cortex's search tool, keyword-only, byte for byte.
//!
//! The repo (tests/fixtures/repos/qa/base plus fixtures/search/overlay) is
//! committed with the oracle's fixed author and date and indexed by vex;
//! every case must produce Cortex's exact wire string: key order, float
//! formatting, escaping and the byte budgets those decide. The clock is
//! frozen at 0 as it was for the recording; index_version is random per
//! index, so Cortex's is swapped for vex's (same length) before comparing.
//! Fixtures: tests/fixtures/search/cases.json, recorded from Cortex.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use vex_app::pyjson::{self, Json};
use vex_app::search::{self, Request};
use vex_app::selector::PyIntSet;
use vex_store::db::Index;
use vex_test_support::TestHome;

fn fixture() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/search/cases.json");
    let text = std::fs::read_to_string(path).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn copy_tree(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            std::fs::create_dir_all(&dest).unwrap();
            copy_tree(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), dest).unwrap();
        }
    }
}

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .env("GIT_AUTHOR_NAME", "vex")
        .env("GIT_AUTHOR_EMAIL", "vex@example.invalid")
        .env("GIT_COMMITTER_NAME", "vex")
        .env("GIT_COMMITTER_EMAIL", "vex@example.invalid")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap()
        .status;
    assert!(status.success(), "git {args:?}");
}

struct Project {
    _home: TestHome,
    root: PathBuf,
    idx: Index,
}

fn project() -> Project {
    let home = TestHome::new();
    let root = home.vex_home().join("qa");
    std::fs::create_dir_all(&root).unwrap();
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    copy_tree(&manifest.join("../../tests/fixtures/repos/qa/base"), &root);
    copy_tree(&manifest.join("tests/fixtures/search/overlay"), &root);
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "base"]);
    let root = root.canonicalize().unwrap();
    let idx = Index::open(&home.vex_home().join("index.db")).unwrap();
    vex_app::index::run(&idx, &root, false).unwrap();
    Project {
        _home: home,
        root,
        idx,
    }
}

fn request(call: &Value) -> Request {
    let (queries, list) = match &call["query"] {
        Value::Array(qs) => (
            qs.iter().map(|q| q.as_str().unwrap().to_string()).collect(),
            true,
        ),
        q => (vec![q.as_str().unwrap().to_string()], false),
    };
    Request {
        queries,
        list,
        top_k: call.get("top_k").and_then(Value::as_i64).unwrap_or(5),
        max_bytes: call.get("max_bytes").and_then(Value::as_i64).unwrap_or(0),
        path: call
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        select: call
            .get("select")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        force_fresh: false,
    }
}

/// The first differing position, with context, for a readable failure.
fn first_difference(got: &str, want: &str) -> String {
    let at = got
        .chars()
        .zip(want.chars())
        .position(|(a, b)| a != b)
        .unwrap_or(got.chars().count().min(want.chars().count()));
    let window = |s: &str| {
        s.chars()
            .skip(at.saturating_sub(60))
            .take(160)
            .collect::<String>()
    };
    format!(
        "first difference at char {at}\n  got  …{}\n  want …{}",
        window(got),
        window(want)
    )
}

fn check(p: &Project, cases: &Value, cortex_version: &str, section: &str) -> Vec<String> {
    let vex_version: String = p
        .idx
        .conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'index_version'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let frozen = || 0.0;
    let mut failures = Vec::new();
    for case in cases.as_array().unwrap() {
        let req = request(&case["call"]);
        let out = search::search(&p.idx, &p.root, &req, &frozen).unwrap();
        let want = case["wire"].as_str().unwrap().replace(
            &format!("\"index_version\": \"{cortex_version}\""),
            &format!("\"index_version\": \"{vex_version}\""),
        );
        let got = out.answer.wire();
        if got != want {
            failures.push(format!(
                "{section} {}: {}",
                case["call"],
                first_difference(&got, &want)
            ));
        }
    }
    failures
}

#[test]
fn search_payloads_equal_cortex_wire_output() {
    let doc = fixture();
    let p = project();
    let version = doc["index_version"].as_str().unwrap();
    let mut failures = check(&p, &doc["single"], version, "single");
    failures.extend(check(&p, &doc["list"], version, "list"));
    assert!(
        failures.is_empty(),
        "{} case(s) differ:\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

#[test]
fn stale_index_answers_with_a_freshness_block() {
    let doc = fixture();
    let p = project();
    let path = p.root.join("src/itsdangerous/encoding.py");
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str("\n# edited after indexing\n");
    std::fs::write(&path, text).unwrap();
    let failures = check(
        &p,
        &doc["stale"],
        doc["index_version"].as_str().unwrap(),
        "stale",
    );
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    let out = search::search(&p.idx, &p.root, &Request::single("want_bytes"), &|| 0.0).unwrap();
    assert!(out.freshness.index_stale);
}

#[test]
fn cpython_set_iteration_order() {
    for case in fixture()["set_order"].as_array().unwrap() {
        let inserted: Vec<usize> = serde_json::from_value(case["inserted"].clone()).unwrap();
        let mut s = PyIntSet::new();
        for &v in &inserted {
            s.add(v);
        }
        let got: Vec<usize> = s.iter().collect();
        let want: Vec<usize> = serde_json::from_value(case["order"].clone()).unwrap();
        assert_eq!(got, want, "add order for {inserted:?}");
        let from_list: Vec<usize> = PyIntSet::from_list(&inserted).iter().collect();
        let want: Vec<usize> = serde_json::from_value(case["from_list"].clone()).unwrap();
        assert_eq!(from_list, want, "set(list) order for {inserted:?}");
    }
}

#[test]
fn python_repr_and_json_formats() {
    let f = &fixture()["formats"];
    for pair in f["repr"].as_array().unwrap() {
        let s = pair[0].as_str().unwrap();
        assert_eq!(pyjson::repr_str(s), pair[1].as_str().unwrap(), "repr {s:?}");
    }
    for pair in f["dumps_ascii"].as_array().unwrap() {
        let s = pair[0].as_str().unwrap();
        assert_eq!(
            pyjson::dumps(&Json::from(s), pyjson::DEFAULT),
            pair[1].as_str().unwrap()
        );
    }
    for pair in f["dumps_raw"].as_array().unwrap() {
        let s = pair[0].as_str().unwrap();
        let raw = pyjson::Style {
            ensure_ascii: false,
            compact: false,
        };
        assert_eq!(
            pyjson::dumps(&Json::from(s), raw),
            pair[1].as_str().unwrap()
        );
    }
    for row in f["floats"].as_array().unwrap() {
        let repr = row[0].as_str().unwrap();
        let x: f64 = repr.parse().unwrap();
        assert_eq!(pyjson::float_repr(x), repr, "repr({repr})");
        assert_eq!(pyjson::json_float(x), row[1].as_str().unwrap());
    }
    for row in f["rounding"].as_array().unwrap() {
        let x = row[0].as_f64().unwrap();
        assert_eq!(
            pyjson::py_round(x, 1),
            row[1].as_f64().unwrap(),
            "round({x}, 1)"
        );
        assert_eq!(
            pyjson::py_round(x, 3),
            row[2].as_f64().unwrap(),
            "round({x}, 3)"
        );
    }
}

#[test]
fn selector_rules_match_cortex_on_crafted_leads() {
    use vex_app::selector::{Lead, select_leads};
    for case in fixture()["selector"].as_array().unwrap() {
        let questions: Vec<String> = serde_json::from_value(case["questions"].clone()).unwrap();
        let leads: Vec<Lead> = case["leads"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| Lead {
                target: l["target"].as_str().unwrap().into(),
                name: l["name"].as_str().unwrap().into(),
                kind: l["kind"].as_str().unwrap().into(),
                file: l["file"].as_str().unwrap().into(),
                matched_queries: serde_json::from_value(l["matched_queries"].clone()).unwrap(),
                score: l["score"].as_f64().unwrap(),
                source_available: l["source_available"].as_bool().unwrap(),
                preview: l["_selector_preview"].as_str().unwrap().into(),
            })
            .collect();
        let use_model = case["use_model"].as_bool().unwrap();
        let (selected, block) = select_leads(&questions, &leads, use_model, 0);
        let targets: Vec<&str> = selected.iter().map(|&i| leads[i].target.as_str()).collect();
        let want: Vec<&str> = case["selected"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_str().unwrap())
            .collect();
        assert_eq!(
            targets, want,
            "selection for {questions:?} (model: {use_model})"
        );
        // Key order is pinned by the wire cases; here the values are.
        assert_eq!(
            block.to_value(),
            case["selector"],
            "selector block for {questions:?}"
        );
    }
}

#[test]
fn symbol_lookups_match_cortex() {
    let doc = fixture();
    let p = project();
    let s = &doc["symbols"];
    for row in s["at_span"].as_array().unwrap() {
        let (fp, a, b) = (
            row[0].as_str().unwrap(),
            row[1].as_i64().unwrap(),
            row[2].as_i64().unwrap(),
        );
        let got = vex_app::symbols::at_span(&p.idx.conn, fp, a, b)
            .unwrap()
            .map(|s| serde_json::json!([s.qualified_name, s.kind, s.start_line, s.end_line]))
            .unwrap_or(Value::Null);
        assert_eq!(got, row[3], "at_span({fp}, {a}, {b})");
    }
    for row in s["resolve"].as_array().unwrap() {
        let t = row[0].as_str().unwrap();
        let got: Vec<String> = vex_app::symbols::resolve(&p.idx.conn, t)
            .unwrap()
            .into_iter()
            .map(|s| s.qualified_name)
            .collect();
        assert_eq!(serde_json::json!(got), row[1], "resolve({t:?})");
    }
    for row in s["near"].as_array().unwrap() {
        let t = row[0].as_str().unwrap();
        let got = vex_app::symbols::near_symbols(&p.idx.conn, t, 3).unwrap();
        assert_eq!(serde_json::json!(got), row[1], "near({t:?})");
    }
}

#[test]
fn a_failing_freshness_probe_reads_as_fresh() {
    // Cortex's rule 2: a freshness check never breaks a query.
    let p = project();
    p.idx.conn.execute_batch("DROP TABLE meta").unwrap();
    assert!(!vex_app::freshness::probe(&p.idx, &p.root));
}
