//! language_of against Cortex's own answers: 79 crafted cases (shebangs,
//! suffix rules, C-family header routing) and every corpus and benchmark
//! file, with a byte-exact check of the header lexer's masking.

use base64::Engine;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use vex_extract::cpp_lex::header_code;
use vex_extract::language::{HEAD_BYTES, language_of};

fn fixture(name: &str) -> Value {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/language")
        .join(name);
    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
}

fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[test]
fn crafted_cases_match_cortex() {
    let cases = fixture("cases.json");
    let mut failures = Vec::new();
    for case in cases.as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let head = base64::engine::general_purpose::STANDARD
            .decode(case["head_b64"].as_str().unwrap())
            .unwrap();
        let got = language_of(name, Some(&head));
        if got != case["language"] {
            failures.push(format!(
                "{name} {:?}: got {got:?}, Cortex {}",
                String::from_utf8_lossy(&head[..head.len().min(60)]),
                case["language"]
            ));
        }
        if let Some(want) = case.get("header_code_sha256") {
            let masked = header_code(&head[..head.len().min(HEAD_BYTES)]);
            if sha(&masked) != *want {
                failures.push(format!(
                    "{name} {:?}: header_code differs",
                    String::from_utf8_lossy(&head[..head.len().min(60)])
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} cases differ:\n{}",
        failures.len(),
        cases.as_array().unwrap().len(),
        failures.join("\n")
    );
}

#[test]
fn an_unreadable_header_is_c() {
    assert_eq!(language_of("x.h", None), "c");
}

#[test]
fn an_unreadable_extensionless_file_has_no_language() {
    assert_eq!(language_of("script", None), "");
}

fn corpora() -> Option<PathBuf> {
    let root = std::env::var_os("VEX_CORTEX_CORPORA")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Documents/cortex-corpora"))
        })?;
    root.is_dir().then_some(root)
}

/// Every corpus and benchmark-package file. The corpora are not public, so
/// this is skipped (loudly) only under CI; anywhere else a missing corpus is
/// a failure rather than a silent pass.
#[test]
fn every_corpus_file_matches_cortex() {
    let Some(root) = corpora() else {
        if std::env::var_os("CI").is_some() {
            eprintln!("SKIPPED: the Cortex corpora are not available in CI");
            return;
        }
        panic!("the Cortex corpora are missing; set VEX_CORTEX_CORPORA");
    };
    let corpus = fixture("corpus.json");
    let mut failures = Vec::new();
    let mut headers = 0;
    for (key, want) in corpus.as_object().unwrap() {
        let (group, rel) = if let Some(rest) = key.strip_prefix("graph/") {
            let (pkg, rel) = rest.split_once('/').unwrap();
            (root.join("graph/packages").join(pkg), rel.to_string())
        } else {
            let (g, rel) = key.split_once('/').unwrap();
            (root.join(g).join("corpus"), rel.to_string())
        };
        let path = group.join(&rel);
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let head = &bytes[..bytes.len().min(HEAD_BYTES)];
        let got = language_of(&rel, Some(head));
        if got != want["language"] {
            failures.push(format!("{key}: got {got:?}, Cortex {}", want["language"]));
        }
        if let Some(h) = want.get("header_code_sha256") {
            headers += 1;
            if sha(&header_code(head)) != *h {
                failures.push(format!("{key}: header_code differs"));
            }
        }
    }
    assert_eq!(corpus.as_object().unwrap().len(), 1267);
    let h_files = corpus
        .as_object()
        .unwrap()
        .keys()
        .filter(|k| k.to_lowercase().ends_with(".h"))
        .count();
    assert!(h_files > 100, "the corpora hold {h_files} .h files");
    assert_eq!(headers, h_files, "every .h file carries a masking check");
    assert!(
        failures.is_empty(),
        "{} files differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
