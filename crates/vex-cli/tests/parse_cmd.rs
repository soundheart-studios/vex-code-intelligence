//! `vex parse FILE --json` prints exactly Cortex's golden Parsed.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use vex_test_support::TestHome;

fn vex(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_vex"))
        .args(args)
        .output()
        .unwrap()
}

fn corpora() -> Option<PathBuf> {
    let root = std::env::var_os("VEX_CORTEX_CORPORA")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Documents/cortex-corpora"))
        })?;
    root.is_dir().then_some(root)
}

#[test]
fn every_python_golden_file_is_reproduced_byte_for_byte() {
    let Some(corpora) = corpora() else {
        if std::env::var_os("CI").is_some() {
            eprintln!("SKIPPED: the Cortex corpora are not available in CI");
            return;
        }
        panic!("the Cortex corpora are missing; set VEX_CORTEX_CORPORA");
    };
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let rev = std::fs::read_to_string(workspace.join("golden/REVISION")).unwrap();
    let golden = corpora.join("golden").join(rev.trim());
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(golden.join("manifest.json")).unwrap()).unwrap();
    let mut checked = 0;
    let mut failures = Vec::new();
    for (group, files) in manifest["sources"].as_object().unwrap() {
        let root = match group.strip_prefix("graph/") {
            Some(pkg) => corpora.join("graph/packages").join(pkg),
            None => corpora.join(group).join("corpus"),
        };
        for rel in files
            .as_object()
            .unwrap()
            .keys()
            .filter(|r| r.ends_with(".py"))
        {
            let out = vex(&[
                "parse",
                root.join(rel).to_str().unwrap(),
                "--root",
                root.to_str().unwrap(),
                "--json",
            ]);
            let want = std::fs::read(golden.join(format!("parsed/{group}/{rel}.json"))).unwrap();
            checked += 1;
            if !out.status.success() || out.stdout != want {
                failures.push(format!("{group}/{rel}"));
            }
        }
    }
    assert_eq!(checked, 58, "every Python golden file");
    assert!(
        failures.is_empty(),
        "{} differ: {failures:?}",
        failures.len()
    );
}

#[test]
fn a_crafted_file_matches_cortexs_output_without_the_corpora() {
    let cases: Value = serde_json::from_slice(
        &std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../vex-extract/tests/fixtures/python/cases.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let h = TestHome::new();
    let root = h.vex_home().join("proj");
    for name in ["owners.py", "instance_evidence.py", "imports.py"] {
        let file = root.join("pkg").join(name);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, cases[name]["source"].as_str().unwrap()).unwrap();
        let out = vex(&[
            "parse",
            file.to_str().unwrap(),
            "--root",
            root.to_str().unwrap(),
            "--json",
        ]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8(out.stdout).unwrap(),
            cases[name]["canonical"].as_str().unwrap(),
            "{name}"
        );
    }
}

#[test]
fn a_file_outside_the_root_is_a_usage_error() {
    let h = TestHome::new();
    let file = h.vex_home().join("a.py");
    std::fs::write(&file, "x = 1\n").unwrap();
    let elsewhere = h.vex_home().join("other");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let out = vex(&[
        "parse",
        file.to_str().unwrap(),
        "--root",
        elsewhere.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--root"));
}

#[test]
fn a_missing_file_is_a_usage_error() {
    let h = TestHome::new();
    let out = vex(&[
        "parse",
        h.vex_home().join("nope.py").to_str().unwrap(),
        "--root",
        h.vex_home().to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot read"));
}

#[test]
fn the_human_form_summarises_the_extraction() {
    let h = TestHome::new();
    let file = h.vex_home().join("m.py");
    std::fs::write(&file, "import os\n\ndef f():\n    return os.getcwd()\n").unwrap();
    let out = vex(&[
        "parse",
        file.to_str().unwrap(),
        "--root",
        h.vex_home().to_str().unwrap(),
    ]);
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.starts_with("m.py (python)"), "{text}");
    assert!(
        text.contains("symbols: 1") && text.contains("imports: 1"),
        "{text}"
    );
}

#[test]
fn an_extensionless_script_is_routed_by_its_shebang() {
    // The language comes from the file's head bytes when the name has no
    // suffix, exactly as Cortex's language_of reads the shebang.
    let h = TestHome::new();
    let file = h.vex_home().join("tool");
    std::fs::write(&file, "#!/usr/bin/env python3\ndef main():\n    run()\n").unwrap();
    let out = vex(&[
        "parse",
        file.to_str().unwrap(),
        "--root",
        h.vex_home().to_str().unwrap(),
    ]);
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.starts_with("tool (python)"), "{text}");
    assert!(
        text.contains("symbols: 1"),
        "an AST parse, not the line fallback: {text}"
    );
}
