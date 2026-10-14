//! `vex grep` against Cortex's grep tool on its ripgrep path, byte for byte.
//!
//! The repo (tests/fixtures/repos/qa/base plus fixtures/grep/overlay, with
//! `_gitignore` renamed) is committed with the oracle's fixed author and
//! date and indexed by vex. Every case must produce Cortex's exact wire
//! string -- which files rg skips, the per-file match cap, context and its
//! `--` separators counted against the line cap, Python's splitlines on the
//! printed text, the 300-character clip and its recovery chunk -- and each
//! hit's owning symbol must be the one Cortex's at_span finds. As in the
//! oracle, `excluded.txt` goes in .git/info/exclude and a symlink is added
//! after indexing.
//! Fixtures: tests/fixtures/grep/cases.json, recorded from Cortex.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use vex_app::grep::{self, Request};
use vex_app::pyjson;
use vex_store::db::Index;
use vex_test_support::TestHome;

fn fixture() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/grep/cases.json");
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
    copy_tree(&manifest.join("tests/fixtures/grep/overlay"), &root);
    std::fs::rename(root.join("_gitignore"), root.join(".gitignore")).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    let exclude = root.join(".git/info/exclude");
    let mut rules = std::fs::read_to_string(&exclude).unwrap_or_default();
    rules.push_str("excluded.txt\n");
    std::fs::write(&exclude, rules).unwrap();
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "base"]);
    let root = root.canonicalize().unwrap();
    let idx = Index::open(&home.vex_home().join("index.db")).unwrap();
    vex_app::index::run(&idx, &root, false).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("a-b/c-d.txt", root.join("docs/link.txt")).unwrap();
    Project {
        _home: home,
        root,
        idx,
    }
}

fn request(call: &Value) -> Request {
    Request {
        pattern: call["pattern"].as_str().unwrap().to_string(),
        path: call
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        literal: call
            .get("literal")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        max_results: call
            .get("max_results")
            .and_then(Value::as_i64)
            .unwrap_or(grep::DEFAULT_MAX_RESULTS),
        context_lines: call
            .get("context_lines")
            .and_then(Value::as_i64)
            .unwrap_or(0),
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

#[test]
fn grep_payloads_and_owning_symbols_equal_cortex() {
    let doc = fixture();
    let p = project();
    let mut failures = Vec::new();
    for case in doc["cases"].as_array().unwrap() {
        if cfg!(not(unix)) && case["call"]["path"] == "docs/link.txt" {
            continue;
        }
        let payload = grep::grep(&p.idx.conn, &p.root, &request(&case["call"])).unwrap();
        let got = pyjson::dumps(&payload, pyjson::DEFAULT);
        let want = case["wire"].as_str().unwrap();
        if got != want {
            failures.push(format!(
                "{}: {}",
                case["call"],
                first_difference(&got, want)
            ));
            continue;
        }
        let owners = grep::owners(&p.idx.conn, &payload).unwrap();
        if serde_json::json!(owners) != case["owners"] {
            failures.push(format!(
                "{}: owners {owners:?} != {}",
                case["call"], case["owners"]
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} case(s) differ:\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

#[test]
fn patterns_ripgrep_rejects_are_bad_patterns() {
    // Cortex retries these with Python's `re` (a different dialect, not
    // ported): `want_bytes(` fails there too, a lookbehind succeeds. vex
    // reports both as bad patterns, in Cortex's error shape.
    let doc = fixture();
    let p = project();
    for case in doc["divergent"].as_array().unwrap() {
        let payload = grep::grep(&p.idx.conn, &p.root, &request(&case["call"])).unwrap();
        let got = pyjson::dumps(&payload, pyjson::DEFAULT);
        assert!(
            got.starts_with("{\"error\": \"bad pattern: "),
            "{}: {got}",
            case["call"]
        );
    }
    let cortex = doc["divergent"][0]["wire"].as_str().unwrap();
    assert!(
        cortex.starts_with("{\"error\": \"bad pattern: "),
        "{cortex}"
    );
}

#[test]
fn hyphenated_paths_split_on_the_unit_separator() {
    // The reason Cortex fixes rg's field separators: a `-` in the path.
    let p = project();
    let payload = grep::grep(&p.idx.conn, &p.root, &Request::new("hyphen")).unwrap();
    let hits = payload.get("hits").unwrap().to_value();
    assert_eq!(hits[0]["file"], "docs/a-b/c-d.txt");
    assert_eq!(hits[0]["line"], "1");
    let cli = grep::cli_json(&p.idx.conn, &payload).unwrap().to_value();
    assert_eq!(cli["schema"], 1);
    assert!(cli["hits"][0]["symbol"].is_null());
    let payload = grep::grep(&p.idx.conn, &p.root, &Request::new("find_needle")).unwrap();
    let cli = grep::cli_json(&p.idx.conn, &payload).unwrap().to_value();
    assert_eq!(cli["hits"][0]["file"], "src/my-pkg/thing-one.py");
    assert_eq!(
        cli["hits"][0]["symbol"],
        "src.my-pkg.thing-one.py.find_needle"
    );
}
