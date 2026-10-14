//! `vex context` and `vex impact` against Cortex's tools, byte for byte.
//!
//! The repo (tests/fixtures/repos/qa/base plus fixtures/graph/overlay) is
//! committed with the oracle's fixed author and date and indexed by vex.
//! Every case must produce Cortex's exact wire string: edge rows in the
//! order Cortex reports them, the confidence floor and the 25-edge cap with
//! their recovery pointer, the detail ladder and the structure view,
//! depth-attenuated impact confidences, and the `both` envelope with its
//! node and edge budgets. index_version is swapped for vex's.
//! Fixtures: tests/fixtures/graph/cases.json, recorded from Cortex.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use vex_app::graph::{self, ImpactRequest};
use vex_store::db::Index;
use vex_test_support::TestHome;

fn fixture() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/graph/cases.json");
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
    root: PathBuf,
    idx: Index,
}

fn project() -> Project {
    let home = TestHome::new();
    let root = home.vex_home().join("qa");
    std::fs::create_dir_all(&root).unwrap();
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    copy_tree(&manifest.join("../../tests/fixtures/repos/qa/base"), &root);
    copy_tree(&manifest.join("tests/fixtures/graph/overlay"), &root);
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

fn context_wire(p: &Project, call: &Value) -> String {
    graph::context(
        &p.idx.conn,
        &p.root,
        call["symbol"].as_str().unwrap(),
        call.get("detail").and_then(Value::as_str).unwrap_or("full"),
        call.get("confidence_min")
            .and_then(Value::as_f64)
            .unwrap_or(0.7),
    )
    .unwrap()
    .wire()
}

fn impact_wire(p: &Project, call: &Value) -> String {
    let (roots, list) = match &call["symbol"] {
        Value::Array(rs) => (
            rs.iter().map(|r| r.as_str().unwrap().to_string()).collect(),
            true,
        ),
        r => (vec![r.as_str().unwrap().to_string()], false),
    };
    let req = ImpactRequest {
        roots,
        list,
        direction: call
            .get("direction")
            .and_then(Value::as_str)
            .unwrap_or("upstream")
            .to_string(),
        depth: call.get("depth").and_then(Value::as_i64).unwrap_or(3),
        confidence_min: call
            .get("confidence_min")
            .and_then(Value::as_f64)
            .unwrap_or(0.7),
    };
    graph::impact(&p.idx.conn, &req).unwrap().wire()
}

fn check(
    p: &Project,
    doc: &Value,
    section: &str,
    wire: fn(&Project, &Value) -> String,
) -> Vec<String> {
    let cortex_version = doc["index_version"].as_str().unwrap();
    let vex_version: String = p
        .idx
        .conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'index_version'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let mut failures = Vec::new();
    for case in doc[section].as_array().unwrap() {
        let got = wire(p, &case["call"]);
        let want = case["wire"].as_str().unwrap().replace(
            &format!("\"index_version\": \"{cortex_version}\""),
            &format!("\"index_version\": \"{vex_version}\""),
        );
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

fn assert_clean(failures: Vec<String>) {
    assert!(
        failures.is_empty(),
        "{} case(s) differ:\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

#[test]
fn context_payloads_equal_cortex_wire_output() {
    let doc = fixture();
    let p = project();
    assert_clean(check(&p, &doc, "context", context_wire));
}

#[test]
fn impact_up_down_and_both_equal_cortex_wire_output() {
    let doc = fixture();
    let p = project();
    assert_clean(check(&p, &doc, "impact", impact_wire));
}

#[test]
fn abstentions_file_edges_and_the_edge_budget_match_cortex() {
    let doc = fixture();
    let p = project();
    for (qn, name, fp, a, b) in [
        ("tamper.md", "md_sym", "README.md", 1, 3),
        ("tamper.txt", "txt_sym", "LICENSE.txt", 1, 3),
        ("tamper.gone", "gone_sym", "graphpkg/gone.py", 1, 3),
        ("tamper.wide", "wide_sym", "graphpkg/control.py", 1, 400),
        ("tamper.empty", "empty_sym", "graphpkg/hub.py", 900, 901),
    ] {
        p.idx
            .conn
            .execute(
                "INSERT INTO symbols (qualified_name, name, kind, file_path, \
                 start_line, end_line) VALUES (?1, ?2, 'function', ?3, ?4, ?5)",
                rusqlite::params![qn, name, fp, a, b],
            )
            .unwrap();
    }
    let edge = "INSERT INTO edges VALUES (?1, ?2, ?3, ?4, ?5, ?6)";
    p.idx
        .conn
        .execute(
            edge,
            rusqlite::params![
                "tamper.importer",
                "file:graphpkg/hub.py",
                "IMPORTS",
                0.9,
                "graphpkg/x.py",
                "graphpkg/hub.py"
            ],
        )
        .unwrap();
    for i in 0..50 {
        p.idx
            .conn
            .execute(
                edge,
                rusqlite::params![
                    "graphpkg.hub.py.ping",
                    "graphpkg.hub.py.pong",
                    format!("T{i:02}"),
                    1.0,
                    "graphpkg/hub.py",
                    "graphpkg/hub.py"
                ],
            )
            .unwrap();
    }
    assert_clean(check(&p, &doc, "tampered", |p, call| {
        if call["tool"] == "context" {
            context_wire(p, call)
        } else {
            impact_wire(p, call)
        }
    }));
}

#[test]
fn the_fixture_covers_every_branch_it_claims() {
    let doc = fixture();
    let all: Vec<&str> = ["context", "impact", "tampered"]
        .iter()
        .flat_map(|s| doc[*s].as_array().unwrap())
        .map(|c| c["wire"].as_str().unwrap())
        .collect();
    for needle in [
        "\"reason\": \"context_edges_withheld\"",
        "\"over_edge_cap\": 5",
        "\"reason\": \"graph_confidence_floor\"",
        "\"reason\": \"graph_node_cap\"",
        "\"reason\": \"graph_edge_cap\"",
        "\"truncated\": true",
        "\"opened_by\": \"for_statement\"",
        "\"abstained\": true",
        "\"depth\": 3, \"edge\"",
        "\"error\": \"Symbol 'nosuch' not found.\"",
        "0.333333333333333",
    ] {
        assert!(
            all.iter().any(|w| w.contains(needle)),
            "no case exercises {needle}"
        );
    }
}
