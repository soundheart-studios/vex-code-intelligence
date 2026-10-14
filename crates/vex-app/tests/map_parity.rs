//! `vex map` against Cortex's map tool, byte for byte.
//!
//! The repo (tests/fixtures/repos/qa/base plus the graph and map overlays)
//! is committed with the oracle's fixed author and date and indexed by vex.
//! Every case must produce Cortex's exact map text and included files:
//! PageRank order (ties included), the twelve-file cap, focus
//! personalization, the in-file symbol order and every exact packing edge
//! of the token budget. Tampered rows and an emptied files table follow.
//! Fixtures: tests/fixtures/map/cases.json, recorded from Cortex.

use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use vex_search::repomap::pagerank;
use vex_store::db::Index;
use vex_test_support::TestHome;

fn fixture() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/map/cases.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
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
    _root: PathBuf,
    idx: Index,
}

fn project() -> Project {
    let home = TestHome::new();
    let root = home.vex_home().join("qa");
    std::fs::create_dir_all(&root).unwrap();
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    copy_tree(&manifest.join("../../tests/fixtures/repos/qa/base"), &root);
    copy_tree(&manifest.join("tests/fixtures/graph/overlay"), &root);
    copy_tree(&manifest.join("tests/fixtures/map/overlay"), &root);
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "base"]);
    let root = root.canonicalize().unwrap();
    let idx = Index::open(&home.vex_home().join("index.db")).unwrap();
    vex_app::index::run(&idx, &root, false).unwrap();
    Project {
        _home: home,
        _root: root,
        idx,
    }
}

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

fn check(p: &Project, cases: &Value, section: &str) -> Vec<String> {
    let mut failures = Vec::new();
    for case in cases.as_array().unwrap() {
        let call = &case["call"];
        let focus: Vec<String> = match &call["focus"] {
            Value::Null => Vec::new(),
            f => serde_json::from_value(f.clone()).unwrap(),
        };
        let budget = call
            .get("budget_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(vex_app::map::DEFAULT_MAP_TOKEN_BUDGET);
        let (text, included) = vex_app::map::build(&p.idx, budget, &focus).unwrap();
        let want = case["wire"].as_str().unwrap();
        if text != want {
            failures.push(format!(
                "{section} {call}: {}",
                first_difference(&text, want)
            ));
        }
        let want_files: Vec<String> = serde_json::from_value(case["included"].clone()).unwrap();
        if included != want_files {
            failures.push(format!(
                "{section} {call}: included {included:?}, want {want_files:?}"
            ));
        }
    }
    failures
}

fn assert_clean(failures: Vec<String>) {
    assert!(
        failures.is_empty(),
        "{} case(s) differ:\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

#[test]
fn maps_with_and_without_focus_equal_cortex() {
    let p = project();
    assert_clean(check(&p, &fixture()["cases"], "cases"));
}

#[test]
fn tampered_rows_and_an_empty_index_map_like_cortex() {
    // Rows no indexer writes, inserted exactly as the oracle inserts them.
    let doc = fixture();
    let p = project();
    for (qn, name, kind, fp, line) in [
        ("tamper.k", "\u{df}\u{540d}", "x", "README.md", 4),
        ("tamper.k2", "zz", "", "README.md", 9),
        ("tamper.ghost", "ghost", "function", "not/indexed.py", 1),
        ("tamper.d1", "dup", "function", "README.md", 2),
        ("tamper.d2", "dup", "class", "README.md", 20),
    ] {
        p.idx
            .conn
            .execute(
                "INSERT INTO symbols (qualified_name, name, kind, file_path, \
                 start_line, end_line) VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                rusqlite::params![qn, name, kind, fp, line],
            )
            .unwrap();
    }
    for (src, dst, ty, conf, sf, df) in [
        (
            "tamper.self",
            "tamper.self2",
            "REFS",
            5.0,
            "README.md",
            "README.md",
        ),
        (
            "tamper.neg",
            "tamper.neg2",
            "REFS",
            -2.0,
            "LICENSE.txt",
            "README.md",
        ),
        (
            "tamper.zero",
            "tamper.zero2",
            "CALLS",
            0.0,
            "pyproject.toml",
            "README.md",
        ),
        (
            "tamper.out",
            "tamper.out2",
            "IMPORTS",
            1.0,
            "README.md",
            "not/indexed.py",
        ),
        (
            "tamper.in",
            "tamper.in2",
            "IMPORTS",
            1.0,
            "not/indexed.py",
            "README.md",
        ),
        (
            "tamper.blank",
            "tamper.blank2",
            "REFS",
            1.0,
            "README.md",
            "",
        ),
        (
            "tamper.typed",
            "tamper.typed2",
            "EXTENDS",
            9.0,
            "LICENSE.txt",
            "pyproject.toml",
        ),
        (
            "tamper.a",
            "tamper.k",
            "REFS",
            1.0,
            "LICENSE.txt",
            "README.md",
        ),
        (
            "tamper.b",
            "tamper.k",
            "REFS",
            1.0,
            "pyproject.toml",
            "README.md",
        ),
        (
            "tamper.c",
            "tamper.k2",
            "CALLS",
            1.0,
            "pyproject.toml",
            "README.md",
        ),
        (
            "tamper.imp",
            "tamper.imp2",
            "IMPORTS",
            1000.0,
            "src/itsdangerous/exc.py",
            "pyproject.toml",
        ),
    ] {
        p.idx
            .conn
            .execute(
                "INSERT INTO edges VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![src, dst, ty, conf, sf, df],
            )
            .unwrap();
    }
    p.idx
        .conn
        .execute(
            "INSERT INTO files (path, hash, language, indexed_at) VALUES ('Z', 'h', '', 0)",
            [],
        )
        .unwrap();
    let mut failures = check(&p, &doc["tampered"], "tampered");
    p.idx.conn.execute("DELETE FROM files", []).unwrap();
    failures.extend(check(&p, &doc["emptied"], "emptied"));
    assert_clean(failures);
}

#[test]
fn pagerank_equals_cortex_bit_for_bit() {
    // The map shows ranks only through their order; Cortex's own float
    // results pin the iteration count and evaluation order exactly.
    for case in fixture()["pagerank"].as_array().unwrap() {
        let nodes: Vec<String> = serde_json::from_value(case["nodes"].clone()).unwrap();
        let edges: Vec<(String, String, f64)> =
            serde_json::from_value(case["edges"].clone()).unwrap();
        let personalization: Option<HashMap<String, f64>> =
            serde_json::from_value(case["personalization"].clone()).unwrap();
        let got = pagerank(&nodes, &edges, personalization.as_ref());
        let want: HashMap<String, String> = serde_json::from_value(case["ranks"].clone()).unwrap();
        assert_eq!(got.len(), want.len(), "pagerank over {nodes:?}");
        for (node, repr) in &want {
            let want = repr.parse::<f64>().unwrap();
            assert_eq!(
                got[node].to_bits(),
                want.to_bits(),
                "rank of {node} over {} nodes: got {:?}, want {repr}",
                nodes.len(),
                got[node]
            );
        }
    }
}

#[test]
fn the_fixture_covers_every_branch_it_claims() {
    // Guard the oracle: if a fixture edit stopped exercising these, the
    // parity tests above would pass vacuously.
    let doc = fixture();
    let cases = doc["cases"].as_array().unwrap();
    let included = |c: &Value| c["included"].as_array().unwrap().len();
    assert!(
        cases.iter().any(|c| included(c) == 12),
        "no case hits the file cap"
    );
    assert!(
        (1..12).all(|n| cases.iter().any(|c| included(c) == n)),
        "some packing edge is missing"
    );
    let unfocused = cases.iter().find(|c| c["call"]["focus"].is_null()).unwrap();
    assert!(
        cases.iter().any(|c| !c["call"]["focus"].is_null()
            && included(c) == 12
            && c["included"] != unfocused["included"]),
        "no focus changes the ranking"
    );
    for needle in ["  <-", "\u{fc}", "\n  fu ", "\n  cl "] {
        assert!(
            cases
                .iter()
                .any(|c| c["wire"].as_str().unwrap().contains(needle)),
            "no case exercises {needle:?}"
        );
    }
    assert!(
        doc["emptied"][0]["wire"]
            .as_str()
            .unwrap()
            .starts_with("(index empty")
    );
}
