//! `vex changes` against Cortex's detect_changes tool, byte for byte.
//!
//! The repo is built exactly as the oracle builds it (two commits, staged
//! changes, unstaged changes on top; tests/fixtures/changes/steps.json) and
//! indexed by vex. Working-tree and --base cases run against that index;
//! the staged cases must equal Cortex's detect_changes on a checkout holding
//! only the staged changes. Cortex's answer depends on its interpreter's
//! hash seed where a caller is reachable from several changed symbols, so
//! each case lists every distinct answer Cortex gave across seeds and vex
//! must produce one of them.
//! Fixtures: tests/fixtures/changes/cases.json, recorded from Cortex.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use vex_app::changes;
use vex_store::db::Index;
use vex_test_support::TestHome;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/changes")
}

fn fixture() -> Value {
    let text = std::fs::read_to_string(fixtures().join("cases.json")).unwrap();
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
            // Contents only: a clone keeps the fixture's mtime, and a
            // same-size overwrite with an old mtime looks unchanged to
            // git's stat cache, so `git add -A` would skip it.
            std::fs::write(dest, std::fs::read(entry.path()).unwrap()).unwrap();
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

fn strings(v: &Value) -> Vec<&str> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap())
        .collect()
}

/// The oracle's build(): c1 committed, c2 committed, staged/ added, then
/// worktree/ on top with an unstaged deletion, an append and an untracked
/// file.
fn project() -> Project {
    let home = TestHome::new();
    let root = home.vex_home().join("qa");
    std::fs::create_dir_all(&root).unwrap();
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let f = fixtures();
    let steps: Value =
        serde_json::from_str(&std::fs::read_to_string(f.join("steps.json")).unwrap()).unwrap();
    copy_tree(&manifest.join("../../tests/fixtures/repos/qa/base"), &root);
    copy_tree(&manifest.join("tests/fixtures/graph/overlay"), &root);
    copy_tree(&f.join("c1"), &root);
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "c1"]);
    copy_tree(&f.join("c2"), &root);
    for rel in strings(&steps["c2_delete"]) {
        std::fs::remove_file(root.join(rel)).unwrap();
    }
    for (rel, text) in steps["c2_create"].as_object().unwrap() {
        std::fs::write(root.join(rel), text.as_str().unwrap()).unwrap();
    }
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "c2"]);
    copy_tree(&f.join("staged"), &root);
    for rel in strings(&steps["staged_delete"]) {
        std::fs::remove_file(root.join(rel)).unwrap();
    }
    git(&root, &["add", "-A"]);
    copy_tree(&f.join("worktree"), &root);
    for rel in strings(&steps["worktree_delete"]) {
        std::fs::remove_file(root.join(rel)).unwrap();
    }
    for (rel, text) in steps["worktree_append"].as_object().unwrap() {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(root.join(rel))
            .unwrap();
        file.write_all(text.as_str().unwrap().as_bytes()).unwrap();
    }
    for (rel, text) in steps["untracked"].as_object().unwrap() {
        std::fs::write(root.join(rel), text.as_str().unwrap()).unwrap();
    }
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

fn check(cases: &Value, section: &str, answer: impl Fn(&str, i64) -> String) -> Vec<String> {
    let mut failures = Vec::new();
    for case in cases.as_array().unwrap() {
        let call = &case["call"];
        let base = call["base"].as_str().unwrap();
        let depth = call
            .get("max_depth")
            .and_then(Value::as_i64)
            .unwrap_or(changes::DEFAULT_MAX_DEPTH);
        let got = answer(base, depth);
        let wires = strings(&case["wires"]);
        if !wires.contains(&got.as_str()) {
            failures.push(format!(
                "{section} {call}: {}",
                first_difference(&got, wires[0])
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
fn working_tree_and_base_changes_equal_cortex() {
    let p = project();
    assert_clean(check(&fixture()["cases"], "cases", |base, depth| {
        let result = changes::detect(&p.idx.conn, &p.root, base, depth).unwrap();
        vex_app::pyjson::dumps(&result, vex_app::pyjson::DEFAULT)
    }));
}

#[test]
fn hunk_headers_parse_like_cortex_regex() {
    // Git prints only well-formed headers; Cortex's regex decides the rest.
    for case in fixture()["hunks"].as_array().unwrap() {
        let line = case[0].as_str().unwrap();
        let want = case[1]
            .as_array()
            .map(|p| (p[0].as_i64().unwrap(), p[1].as_i64().unwrap()));
        assert_eq!(changes::hunk(line), want, "hunk header {line:?}");
    }
}

#[test]
fn tampered_edges_merge_and_rank_like_cortex() {
    // The oracle's tamper steps: shared SQL, then each step's cases.
    let p = project();
    let doc = fixture();
    let steps: Value =
        serde_json::from_str(&std::fs::read_to_string(fixtures().join("steps.json")).unwrap())
            .unwrap();
    let mut failures = Vec::new();
    for (i, step) in steps["tamper"].as_array().unwrap().iter().enumerate() {
        p.idx
            .conn
            .execute_batch(step["sql"].as_str().unwrap())
            .unwrap();
        let section = format!("tamper[{i}]");
        failures.extend(check(&doc["tamper"][i], &section, |base, depth| {
            let result = changes::detect(&p.idx.conn, &p.root, base, depth).unwrap();
            vex_app::pyjson::dumps(&result, vex_app::pyjson::DEFAULT)
        }));
    }
    assert_clean(failures);
}

#[test]
fn staged_changes_equal_cortex_on_a_staged_only_checkout() {
    let p = project();
    let before = std::fs::read(p.root.join(".git/index")).unwrap();
    assert_clean(check(&fixture()["staged"], "staged", |_, depth| {
        let result = changes::staged(&p.root, depth).unwrap();
        vex_app::pyjson::dumps(&result, vex_app::pyjson::DEFAULT)
    }));
    // Where Cortex's answer depends on its hash seed, vex's is fixed: hit
    // symbols are walked in index row order, so `caller` (reachable from
    // editbody and editsig at equal confidence) is reported via editbody,
    // the earlier row.
    for depth in [1, 2, 3] {
        let result = changes::staged(&p.root, depth).unwrap();
        let caller = match result.get("affected") {
            Some(vex_app::pyjson::Json::Arr(a)) => a
                .iter()
                .find(|a| a.get("symbol").and_then(|s| s.as_str()) == Some("shapes.mod.py.caller"))
                .and_then(|a| a.get("via").and_then(|v| v.as_str()).map(str::to_string)),
            _ => None,
        };
        assert_eq!(
            caller.as_deref(),
            Some("shapes.mod.py.editbody"),
            "depth {depth}"
        );
    }
    // Neither git's index nor vex's was touched.
    assert_eq!(std::fs::read(p.root.join(".git/index")).unwrap(), before);
    let files: i64 = p
        .idx
        .conn
        .query_row(
            "SELECT COUNT(*) FROM files WHERE path = 'shapes/untracked.py'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(files, 1, "the project index still holds the working tree");
}

#[test]
fn the_fixture_covers_every_branch_it_claims() {
    // Guard the oracle: if a fixture edit stopped exercising these, the
    // parity tests above would pass vacuously.
    let doc = fixture();
    let tamper = doc["tamper"].as_array().unwrap().iter();
    let all: Vec<&str> = ["cases", "staged"]
        .iter()
        .flat_map(|s| doc[*s].as_array().unwrap())
        .chain(tamper.flat_map(|step| step.as_array().unwrap()))
        .flat_map(|c| strings(&c["wires"]))
        .collect();
    for needle in [
        "\"risk_level\": \"low\"",
        "\"risk_level\": \"medium\"",
        "\"risk_level\": \"high\"",
        "\"shape\": \"added\"",
        "\"shape\": \"removed\"",
        "\"shape\": \"moved\"",
        "\"shape\": \"body-edited\"",
        "\"shape\": \"signature-edited\"",
        "\"origin\": \"introduced\"",
        "\"origin\": \"pre-existing\"",
        "\"type\": \"throw_statement\"",
        "\"type\": \"switch_statement\"",
        "\"shape_abstention\": \"base \\\"it's\\\"",
        "no uncommitted changes detected",
        "\"depth\": 2",
        "\"confidence\": 0.69",
        "\"symbol\": \"shapes.dels.py.trimmed\"",
        "\"name\": \"cr_fn\"",
        "\"name\": \"reflow\", \"file\": \"shapes/sig.py\", \"shape\": \"signature-edited\"",
        "\"name\": \"morph\", \"file\": \"shapes/sig.py\", \"shape\": \"signature-edited\", \"from_line\": 9, \"to_line\": 8, \"kind\": \"class\"",
    ] {
        assert!(
            all.iter().any(|w| w.contains(needle)),
            "no case exercises {needle}"
        );
    }
    let truncated = all.iter().any(|w| {
        serde_json::from_str::<Value>(w).unwrap()["summary"]["affected_symbols"]
            .as_i64()
            .is_some_and(|n| n > 30)
    });
    assert!(truncated, "no case truncates the affected list");
    assert!(
        doc["staged"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["wires"].as_array().unwrap().len() > 1),
        "no case shows Cortex's hash-seed dependent order"
    );
}
