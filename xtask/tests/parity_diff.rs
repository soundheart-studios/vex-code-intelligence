//! The parity harness: first-difference reporting and golden-set verification
//! against a small synthetic golden export.

use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;
use vex_test_support::TestHome;

fn xtask(args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("parity")
        .args(args)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.code().unwrap_or(-1), text)
}

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn sha(body: &str) -> String {
    format!("{:x}", Sha256::digest(body.as_bytes()))
}

fn pinned() -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    std::fs::read_to_string(root.join("golden/REVISION"))
        .unwrap()
        .trim()
        .to_string()
}

/// A golden export with one corpus file and one package file.
fn synthetic(home: &TestHome, revision: &str) -> (PathBuf, PathBuf) {
    let corpora = home.vex_home().join("corpora");
    let golden = corpora.join("golden").join(revision);
    write(&corpora.join("language-475/corpus/a.py"), "def f(): pass\n");
    write(&corpora.join("graph/packages/pkg/b.go"), "package b\n");
    let out = "{\"chunks\":[]}\n";
    write(&golden.join("parsed/language-475/a.py.json"), out);
    let manifest = json!({
        "cortex_revision": revision, "dirty": false, "contract": "docs/design/port-contract.md",
        "outputs": {"parsed/language-475/a.py.json": sha(out)},
        "sources": {"language-475": {"a.py": sha("def f(): pass\n")}, "graph/pkg": {"b.go": sha("package b\n")}},
    });
    write(&golden.join("manifest.json"), &manifest.to_string());
    (golden, corpora)
}

fn verify(golden: &Path, corpora: &Path) -> (i32, String) {
    xtask(&[
        "--verify-only",
        "--golden",
        golden.to_str().unwrap(),
        "--corpora",
        corpora.to_str().unwrap(),
    ])
}

#[test]
fn an_intact_golden_set_verifies() {
    let h = TestHome::new();
    let (g, c) = synthetic(&h, &pinned());
    let (code, text) = verify(&g, &c);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("1 outputs, 2 sources"), "{text}");
}

#[test]
fn an_altered_output_fails_verification_by_name() {
    let h = TestHome::new();
    let (g, c) = synthetic(&h, &pinned());
    write(
        &g.join("parsed/language-475/a.py.json"),
        "{\"chunks\":[1]}\n",
    );
    let (code, text) = verify(&g, &c);
    assert_eq!(code, 1);
    assert!(
        text.contains("output changed: parsed/language-475/a.py.json"),
        "{text}"
    );
}

#[test]
fn an_altered_source_fails_verification_by_name() {
    let h = TestHome::new();
    let (g, c) = synthetic(&h, &pinned());
    write(&c.join("graph/packages/pkg/b.go"), "package changed\n");
    let (code, text) = verify(&g, &c);
    assert_eq!(code, 1);
    assert!(text.contains("source changed: graph/pkg/b.go"), "{text}");
}

#[test]
fn a_manifest_at_another_revision_fails() {
    let h = TestHome::new();
    let other = "0".repeat(40);
    let (g, c) = synthetic(&h, &other);
    let (code, text) = verify(&g, &c);
    assert_eq!(code, 1);
    assert!(text.contains("golden/REVISION pins"), "{text}");
}

#[test]
fn diff_reports_the_first_differing_path() {
    let h = TestHome::new();
    let a = h.vex_home().join("a.json");
    let b = h.vex_home().join("b.json");
    write(
        &a,
        r#"{"chunks":[{"id":"x","end_line":3},{"id":"y","end_line":9}],"refs":{}}"#,
    );
    write(
        &b,
        r#"{"chunks":[{"id":"x","end_line":3},{"id":"y","end_line":10}],"refs":{}}"#,
    );
    let (code, text) = xtask(&["--diff", a.to_str().unwrap(), b.to_str().unwrap()]);
    assert_eq!(code, 1);
    assert_eq!(text.trim(), "$.chunks[1].end_line: 9 vs 10");
}

#[test]
fn diff_reports_missing_keys_and_length_changes() {
    let h = TestHome::new();
    let a = h.vex_home().join("a.json");
    let b = h.vex_home().join("b.json");
    write(&a, r#"{"calls":[1,2],"members":[]}"#);
    write(&b, r#"{"calls":[1,2,3]}"#);
    let (_, text) = xtask(&["--diff", a.to_str().unwrap(), b.to_str().unwrap()]);
    assert_eq!(text.trim(), "$.calls: length 2 vs 3");
}

#[test]
fn identical_files_diff_clean() {
    let h = TestHome::new();
    let a = h.vex_home().join("a.json");
    write(&a, r#"{"chunks":[]}"#);
    let (code, text) = xtask(&["--diff", a.to_str().unwrap(), a.to_str().unwrap()]);
    assert_eq!((code, text.trim()), (0, "identical"));
}
