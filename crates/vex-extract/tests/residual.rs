//! Residual and line-fallback chunks against Cortex.
//!
//! - Residual chunks: for every golden Parsed, its non-residual chunk spans
//!   go in and the golden residual chunks must come out (ids, spans, content
//!   hashes, kind, language, metadata).
//! - Line fallback: Cortex's `_chunk_lines` output for every corpus file
//!   (with its own language and with none) and for crafted inputs.
//!
//! The corpora and golden export are not public: these skip loudly under CI
//! and fail anywhere else they are missing.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use vex_core::canonical::{parsed_bytes, to_canonical};
use vex_core::{Chunk, Parsed};
use vex_extract::engine::chunk_bytes;
use vex_extract::fallback::{line_chunks, source_lines};
use vex_extract::residual::fill_gaps;

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn corpora() -> Option<PathBuf> {
    let root = std::env::var_os("VEX_CORTEX_CORPORA")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Documents/cortex-corpora"))
        })?;
    root.is_dir().then_some(root)
}

/// The corpora root, or None when skipping is allowed (CI only).
fn require_corpora() -> Option<PathBuf> {
    match corpora() {
        Some(c) => Some(c),
        None if std::env::var_os("CI").is_some() => {
            eprintln!("SKIPPED: the Cortex corpora are not available in CI");
            None
        }
        None => panic!("the Cortex corpora are missing; set VEX_CORTEX_CORPORA"),
    }
}

fn source_path(corpora: &Path, key: &str) -> (PathBuf, String) {
    match key.strip_prefix("graph/") {
        Some(rest) => {
            let (pkg, rel) = rest.split_once('/').unwrap();
            (corpora.join("graph/packages").join(pkg), rel.to_string())
        }
        None => {
            let (group, rel) = key.split_once('/').unwrap();
            (corpora.join(group).join("corpus"), rel.to_string())
        }
    }
}

fn fixture(name: &str) -> Value {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/fallback")
        .join(name);
    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
}

fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[test]
fn residual_chunks_match_golden_for_every_file() {
    let Some(corpora) = require_corpora() else {
        return;
    };
    let rev = std::fs::read_to_string(workspace().join("golden/REVISION")).unwrap();
    let golden = std::env::var_os("VEX_GOLDEN_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| corpora.join("golden").join(rev.trim()));
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(golden.join("manifest.json")).unwrap()).unwrap();
    let mut failures = Vec::new();
    let (mut files, mut residuals) = (0, 0);
    for (group, rels) in manifest["sources"].as_object().unwrap() {
        for rel in rels.as_object().unwrap().keys() {
            let key = format!("{group}/{rel}");
            let want: Value = serde_json::from_slice(
                &std::fs::read(golden.join(format!("parsed/{key}.json"))).unwrap(),
            )
            .unwrap();
            let chunks = want["chunks"].as_array().unwrap();
            let want_residual: Vec<&Value> =
                chunks.iter().filter(|c| c["kind"] == "residual").collect();
            let claimed: Vec<Chunk> = chunks
                .iter()
                .filter(|c| c["kind"] != "residual")
                .map(|c| {
                    Chunk::new(
                        "",
                        rel.as_str(),
                        c["start_line"].as_u64().unwrap() as usize,
                        c["end_line"].as_u64().unwrap() as usize,
                        "",
                    )
                })
                .collect();
            if claimed.is_empty() {
                continue; // Cortex only fills gaps around AST chunks
            }
            let lang = want_residual
                .first()
                .map(|c| c["language"].as_str().unwrap().to_string())
                .unwrap_or_default();
            let (root, rel_path) = source_path(&corpora, &key);
            let bytes = std::fs::read(root.join(&rel_path)).unwrap();
            let mut p = Parsed {
                chunks: claimed,
                ..Parsed::default()
            };
            let before = p.chunks.len();
            fill_gaps(&rel_path, &lang, &source_lines(&bytes), &mut p);
            let produced = Parsed {
                chunks: p.chunks.split_off(before),
                ..Parsed::default()
            };
            let got = to_canonical(Some(&produced));
            let got_chunks = got["chunks"].as_array().unwrap();
            files += 1;
            residuals += want_residual.len();
            if got_chunks.len() != want_residual.len()
                || got_chunks.iter().zip(&want_residual).any(|(g, w)| g != *w)
            {
                failures.push(format!(
                    "{key}: {} residual chunks, golden {}",
                    got_chunks.len(),
                    want_residual.len()
                ));
            }
        }
    }
    assert!(
        files > 1000 && residuals > 1000,
        "only {files} files / {residuals} residual chunks compared"
    );
    assert!(
        failures.is_empty(),
        "{} files differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn line_fallback_matches_cortex_for_every_corpus_file() {
    let Some(corpora) = require_corpora() else {
        return;
    };
    let oracle = fixture("corpus.json");
    let mut failures = Vec::new();
    for (key, want) in oracle.as_object().unwrap() {
        let (root, rel) = source_path(&corpora, key);
        let bytes = std::fs::read(root.join(&rel)).unwrap();
        let lang = want["language"].as_str().unwrap();
        if sha(&parsed_bytes(Some(&line_chunks(&rel, lang, &bytes)))) != want["sha256"] {
            failures.push(format!("{key} ({lang})"));
        }
        if sha(&parsed_bytes(Some(&line_chunks(&rel, "", &bytes)))) != want["sha256_no_language"] {
            failures.push(format!("{key} (no language)"));
        }
    }
    assert_eq!(oracle.as_object().unwrap().len(), 1267);
    assert!(
        failures.is_empty(),
        "{} differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn line_fallback_matches_cortex_on_crafted_inputs() {
    let samples = fixture("samples.json");
    for (name, case) in samples["crafted"].as_object().unwrap() {
        let bytes = hex(case["bytes_hex"].as_str().unwrap());
        let lang = case["language"].as_str().unwrap();
        let got = String::from_utf8(parsed_bytes(Some(&line_chunks(name, lang, &bytes)))).unwrap();
        assert_eq!(got, case["canonical"].as_str().unwrap(), "{name}");
    }
}

#[test]
fn without_a_grammar_the_facade_is_the_line_chunker() {
    // Cortex `chunk_file` with no language: `_chunk_lines(root, path, "")`.
    let samples = fixture("samples.json");
    for (name, case) in samples["crafted"].as_object().unwrap() {
        let bytes = hex(case["bytes_hex"].as_str().unwrap());
        assert_eq!(
            parsed_bytes(Some(&chunk_bytes(name, "", &bytes))),
            parsed_bytes(Some(&line_chunks(name, "", &bytes)))
        );
    }
    assert_eq!(
        to_canonical(Some(&chunk_bytes("e.txt", "", b"")))["chunks"],
        json!([])
    );
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}
