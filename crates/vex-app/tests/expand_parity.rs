//! `vex expand` against Cortex's expand tool, byte for byte.
//!
//! The repo (tests/fixtures/repos/qa/base plus the search and expand
//! overlays) is committed with the oracle's fixed author and date and
//! indexed by vex; every case must produce Cortex's exact wire string. The
//! detail ladder, multi-site symbols, chunk ids, refusals and the list
//! budget (including its exact edge) are all in the single and list
//! sections; after_delete runs once two files are gone from disk.
//! index_version is random per index, so Cortex's is swapped for vex's.
//! Fixtures: tests/fixtures/expand/cases.json, recorded from Cortex.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use vex_app::expand::{self, Request};
use vex_store::db::Index;
use vex_test_support::TestHome;

fn fixture() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/expand/cases.json");
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
    copy_tree(&manifest.join("tests/fixtures/search/overlay"), &root);
    copy_tree(&manifest.join("tests/fixtures/expand/overlay"), &root);
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
    let (targets, list) = match &call["target"] {
        Value::Array(ts) => (
            ts.iter().map(|t| t.as_str().unwrap().to_string()).collect(),
            true,
        ),
        t => (vec![t.as_str().unwrap().to_string()], false),
    };
    Request {
        targets,
        list,
        detail: call
            .get("detail")
            .and_then(Value::as_str)
            .unwrap_or("full")
            .to_string(),
        max_bytes: call
            .get("max_bytes")
            .and_then(Value::as_i64)
            .unwrap_or(expand::DEFAULT_MAX_BYTES),
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
    let mut failures = Vec::new();
    for case in cases.as_array().unwrap() {
        let req = request(&case["call"]);
        let got = expand::answer(&p.idx, &p.root, &req).unwrap().wire();
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
fn single_targets_equal_cortex_wire_output() {
    let doc = fixture();
    let p = project();
    assert_clean(check(
        &p,
        &doc["single"],
        doc["index_version"].as_str().unwrap(),
        "single",
    ));
}

#[test]
fn target_lists_and_budgets_equal_cortex_wire_output() {
    let doc = fixture();
    let p = project();
    assert_clean(check(
        &p,
        &doc["list"],
        doc["index_version"].as_str().unwrap(),
        "list",
    ));
}

#[test]
fn deleted_sources_fall_back_like_cortex() {
    let doc = fixture();
    let p = project();
    std::fs::remove_file(p.root.join("src/itsdangerous/_table.py")).unwrap();
    std::fs::remove_file(p.root.join("README.md")).unwrap();
    assert_clean(check(
        &p,
        &doc["after_delete"],
        doc["index_version"].as_str().unwrap(),
        "after_delete",
    ));
}

#[cfg(unix)]
#[test]
fn escapes_symlinks_and_odd_spans_are_handled_like_cortex() {
    // Rows and files no indexer writes, as the oracle's tamper() makes them.
    let doc = fixture();
    let p = project();
    let pkg = p.root.join("src/itsdangerous");
    let outside = p.root.parent().unwrap().join("outside.py");
    std::fs::write(&outside, "def outside():\n    pass\n").unwrap();
    std::os::unix::fs::symlink("_ladder.py", pkg.join("link_in.py")).unwrap();
    std::os::unix::fs::symlink(&outside, pkg.join("link_out.py")).unwrap();
    std::fs::write(pkg.join("huge.py"), "x = 1\n".repeat(110_000)).unwrap();
    for (qn, name, fp, a, b) in [
        ("tamper.escape", "escape_sym", "../outside.py", 1, 1),
        (
            "tamper.link_in",
            "link_in_sym",
            "src/itsdangerous/link_in.py",
            4,
            6,
        ),
        (
            "tamper.link_out",
            "link_out_sym",
            "src/itsdangerous/link_out.py",
            1,
            1,
        ),
        ("tamper.huge", "huge_sym", "src/itsdangerous/huge.py", 1, 1),
        ("tamper.missing", "missing_sym", "src/nope.py", 1, 2),
        (
            "tamper.zero",
            "zero_sym",
            "src/itsdangerous/_ladder.py",
            0,
            2,
        ),
        (
            "tamper.neg",
            "neg_sym",
            "src/itsdangerous/_ladder.py",
            -1,
            3,
        ),
        (
            "tamper.past",
            "past_sym",
            "src/itsdangerous/_ladder.py",
            66,
            999,
        ),
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
    // A declaration site sorting first by line; a capped non-ASCII chunk
    // whose file is gone.
    p.idx
        .conn
        .execute_batch(
            "INSERT INTO symbol_sites (qualified_name, name, kind, file_path, start_line, \
             end_line, declaration) VALUES ('src.itsdangerous._ladder.py.Box.value', 'value', \
             'function', 'src/itsdangerous/_ladder.py', 56, 57, 1); \
             UPDATE chunks SET full_chars = full_chars + 50 WHERE id = 'bca9af36cde00a595d57d186';",
        )
        .unwrap();
    std::fs::remove_file(pkg.join("_i18n.py")).unwrap();
    assert_clean(check(
        &p,
        &doc["tampered"],
        doc["index_version"].as_str().unwrap(),
        "tampered",
    ));
}

#[test]
fn detail_ladder_matches_cortex_on_crafted_lines() {
    // The C-family rungs (`{`, `/**`, `///`, `//!`), whitespace stripping
    // and the 12-line signature window, independent of any grammar.
    for case in fixture()["apply_detail"].as_array().unwrap() {
        let lines: Vec<String> = serde_json::from_value(case["lines"].clone()).unwrap();
        let detail = case["detail"].as_str().unwrap();
        let (source, exact) = expand::apply_detail(&lines, detail);
        assert_eq!(
            serde_json::json!([source, exact]),
            case["result"],
            "apply_detail({lines:?}, {detail})"
        );
    }
}

#[test]
fn the_fixture_covers_every_branch_it_claims() {
    // Guard the oracle: if a fixture edit stopped exercising these, the
    // parity tests above would pass vacuously.
    let doc = fixture();
    let all: Vec<&str> = ["single", "list", "after_delete"]
        .iter()
        .flat_map(|s| doc[*s].as_array().unwrap())
        .map(|c| c["wire"].as_str().unwrap())
        .collect();
    for needle in [
        "\"sites\": [",
        "\"chunk\": ",
        "\"reason\": \"chunk_body_cap\"",
        "\"reason\": \"response_budget\"",
        "detail='minimal'",
        "detail='signature'",
        "CORTEX ERROR (not_found)",
        "CORTEX ERROR (invalid_request)",
        "\\u00df",
    ] {
        assert!(
            all.iter().any(|w| w.contains(needle)),
            "no case exercises {needle}"
        );
    }
}
