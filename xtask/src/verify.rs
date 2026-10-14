//! The deterministic half of verify_evidence for vex (a Rust port of
//! converged's `scripts/revert_and_test.py`, verify_evidence r5).
//!
//! It removes a task's subject and re-runs its checkers. A checker that still
//! passes with the thing it judges taken away has proved nothing.
//!
//! What it keeps from r5:
//! - A ref is a SHELL COMMAND judged by its exit code, run in a scratch
//!   worktree with the subject restored to its pre-task state.
//! - "Could not run" is not "went red". A checker that never executed is
//!   reported as `unavailable` and can never count as proof.
//! - Every ref must pass on the unchanged committed checkout first, so a
//!   failure that was already there can never pass for mutation evidence.
//!
//! What it fixes, because each one would let a criterion close green having
//! proved nothing:
//! - Cargo refs name no paths. `cargo test -p C --test T` protects
//!   `crates/C/tests/T.rs`. Without this the test file is removed along with
//!   the subject, the target vanishes, and the ref "goes red" on its own
//!   absence.
//! - A territory directory that contains a protected test is reverted with
//!   that test put back from HEAD. Holding the whole directory out (as r5
//!   does) collapses a task that owns `crates/X/` into test-only mode.
//! - The task id is matched exactly on the commit subject, not as a regex:
//!   the `.` in `0.1:` also matches `011:`.
//! - A task whose first commit is the root commit is reverted against the
//!   empty tree instead of being reported as having no commits.
//! - Cargo saying it could not find the target, package or command, and a
//!   run that passed only because zero tests executed, are `unavailable`.

use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Long enough for the full workspace suite, short enough that a hung checker
/// is a reported fact rather than a stalled gate.
const REF_TIMEOUT_S: u64 = 900;

/// git's well-known empty tree: the base of a task whose first commit is the
/// repository's root commit. Nothing exists in it, so every subject path is
/// deleted, which is exactly the pre-task state.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// Output that means cargo never ran the checker it was asked for.
const CARGO_UNAVAILABLE: &[&str] = &[
    "no test target named",
    "no bin target named",
    "did not match any packages",
    "no such command",
    "could not find `Cargo.toml`",
];

/// The published workflow definition, mirroring converged's assertions. Kept
/// here, next to the runner it invokes, so the two cannot drift apart.
pub const WORKFLOW_COMMAND: &str = "cargo xtask verify-evidence";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Passed,
    Failed,
    Unavailable,
}

/// The paths a set of refs protects from removal.
pub fn named_paths(refs: &[String]) -> BTreeSet<String> {
    let mut named = BTreeSet::new();
    for r in refs {
        let tokens =
            shlex::split(r).unwrap_or_else(|| r.split_whitespace().map(String::from).collect());
        for t in tokens.iter().filter(|t| is_checker_path(t)) {
            named.insert(t.trim_start_matches("./").to_string());
        }
        named.extend(cargo_named(&tokens));
    }
    named
}

/// Whether a ref token names a checker (a test or script the ref executes)
/// rather than something the ref merely inspects. r5 protected every token
/// containing `/`, so `test -f src/x.rs`-style refs had their own subject put
/// back and could never go red.
fn is_checker_path(t: &str) -> bool {
    const SCRIPT_EXTS: &[&str] = &[".rs", ".sh", ".py", ".js", ".ts"];
    let segments: Vec<&str> = t.split('/').collect();
    let in_tests_dir = segments.len() > 1
        && segments[..segments.len() - 1]
            .iter()
            .any(|s| matches!(*s, "tests" | "test"));
    let is_script = SCRIPT_EXTS.iter().any(|e| t.ends_with(e)) && !t.starts_with('-');
    in_tests_dir
        || (is_script && (t.contains("test") || segments.len() == 1 || t.starts_with("scripts/")))
}

/// Paths a cargo invocation depends on without naming them.
fn cargo_named(tokens: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    // A ref may chain several commands (`a && b`), so scan every `cargo`.
    for (i, t) in tokens.iter().enumerate() {
        if t != "cargo" {
            continue;
        }
        let rest: Vec<&str> = tokens[i + 1..]
            .iter()
            .take_while(|t| !matches!(t.as_str(), "&&" | "||" | ";" | "|"))
            .map(String::as_str)
            .collect();
        // Only `cargo test` names a checker file. `cargo xtask X` protects
        // nothing: when a task names an xtask command, that command's source
        // is the task's subject, and holding it out left nothing to go red.
        if rest.first() == Some(&"test") {
            let pkg = flag_value(&rest, &["-p", "--package"]);
            let test = flag_value(&rest, &["--test"]);
            if let Some(p) = pkg {
                let dir = if p == "xtask" {
                    "xtask".to_string()
                } else {
                    format!("crates/{p}")
                };
                // `--test T` runs one integration test; without it every
                // integration test of the crate runs, so all of them (and
                // their fixtures) are checkers, not subject.
                match test {
                    Some(t) => out.push(format!("{dir}/tests/{t}.rs")),
                    None => out.push(format!("{dir}/tests/")),
                }
            }
        }
    }
    out
}

fn flag_value<'a>(args: &[&'a str], names: &[&str]) -> Option<&'a str> {
    for (i, a) in args.iter().enumerate() {
        for n in names {
            if a == n {
                return args.get(i + 1).copied();
            }
            if let Some(v) = a.strip_prefix(&format!("{n}=")) {
                return Some(v);
            }
        }
    }
    None
}

fn under(path: &str, prefix: &str) -> bool {
    let p = prefix.trim_end_matches('/');
    path == p || path.starts_with(&format!("{p}/"))
}

/// What gets removed, what is put back afterwards, and which mode applies.
#[derive(Debug, PartialEq, Eq)]
pub struct Subject {
    /// Territory entries reverted to their pre-task state.
    pub impl_paths: Vec<String>,
    /// Named checkers inside a reverted entry, restored from HEAD afterwards.
    pub protected: Vec<String>,
    /// `subject`, or `test_only` when the checkers are the whole territory.
    pub mode: &'static str,
}

pub fn subject(territory: &[String], refs: &[String]) -> Subject {
    let named = named_paths(refs);
    let fully_held = |p: &str| named.iter().any(|n| under(p, n));
    let impl_paths: Vec<String> = territory
        .iter()
        .filter(|p| !fully_held(p))
        .cloned()
        .collect();
    if impl_paths.is_empty() {
        // Nothing but the checkers themselves: the harness case. The test is
        // the deliverable, so it is what gets removed.
        return Subject {
            impl_paths: territory.to_vec(),
            protected: vec![],
            mode: "test_only",
        };
    }
    let protected = named
        .iter()
        .filter(|n| {
            impl_paths
                .iter()
                .any(|p| under(n, p) && n.as_str() != p.trim_end_matches('/'))
        })
        .cloned()
        .collect();
    Subject {
        impl_paths,
        protected,
        mode: "subject",
    }
}

fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            err.trim().chars().take(200).collect::<String>()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The tree to restore the subject to: the parent of the task's first
/// commit, the empty tree if that commit is the root, or None if nothing
/// landed under this id.
pub fn base_for(repo: &Path, task_id: &str) -> Result<Option<String>, String> {
    let log = git(repo, &["log", "--format=%H%x1f%s"])?;
    let prefix = format!("{task_id}:");
    let commits: Vec<&str> = log
        .lines()
        .filter_map(|l| l.split_once('\x1f'))
        .filter(|(_, subject)| subject.starts_with(&prefix))
        .map(|(sha, _)| sha)
        .collect();
    let Some(earliest) = commits.last() else {
        return Ok(None);
    };
    match git(
        repo,
        &["rev-parse", "--verify", "--quiet", &format!("{earliest}^")],
    ) {
        Ok(parent) if !parent.is_empty() => Ok(Some(parent)),
        _ => Ok(Some(EMPTY_TREE.to_string())),
    }
}

fn safe_relative(path: &str) -> Result<(), String> {
    let p = Path::new(path.trim_end_matches('/'));
    let ok = !path.is_empty()
        && p.components().all(|c| matches!(c, Component::Normal(_)))
        && p.components().next().is_some();
    if ok {
        Ok(())
    } else {
        Err(format!(
            "subject must be a non-root relative path: {path:?}"
        ))
    }
}

fn exists_in(repo: &Path, tree: &str, path: &str) -> bool {
    Command::new("git")
        .args([
            "cat-file",
            "-e",
            &format!("{tree}:{}", path.trim_end_matches('/')),
        ])
        .current_dir(repo)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Whether a subject path is, or contains at HEAD, a Cargo manifest at any
/// depth (`crates/` owns every crate's manifest beneath it).
fn contains_manifest(repo: &Path, path: &str) -> bool {
    let p = path.trim_end_matches('/');
    if p == "Cargo.toml" || p.ends_with("/Cargo.toml") {
        return true;
    }
    git(repo, &["ls-tree", "-r", "--name-only", "HEAD", "--", p])
        .map(|out| {
            out.lines()
                .any(|l| l == "Cargo.toml" || l.ends_with("/Cargo.toml"))
        })
        .unwrap_or(false)
}

/// Put one path back to `tree` exactly, deleting anything added after it.
/// `git checkout` of a directory is an overlay, so the subtree goes first.
fn restore(worktree: &Path, repo: &Path, tree: &str, path: &str) -> Result<(), String> {
    safe_relative(path)?;
    let rel = path.trim_end_matches('/');
    let target = worktree.join(rel);
    if let Ok(meta) = std::fs::symlink_metadata(&target) {
        if meta.is_dir() {
            std::fs::remove_dir_all(&target).map_err(|e| e.to_string())?;
        } else {
            std::fs::remove_file(&target).map_err(|e| e.to_string())?;
        }
    }
    if exists_in(repo, tree, rel) {
        git(worktree, &["checkout", tree, "--", rel])?;
    }
    Ok(())
}

/// Sum of tests cargo reports running, or None when no summary was printed.
fn tests_run(output: &str) -> Option<u64> {
    let mut seen = false;
    let mut total = 0;
    for line in output
        .lines()
        .filter(|l| l.trim_start().starts_with("test result:"))
    {
        seen = true;
        // `test result: ok. 3 passed; 0 failed; 1 ignored; ...`
        let words: Vec<&str> = line
            .split_whitespace()
            .map(|w| w.trim_end_matches([';', '.']))
            .collect();
        for w in words.windows(2) {
            if matches!(w[1], "passed" | "failed") {
                total += w[0].parse::<u64>().unwrap_or(0);
            }
        }
    }
    seen.then_some(total)
}

/// passed | failed | unavailable, never conflating the last two.
/// `manifest_reverted`: the revert removed a Cargo manifest. Cargo then
/// failing to find its manifest, package or target is the subject's absence
/// talking (the preflight already proved the ref runs), so it is red.
pub fn run_ref(
    r: &str,
    cwd: &Path,
    env: &[(String, String)],
    timeout: Duration,
    manifest_reverted: bool,
) -> Outcome {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(r).current_dir(cwd).stdin(Stdio::null());
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let Ok(mut child) = cmd.spawn() else {
        return Outcome::Unavailable;
    };
    // Drain the pipes on threads so a chatty checker cannot block on a full
    // pipe and be misread as a timeout.
    let mut so = child.stdout.take().unwrap();
    let mut se = child.stderr.take().unwrap();
    let ho = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = so.read_to_string(&mut s);
        s
    });
    let he = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = se.read_to_string(&mut s);
        s
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if start.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Outcome::Unavailable;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => return Outcome::Unavailable,
        }
    };
    let output = format!(
        "{}{}",
        ho.join().unwrap_or_default(),
        he.join().unwrap_or_default()
    );
    classify(status.code(), &output, manifest_reverted)
}

pub fn classify(code: Option<i32>, output: &str, manifest_reverted: bool) -> Outcome {
    match code {
        // Killed by a signal, or the shell could not execute the command at
        // all (127 not found, 126 not executable): nothing answered.
        None | Some(126) | Some(127) => return Outcome::Unavailable,
        _ => {}
    }
    if CARGO_UNAVAILABLE.iter().any(|m| output.contains(m)) {
        return if manifest_reverted {
            Outcome::Failed
        } else {
            Outcome::Unavailable
        };
    }
    if code == Some(0) {
        // A green run in which no test executed judged nothing.
        return if tests_run(output) == Some(0) {
            Outcome::Unavailable
        } else {
            Outcome::Passed
        };
    }
    Outcome::Failed
}

fn strings(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn timeout() -> Duration {
    let secs = std::env::var("VEX_VERIFY_TIMEOUT_S")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(REF_TIMEOUT_S);
    Duration::from_secs(secs)
}

/// Run verification for one payload and return the report.
pub fn verify(payload: &Value) -> Value {
    let repo = PathBuf::from(payload.get("repo").and_then(Value::as_str).unwrap_or("."));
    let repo = repo.canonicalize().unwrap_or(repo);
    let task_id = payload
        .get("taskId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let territory = strings(payload, "territory");
    let test_refs = strings(payload, "testRefs");
    let criteria: Vec<(String, Vec<String>)> = payload
        .get("criteria")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|c| {
                    (
                        c.get("text")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        strings(c, "refs"),
                    )
                })
                .collect()
        })
        .unwrap_or_default();

    let subj = subject(&territory, &test_refs);
    let mut reverted = Vec::new();
    let mut refs_run = Vec::new();
    let mut failed = Vec::new();
    let mut survived = Vec::new();
    let mut unavailable: Vec<String> = Vec::new();
    let mut error: Option<String> = None;

    let result: Result<(), String> = (|| {
        let base = base_for(&repo, &task_id)?.ok_or("no commits found for this task id")?;
        if subj.impl_paths.is_empty() {
            return Err("nothing to remove".into());
        }
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let name = format!(
            "verify-{}-{}-{stamp}",
            task_id.replace(['/', ' '], "_"),
            std::process::id()
        );
        let worktree = std::env::temp_dir().join(&name);
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--detach",
                worktree.to_str().unwrap(),
                "HEAD",
            ],
        )?;
        let run = (|| -> Result<(), String> {
            // A shared target dir keeps incremental builds across runs; cargo
            // locks it, so concurrent verifications serialise rather than race.
            let target = std::env::var("CARGO_TARGET_DIR")
                .unwrap_or_else(|_| repo.join("target/verify").to_string_lossy().into_owned());
            let env = vec![
                ("PW_RUN_ID".to_string(), name.clone()),
                ("CARGO_TARGET_DIR".to_string(), target),
            ];
            let limit = timeout();
            for r in &test_refs {
                if run_ref(r, &worktree, &env, limit, false) != Outcome::Passed {
                    unavailable.push(r.clone());
                }
            }
            if !unavailable.is_empty() {
                return Err("unchanged committed checkout did not pass".into());
            }
            for p in &subj.impl_paths {
                restore(&worktree, &repo, &base, p)?;
                reverted.push(p.clone());
            }
            for p in &subj.protected {
                let p = p.trim_end_matches('/');
                if exists_in(&repo, "HEAD", p) {
                    git(&worktree, &["checkout", "HEAD", "--", p])?;
                }
            }
            let manifest_reverted = subj.impl_paths.iter().any(|p| contains_manifest(&repo, p));
            for r in &test_refs {
                refs_run.push(r.clone());
                match run_ref(r, &worktree, &env, limit, manifest_reverted) {
                    Outcome::Unavailable => unavailable.push(r.clone()),
                    Outcome::Failed => failed.push(r.clone()),
                    Outcome::Passed => survived.push(r.clone()),
                }
            }
            Ok(())
        })();
        let _ = git(
            &repo,
            &["worktree", "remove", "--force", worktree.to_str().unwrap()],
        );
        run
    })();
    if let Err(e) = result {
        error = Some(e.chars().take(400).collect());
    }

    // A criterion is PROVED when at least one of its refs went red: that ref
    // depends on the change. A ref that could not run proves nothing.
    let mut proved = Vec::new();
    let crit_json: Vec<Value> = criteria
        .iter()
        .map(|(text, refs)| {
            let outcome = if refs.is_empty() || error.is_some() {
                "unchecked"
            } else if refs.iter().any(|r| failed.contains(r)) {
                proved.push(text.clone());
                "proved"
            } else {
                "unproved"
            };
            json!({"text": text, "refs": refs, "outcome": outcome})
        })
        .collect();
    let declared: Vec<&String> = criteria
        .iter()
        .filter(|(_, r)| !r.is_empty())
        .map(|(t, _)| t)
        .collect();

    json!({
        "taskId": task_id,
        "mode": subj.mode,
        "impl": subj.impl_paths,
        "protected": subj.protected,
        "reverted": reverted,
        "refs": refs_run,
        "failed": failed,
        "survived": survived,
        "unavailable": unavailable,
        "error": error,
        "criteria": crit_json,
        "declared": declared,
        "proved": proved,
    })
}

/// The workflow document this runner is published under.
pub fn workflow_definition() -> Value {
    let input = json!({"type": "object", "fields": {
        "repo": {"type": "string"},
        "taskId": {"type": "string"},
        "territory": {"type": "array", "of": {"type": "string"}},
        "testRefs": {"type": "array", "of": {"type": "string"}},
        "criteria": {"type": "array", "optional": true, "of": {"type": "object", "fields": {
            "text": {"type": "string"}, "refs": {"type": "array", "of": {"type": "string"}}}}}
    }});
    let strs = json!({"type": "array", "of": {"type": "string"}});
    json!({
        "name": "verify_evidence",
        "description": "Remove a task's subject and re-run its checkers as shell commands, answering per criterion. Rust port of converged verify_evidence r5 with cargo-aware ref paths, exact task-id matching, root-commit bases and cargo 'could not run' classification.",
        "input": input,
        "steps": [{
            "key": "mutate",
            "determinism": "deterministic",
            "body": {"kind": "shell", "command": WORKFLOW_COMMAND},
            "input": input,
            "output": {"type": "object", "fields": {
                "taskId": {"type": "string"}, "mode": {"type": "string"},
                "impl": strs, "protected": strs, "reverted": strs, "refs": strs,
                "failed": strs, "survived": strs, "unavailable": strs,
                "error": {"type": "union", "of": [{"type": "string"}, {"type": "null"}]},
                "criteria": {"type": "array", "of": {"type": "object", "fields": {
                    "text": {"type": "string"}, "refs": strs, "outcome": {"type": "string"}}}},
                "declared": strs, "proved": strs
            }}
        }],
        "assertions": [
            {"kind": "schema", "reason": "unreadable_mutation"},
            {"kind": "count", "min": 1, "of": "output.reverted[]", "reason": "nothing_to_revert"},
            {"kind": "count", "min": 1, "of": "output.refs[]", "reason": "no_refs_to_run"},
            {"kind": "count", "max": 0, "of": "output.unavailable[]", "reason": "checker_unavailable"},
            {"kind": "subset", "of": "output.declared[]", "within": "output.proved[]", "reason": "criterion_unproved"}
        ]
    })
}

/// End-to-end check on a throwaway repository: a watched change is proved,
/// an unwatched one survives, and the published definition invokes this
/// runner with every assertion converged's gate relies on.
pub fn self_check() -> Result<(), String> {
    let def = workflow_definition();
    let cmd = def["steps"][0]["body"]["command"].as_str().unwrap_or("");
    if cmd != WORKFLOW_COMMAND {
        return Err(format!("workflow runs {cmd:?}, not the xtask verifier"));
    }
    let reasons: BTreeSet<&str> = def["assertions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a["reason"].as_str())
        .collect();
    for need in [
        "nothing_to_revert",
        "no_refs_to_run",
        "checker_unavailable",
        "criterion_unproved",
    ] {
        if !reasons.contains(need) {
            return Err(format!("workflow lacks assertion {need}"));
        }
    }

    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("vex-selfcheck-{}-{stamp}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let result = (|| {
        let g = |args: &[&str]| {
            git(
                &dir,
                &[
                    &[
                        "-c",
                        "user.name=vex",
                        "-c",
                        "user.email=vex@localhost",
                        "-c",
                        "commit.gpgsign=false",
                    ],
                    args,
                ]
                .concat(),
            )
        };
        g(&["init", "-q"])?;
        std::fs::write(dir.join("README"), "base\n").map_err(|e| e.to_string())?;
        g(&["add", "."])?;
        g(&["commit", "-q", "-m", "base"])?;
        std::fs::write(dir.join("feature.txt"), "x\n").map_err(|e| e.to_string())?;
        g(&["add", "."])?;
        g(&["commit", "-q", "-m", "T1: add feature"])?;
        let report = verify(&json!({
            "repo": dir, "taskId": "T1", "territory": ["feature.txt"],
            "testRefs": ["test -f feature.txt", "true"],
            "criteria": [{"text": "watched", "refs": ["test -f feature.txt"]},
                         {"text": "unwatched", "refs": ["true"]}]
        }));
        if report["proved"] != json!(["watched"]) || report["survived"] != json!(["true"]) {
            return Err(format!("unexpected self-check report: {report}"));
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// `cargo xtask verify-evidence [--self-check | --print-workflow]`.
pub fn main(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("--self-check") => match self_check() {
            Ok(()) => {
                println!("verify-evidence self-check: ok");
                0
            }
            Err(e) => {
                eprintln!("verify-evidence self-check failed: {e}");
                1
            }
        },
        Some("--print-workflow") => {
            println!(
                "{}",
                serde_json::to_string_pretty(&workflow_definition()).unwrap()
            );
            0
        }
        Some("--explain-subject") | None => {
            let mut input = String::new();
            if std::io::stdin().read_to_string(&mut input).is_err() {
                eprintln!("could not read the payload from stdin");
                return 1;
            }
            let payload: Value = match serde_json::from_str(&input) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("payload is not JSON: {e}");
                    return 1;
                }
            };
            let report = if !args.is_empty() {
                let s = subject(
                    &strings(&payload, "territory"),
                    &strings(&payload, "testRefs"),
                );
                json!({"impl": s.impl_paths, "protected": s.protected, "mode": s.mode})
            } else {
                verify(&payload)
            };
            println!("{report}");
            0
        }
        Some(other) => {
            eprintln!("unknown argument {other:?}");
            1
        }
    }
}
