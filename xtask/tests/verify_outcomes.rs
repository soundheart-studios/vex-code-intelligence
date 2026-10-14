//! End-to-end verification against throwaway git repositories: what counts
//! as proof, what survives, and what "could not run" means. Each repository
//! is namespaced by $PW_RUN_ID so concurrent runs never share a path.

use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static SEQ: AtomicUsize = AtomicUsize::new(0);

struct Repo(PathBuf);

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Repo {
    fn new() -> Repo {
        let run = std::env::var("PW_RUN_ID").unwrap_or_else(|_| "local".into());
        let dir = std::env::temp_dir().join(format!(
            "vex-verify-test-{run}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let r = Repo(dir);
        r.git(&["init", "-q"]);
        r
    }

    fn git(&self, args: &[&str]) {
        let st = Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@localhost",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(&self.0)
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?}");
    }

    fn write(&self, path: &str, body: &str) {
        let p = self.0.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    fn commit(&self, msg: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", msg]);
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

fn verify(
    repo: &Repo,
    task: &str,
    territory: &[&str],
    criteria: &[(&str, &[&str])],
    timeout_s: Option<u64>,
) -> Value {
    let refs: Vec<&str> = {
        let mut v: Vec<&str> = criteria
            .iter()
            .flat_map(|(_, r)| r.iter().copied())
            .collect();
        v.dedup();
        v
    };
    let payload = json!({
        "repo": repo.path(), "taskId": task, "territory": territory, "testRefs": refs,
        "criteria": criteria.iter().map(|(t, r)| json!({"text": t, "refs": r})).collect::<Vec<_>>(),
    });
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_xtask"));
    cmd.arg("verify-evidence")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    if let Some(t) = timeout_s {
        cmd.env("VEX_VERIFY_TIMEOUT_S", t.to_string());
    }
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    serde_json::from_slice(&out.stdout).unwrap()
}

/// A base commit plus a task commit adding `src/feature.txt`.
fn repo_with_task(task: &str) -> Repo {
    let r = Repo::new();
    r.write("README", "base\n");
    r.commit("base");
    r.write("src/feature.txt", "x\n");
    r.commit(&format!("{task}: add feature"));
    r
}

#[test]
fn a_ref_that_goes_red_without_the_subject_proves_its_criterion() {
    let r = repo_with_task("T1");
    let rep = verify(
        &r,
        "T1",
        &["src/feature.txt"],
        &[("feature exists", &["test -f src/feature.txt"])],
        None,
    );
    assert_eq!(rep["error"], Value::Null, "{rep}");
    assert_eq!(rep["reverted"], json!(["src/feature.txt"]));
    assert_eq!(rep["failed"], json!(["test -f src/feature.txt"]));
    assert_eq!(rep["proved"], json!(["feature exists"]));
}

#[test]
fn a_ref_that_survives_removal_leaves_its_criterion_unproved() {
    let r = repo_with_task("T1");
    let rep = verify(
        &r,
        "T1",
        &["src/feature.txt"],
        &[("watches nothing", &["true"])],
        None,
    );
    assert_eq!(rep["survived"], json!(["true"]));
    assert_eq!(rep["proved"], json!([]));
    assert_eq!(rep["criteria"][0]["outcome"], "unproved");
}

#[test]
fn a_command_that_cannot_execute_is_unavailable_not_failed() {
    let r = repo_with_task("T1");
    let rep = verify(
        &r,
        "T1",
        &["src/feature.txt"],
        &[("c", &["test -f src/feature.txt || exit 127"])],
        None,
    );
    assert_eq!(
        rep["unavailable"],
        json!(["test -f src/feature.txt || exit 127"])
    );
    assert_eq!(rep["failed"], json!([]));
    assert_eq!(rep["proved"], json!([]));
}

#[test]
fn a_timed_out_checker_is_unavailable() {
    let r = repo_with_task("T1");
    let rep = verify(
        &r,
        "T1",
        &["src/feature.txt"],
        &[("c", &["test -f src/feature.txt || sleep 5"])],
        Some(1),
    );
    assert_eq!(
        rep["unavailable"],
        json!(["test -f src/feature.txt || sleep 5"])
    );
    assert_eq!(rep["proved"], json!([]));
}

#[test]
fn cargo_missing_its_test_target_is_unavailable() {
    let r = repo_with_task("T1");
    let cmd = "test -f src/feature.txt || { echo 'error: no test target named `ids` in `vex-core` package' >&2; exit 101; }";
    let rep = verify(&r, "T1", &["src/feature.txt"], &[("c", &[cmd])], None);
    assert_eq!(rep["unavailable"], json!([cmd]));
    assert_eq!(rep["failed"], json!([]));
}

#[test]
fn cargo_losing_a_manifest_the_task_added_is_red_not_unavailable() {
    // The preflight proved the ref runs; once the subject (the manifest) is
    // gone, cargo not finding it is the dependency showing, not a broken checker.
    let r = Repo::new();
    r.write("README", "base\n");
    r.commit("base");
    r.write("Cargo.toml", "[workspace]\n");
    r.commit("T4: add the manifest");
    let cmd = "test -f Cargo.toml || { echo 'error: could not find `Cargo.toml` in `/x` or any parent directory' >&2; exit 101; }";
    let rep = verify(&r, "T4", &["Cargo.toml"], &[("manifest", &[cmd])], None);
    assert_eq!(rep["failed"], json!([cmd]), "{rep}");
    assert_eq!(rep["proved"], json!(["manifest"]));
}

#[test]
fn a_reverted_directory_holding_a_manifest_also_counts_as_red() {
    let r = Repo::new();
    r.write("README", "base\n");
    r.commit("base");
    r.write("crates/new/Cargo.toml", "[package]\n");
    r.commit("T5: add a crate");
    let cmd = "test -f crates/new/Cargo.toml || { echo 'error: package ID specification `new` did not match any packages' >&2; exit 101; }";
    let rep = verify(&r, "T5", &["crates/"], &[("crate", &[cmd])], None);
    assert_eq!(rep["failed"], json!([cmd]), "{rep}");
}

#[test]
fn a_green_run_where_zero_tests_executed_is_unavailable() {
    let r = repo_with_task("T1");
    // Passes on the unchanged checkout only by running nothing.
    let cmd = "echo 'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out'";
    let rep = verify(&r, "T1", &["src/feature.txt"], &[("c", &[cmd])], None);
    assert_eq!(rep["unavailable"], json!([cmd]));
    assert_eq!(
        rep["reverted"],
        json!([]),
        "nothing may be reverted after a failed preflight"
    );
}

#[test]
fn a_ref_red_on_the_unchanged_checkout_aborts_before_any_revert() {
    let r = repo_with_task("T1");
    let rep = verify(&r, "T1", &["src/feature.txt"], &[("c", &["false"])], None);
    assert_eq!(rep["reverted"], json!([]));
    assert_eq!(rep["refs"], json!([]));
    assert_eq!(rep["error"], "unchanged committed checkout did not pass");
    assert_eq!(rep["proved"], json!([]));
}

#[test]
fn the_task_id_is_matched_exactly_not_as_a_regex() {
    let r = Repo::new();
    r.write("README", "base\n");
    r.commit("base");
    r.write("other.txt", "y\n");
    r.commit("011: an unrelated task");
    // `0.1` as a regex would match `011:` and revert against the wrong base.
    let rep = verify(
        &r,
        "0.1",
        &["other.txt"],
        &[("c", &["test -f other.txt"])],
        None,
    );
    assert_eq!(rep["error"], "no commits found for this task id");
}

#[test]
fn a_task_whose_first_commit_is_the_root_reverts_to_the_empty_tree() {
    let r = Repo::new();
    r.write("Cargo.toml", "[workspace]\n");
    r.commit("0.1: root commit");
    let rep = verify(
        &r,
        "0.1",
        &["Cargo.toml"],
        &[("c", &["test -f Cargo.toml"])],
        None,
    );
    assert_eq!(rep["error"], Value::Null, "{rep}");
    assert_eq!(rep["proved"], json!(["c"]));
}

#[test]
fn files_added_under_an_existing_directory_are_removed_exactly() {
    let r = Repo::new();
    r.write("pkg/old.txt", "o\n");
    r.commit("base");
    r.write("pkg/new.txt", "n\n");
    r.commit("T2: add new");
    let rep = verify(
        &r,
        "T2",
        &["pkg/"],
        &[
            ("new exists", &["test -f pkg/new.txt"]),
            ("old kept", &["test -f pkg/old.txt"]),
        ],
        None,
    );
    assert_eq!(rep["proved"], json!(["new exists"]));
    assert_eq!(rep["survived"], json!(["test -f pkg/old.txt"]));
}

#[test]
fn a_protected_test_inside_a_reverted_directory_still_runs() {
    let r = Repo::new();
    r.write("README", "base\n");
    r.commit("base");
    r.write("pkg/impl.txt", "i\n");
    r.write("pkg/tests/check.sh", "test -f pkg/impl.txt\n");
    r.commit("T3: impl and its test");
    let rep = verify(
        &r,
        "T3",
        &["pkg/"],
        &[("impl present", &["sh pkg/tests/check.sh"])],
        None,
    );
    // The test was put back, so it ran and went red on the missing impl
    // rather than on its own absence (which sh reports as exit 127/2).
    assert_eq!(rep["protected"], json!(["pkg/tests/check.sh"]));
    assert_eq!(rep["failed"], json!(["sh pkg/tests/check.sh"]), "{rep}");
    assert_eq!(rep["proved"], json!(["impl present"]));
}
