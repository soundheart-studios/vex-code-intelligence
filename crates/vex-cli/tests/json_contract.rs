//! The CLI's `--json` contract, as insta snapshots of every query command
//! run through the binary on a fixture repo.
//!
//! The repo is tests/fixtures/repos/qa/base plus the graph overlay,
//! committed with a fixed author and date and indexed by `vex init`. Each
//! snapshot is the exit code and the exact stdout: one line of compact JSON
//! whose first key is `"schema": 1`, with no ANSI codes, on success, on a
//! refusal, on a stale index and when the project has no index. These
//! snapshots are the contract the MCP server and the daemon are compared
//! against. Values that differ per run (index_version, index_age_s, timings,
//! response_bytes, which counts the timing digits, the temporary root) are
//! redacted; search_parity pins response_bytes under a frozen clock.

use std::path::{Path, PathBuf};
use std::process::Command;
use vex_test_support::TestHome;

struct Env {
    home: TestHome,
    root: PathBuf,
}

fn copy_tree(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            std::fs::create_dir_all(&dest).unwrap();
            copy_tree(&entry.path(), &dest);
        } else {
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

impl Env {
    fn new() -> Env {
        let home = TestHome::new();
        let root = home.vex_home().join("qa");
        std::fs::create_dir_all(&root).unwrap();
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        copy_tree(&manifest.join("../../tests/fixtures/repos/qa/base"), &root);
        copy_tree(
            &manifest.join("../vex-app/tests/fixtures/graph/overlay"),
            &root,
        );
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "base"]);
        let root = root.canonicalize().unwrap();
        let env = Env { home, root };
        let (code, out) = env.run(&env.root, &["init"]);
        assert_eq!(code, 0, "vex init: {out}");
        env
    }

    /// `vex <args>` in `cwd`: the exit code and stdout.
    fn run(&self, cwd: &Path, args: &[&str]) -> (i32, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_vex"))
            .args(args)
            .current_dir(cwd)
            .env("VEX_EMBED_BACKEND", "none")
            .env("VEX_DAEMON_AUTOSTART", "never")
            .env("VEX_HOME", self.home.vex_home().join("vexhome"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8(out.stdout).expect("stdout is UTF-8"),
        )
    }

    /// `vex --json <args>` checked against the contract and redacted for
    /// its snapshot.
    fn json(&self, args: &[&str]) -> String {
        let mut full = vec!["--json"];
        full.extend_from_slice(args);
        let (code, out) = self.run(&self.root, &full);
        contract(&out, args);
        format!("exit: {code}\n{}", redact(&out, &self.root))
    }
}

/// One line of JSON, schema 1 first, no ANSI escapes.
fn contract(out: &str, args: &[&str]) {
    assert!(
        !out.contains('\u{1b}'),
        "ANSI escape in --json for {args:?}"
    );
    assert_eq!(
        out.lines().count(),
        1,
        "--json is one line for {args:?}: {out}"
    );
    assert!(
        out.starts_with("{\"schema\":1,"),
        "--json starts with schema 1 for {args:?}: {out}"
    );
    serde_json::from_str::<serde_json::Value>(out)
        .unwrap_or_else(|e| panic!("--json parses for {args:?}: {e}: {out}"));
}

/// Replace the value after `"key":` up to the next `,` or `}` (outside a
/// string) with `<key>`.
fn redact_value(text: &str, key: &str) -> String {
    let needle = format!("\"{key}\":");
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(&needle) {
        out.push_str(&rest[..at + needle.len()]);
        rest = &rest[at + needle.len()..];
        let end = if let Some(stripped) = rest.strip_prefix('"') {
            stripped.find('"').map(|i| i + 2).unwrap_or(rest.len())
        } else {
            rest.find([',', '}']).unwrap_or(rest.len())
        };
        out.push_str(&format!("\"<{key}>\""));
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

fn redact(out: &str, root: &Path) -> String {
    let text = out
        .trim_end()
        .replace(&root.display().to_string(), "<ROOT>");
    [
        "index_version",
        "index_age_s",
        "took_ms",
        "selector_ms",
        "response_bytes",
    ]
    .iter()
    .fold(text, |text, key| redact_value(&text, key))
}

macro_rules! snap {
    ($env:expr, $name:literal, [$($arg:expr),* $(,)?]) => {
        insta::assert_snapshot!($name, $env.json(&[$($arg),*]));
    };
}

#[test]
fn query_commands_keep_their_json_contract() {
    let e = Env::new();
    snap!(e, "search_single", ["search", "signature"]);
    snap!(
        e,
        "search_list",
        ["search", "--list", "timestamp signer", "base64 decode"]
    );
    snap!(
        e,
        "search_top_k_clamped",
        ["search", "--top-k", "0", "signature"]
    );
    snap!(
        e,
        "search_refused",
        ["search", "a1", "b2", "c3", "d4", "e5", "f6", "g7"]
    );
    snap!(e, "expand_single", ["expand", "want_bytes"]);
    snap!(
        e,
        "expand_list",
        ["expand", "--detail", "signature", "hub", "leaf"]
    );
    snap!(e, "expand_not_found", ["expand", "nosuch"]);
    snap!(e, "grep_hits", ["grep", "def hub", "-C", "1"]);
    snap!(e, "grep_bad_pattern", ["grep", "("]);
    snap!(e, "context_full", ["context", "mid"]);
    snap!(
        e,
        "context_structure",
        ["context", "control", "--detail", "structure"]
    );
    snap!(e, "context_not_found", ["context", "nosuch"]);
    snap!(e, "impact_up", ["impact", "leaf"]);
    snap!(
        e,
        "impact_both",
        ["impact", "hub", "--direction", "both", "--depth", "1"]
    );
    snap!(e, "impact_list", ["impact", "hub", "nosuch"]);
    snap!(e, "map_default", ["map"]);
    snap!(
        e,
        "map_focus",
        ["map", "--focus", "graphpkg/", "--budget-tokens", "200"]
    );
    snap!(e, "changes_none", ["changes"]);
    snap!(e, "changes_bad_base", ["changes", "--base", "nosuch"]);

    // An edit, reindexed: working-tree, base and (once staged) staged reports.
    let hub = e.root.join("graphpkg/hub.py");
    let text = std::fs::read_to_string(&hub).unwrap();
    std::fs::write(&hub, text.replace("    return 1\n", "    return 2\n")).unwrap();
    assert_eq!(e.run(&e.root, &["index"]).0, 0);
    snap!(e, "changes_worktree", ["changes"]);
    snap!(e, "changes_base", ["changes", "--base", "HEAD"]);
    git(&e.root, &["add", "-A"]);
    snap!(
        e,
        "changes_staged",
        ["changes", "--staged", "--max-depth", "1"]
    );

    // A stale index still answers, with a freshness block.
    std::fs::write(&hub, text.replace("    return 1\n", "    return 3\n")).unwrap();
    snap!(e, "search_stale", ["search", "hub"]);
    snap!(e, "map_stale", ["map", "--budget-tokens", "120"]);
    snap!(e, "changes_stale", ["changes"]);
}

#[test]
fn a_project_without_an_index_still_answers_in_json() {
    let e = Env::new();
    let bare = e.home.vex_home().join("bare");
    std::fs::create_dir_all(&bare).unwrap();
    git(&bare, &["init", "-q", "-b", "main"]);
    let bare = bare.canonicalize().unwrap();
    for args in QUERIES {
        let name = format!("no_index_{}", args[0]);
        let mut full = vec!["--json"];
        full.extend_from_slice(args);
        let (code, out) = e.run(&bare, &full);
        contract(&out, args);
        insta::assert_snapshot!(name, format!("exit: {code}\n{}", redact(&out, &bare)));
    }
}

const QUERIES: [&[&str]; 7] = [
    &["search", "x"],
    &["expand", "x"],
    &["grep", "x"],
    &["context", "x"],
    &["impact", "x"],
    &["map"],
    &["changes"],
];

#[test]
fn a_missing_home_or_root_still_answers_in_json() {
    let e = Env::new();
    for args in QUERIES {
        // Neither VEX_HOME nor HOME: runtime error 4.
        let out = Command::new(env!("CARGO_BIN_EXE_vex"))
            .arg("--json")
            .args(args)
            .current_dir(&e.root)
            .env_remove("VEX_HOME")
            .env_remove("HOME")
            .output()
            .unwrap();
        let stdout = String::from_utf8(out.stdout).unwrap();
        contract(&stdout, args);
        insta::assert_snapshot!(
            "no_home",
            format!(
                "exit: {}\n{}",
                out.status.code().unwrap_or(-1),
                stdout.trim_end()
            )
        );

        // A --root that does not exist: usage error 1. The OS wording
        // varies by platform, so only its prefix is pinned.
        let mut full = vec!["--json"];
        full.extend_from_slice(args);
        full.extend(["--root", "missing-dir"]);
        let (code, stdout) = e.run(&e.root, &full);
        contract(&stdout, args);
        let doc: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(code, 1, "{args:?}");
        assert_eq!(doc["error"], "bad_path", "{args:?}");
        assert!(
            doc["message"]
                .as_str()
                .unwrap()
                .starts_with("missing-dir: "),
            "{args:?}: {stdout}"
        );
    }
}

#[test]
fn human_output_has_no_ansi_codes() {
    let e = Env::new();
    for args in [
        &["search", "signature"][..],
        &["expand", "hub"],
        &["grep", "def hub"],
        &["context", "mid"],
        &["impact", "leaf"],
        &["map"],
        &["changes"],
    ] {
        let (_, out) = e.run(&e.root, args);
        assert!(!out.contains('\u{1b}'), "ANSI escape in {args:?}");
        assert!(!out.is_empty(), "no output for {args:?}");
    }
}
