//! Per-language extraction against Cortex's chunk_file on crafted files the
//! golden corpora never exercise (recorded from Cortex in fixtures/lang_cases/). Each
//! language directory is its own project root; symbols (with the bodiless
//! flag), calls, bindings, imports, member rows, chunks and refs must match
//! exactly.

use serde_json::{Value, json};
use std::path::Path;
use vex_extract::engine::chunk_bytes_with;
use vex_extract::language::{HEAD_BYTES, language_of};

/// Cortex's `(path.parent / raw).resolve().relative_to(root.resolve())`
/// for an existing file (vex-app's resolver, restated: this crate's tests
/// cannot depend on vex-app).
fn fs_resolve(root: &Path, dir: &str, written: &str) -> Option<String> {
    let canon_root = std::fs::canonicalize(root).ok()?;
    let resolved = std::fs::canonicalize(root.join(dir).join(written)).ok()?;
    let rel = resolved
        .strip_prefix(&canon_root)
        .ok()?
        .to_string_lossy()
        .into_owned();
    Some(if rel.is_empty() { ".".into() } else { rel })
}

#[test]
fn crafted_files_extract_like_cortex() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lang_cases");
    let cases: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("cases.json")).unwrap()).unwrap();
    let mut failures = Vec::new();
    for (language_dir, files) in cases.as_object().unwrap() {
        for (rel, want) in files.as_object().unwrap() {
            let bytes = std::fs::read(dir.join(language_dir).join(rel)).unwrap();
            let lang = language_of(rel, Some(&bytes[..bytes.len().min(HEAD_BYTES)]));
            let root = dir.join(language_dir);
            let resolve = |d: &str, written: &str| fs_resolve(&root, d, written);
            let parsed = chunk_bytes_with(rel, lang, &bytes, &resolve);
            let got = json!({
                "symbols": parsed.symbols.iter().map(|s| json!([
                    s.qualified_name, s.name, s.kind, s.start_line, s.end_line, s.declaration
                ])).collect::<Vec<_>>(),
                "calls": parsed.calls.iter()
                    .map(|((caller, callee), n)| json!([caller, callee, n]))
                    .collect::<Vec<_>>(),
                "bindings": parsed.bindings.iter()
                    .map(|(k, (m, o))| (k.clone(), json!([m, o])))
                    .collect::<serde_json::Map<_, _>>(),
                "imports": parsed.imports,
                "members": parsed.members.iter().map(|(o, m, t)| json!([o, m, t]))
                    .collect::<Vec<_>>(),
                "chunks": parsed.chunks.iter().map(|c| json!([
                    c.id, c.start_line, c.end_line, c.kind, c.language,
                    c.metadata.get("symbols")
                ])).collect::<Vec<_>>(),
                "refs": parsed.refs.iter().collect::<std::collections::BTreeMap<_, _>>(),
            });
            for key in [
                "symbols", "calls", "bindings", "imports", "members", "chunks", "refs",
            ] {
                if got[key] != want[key] {
                    failures.push(format!(
                        "{language_dir}/{rel} {key}:\n  vex    {}\n  Cortex {}",
                        got[key], want[key]
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn every_wired_language_has_crafted_cases() {
    // A language reaching golden parity must also be probed here.
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lang_cases");
    for language in [
        "js",
        "go",
        "java",
        "rust",
        "c",
        "cpp",
        "objc",
        "csharp",
        "swift",
        "scala",
        "lua",
        "bash",
        "zig",
        "elixir",
        "json",
        "julia",
        "verilog",
        "fortran",
        "powershell",
        "groovy",
        "sql",
        "hcl",
        "kotlin",
        "php",
        "ruby",
    ] {
        assert!(
            dir.join(language).is_dir(),
            "no crafted cases for {language}"
        );
    }
}
