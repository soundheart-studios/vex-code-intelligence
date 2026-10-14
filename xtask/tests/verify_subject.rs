//! Which paths verification removes, and which it protects. A checker
//! removed along with its subject "goes red" on its own absence and proves
//! nothing, so every rule here guards against a false proof.

use serde_json::{Value, json};
use std::io::Write;
use std::process::{Command, Stdio};

fn explain(territory: &[&str], refs: &[&str]) -> Value {
    let mut child = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["verify-evidence", "--explain-subject"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let payload = json!({"territory": territory, "testRefs": refs});
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

#[test]
fn a_cargo_test_ref_protects_its_test_file() {
    let s = explain(
        &["crates/vex-core/src/ids.rs", "crates/vex-core/tests/ids.rs"],
        &["cargo test -p vex-core --test ids"],
    );
    assert_eq!(s["impl"], json!(["crates/vex-core/src/ids.rs"]));
    assert_eq!(s["mode"], "subject");
}

#[test]
fn the_package_flag_may_use_its_long_or_equals_form() {
    let s = explain(
        &["crates/vex-store/src/a.rs", "crates/vex-store/tests/t.rs"],
        &["cargo test --package=vex-store --test=t"],
    );
    assert_eq!(s["impl"], json!(["crates/vex-store/src/a.rs"]));
}

#[test]
fn xtask_tests_live_under_xtask_not_crates() {
    let s = explain(
        &["xtask/src/verify.rs", "xtask/tests/verify_subject.rs"],
        &["cargo test -p xtask --test verify_subject"],
    );
    assert_eq!(s["impl"], json!(["xtask/src/verify.rs"]));
}

#[test]
fn a_test_inside_a_reverted_directory_is_restored_not_removed() {
    // Holding the whole crate out would turn a crate-owning task into a
    // test-only one that proves itself trivially.
    let s = explain(
        &["crates/vex-core/"],
        &["cargo test -p vex-core --test ids"],
    );
    assert_eq!(s["impl"], json!(["crates/vex-core/"]));
    assert_eq!(s["protected"], json!(["crates/vex-core/tests/ids.rs"]));
    assert_eq!(s["mode"], "subject");
}

#[test]
fn an_xtask_subcommand_is_the_subject_not_a_protected_checker() {
    // A task building `xtask parity` must be able to lose parity.rs.
    let s = explain(
        &["xtask/src/parity.rs", "golden/REVISION"],
        &["cargo xtask parity --verify-only"],
    );
    assert_eq!(s["impl"], json!(["xtask/src/parity.rs", "golden/REVISION"]));
    assert_eq!(s["protected"], json!([]));
}

#[test]
fn chained_commands_are_each_parsed() {
    let s = explain(
        &[
            "crates/a/tests/x.rs",
            "crates/b/tests/y.rs",
            "crates/a/src/lib.rs",
        ],
        &["cargo test -p a --test x && cargo test -p b --test y"],
    );
    assert_eq!(s["impl"], json!(["crates/a/src/lib.rs"]));
}

#[test]
fn a_territory_of_only_checkers_is_the_test_only_case() {
    let s = explain(
        &["crates/vex-core/tests/ids.rs"],
        &["cargo test -p vex-core --test ids"],
    );
    assert_eq!(s["mode"], "test_only");
    assert_eq!(s["impl"], json!(["crates/vex-core/tests/ids.rs"]));
}

#[test]
fn a_path_the_ref_only_inspects_is_not_protected() {
    // converged r5 protected any token containing `/`, so this ref had its
    // own subject restored and could never go red.
    let s = explain(
        &["src/lib.rs"],
        &["test -f src/lib.rs", "grep -q fn src/lib.rs"],
    );
    assert_eq!(s["impl"], json!(["src/lib.rs"]));
    assert_eq!(s["mode"], "subject");
}

#[test]
fn a_script_the_ref_executes_is_protected() {
    let s = explain(
        &["scripts/link-golden.sh", "golden/REVISION"],
        &["sh scripts/link-golden.sh --check"],
    );
    assert_eq!(s["impl"], json!(["golden/REVISION"]));
}

#[test]
fn a_ref_naming_no_path_protects_nothing() {
    let s = explain(
        &["Cargo.toml", "crates/vex-core/"],
        &["cargo test --workspace"],
    );
    assert_eq!(s["impl"], json!(["Cargo.toml", "crates/vex-core/"]));
    assert_eq!(s["protected"], json!([]));
}

#[test]
fn a_whole_crate_test_run_protects_the_crates_tests_directory() {
    // `cargo test -p C` runs every integration test of C: they judge the
    // crate's source, so the source is reverted and the tests are kept.
    let s = explain(&["crates/vex-resolve/"], &["cargo test -p vex-resolve"]);
    assert_eq!(s["impl"], json!(["crates/vex-resolve/"]));
    assert_eq!(s["protected"], json!(["crates/vex-resolve/tests/"]));
    assert_eq!(s["mode"], "subject");
}
