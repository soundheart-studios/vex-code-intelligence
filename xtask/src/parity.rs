//! `cargo xtask parity`: vex against Cortex's golden outputs.
//!
//! The golden set is pinned by `golden/REVISION` and never committed. It is
//! found at `$VEX_GOLDEN_DIR`, else the `golden/data` symlink, else
//! `~/Documents/cortex-corpora/golden/<REVISION>`. The last fallback is what a
//! scratch verification worktree uses, since the symlink is untracked.
//!
//! Modes:
//! - `--verify-only`: the pin matches, the export was clean, and every output
//!   and every source file still hashes to what the manifest recorded. A
//!   golden set that silently changed is no oracle at all.
//! - `--diff A B`: the first JSON path at which two files differ.
//! - default: compare vex's own output with golden. Refuses until vex has an
//!   extractor, rather than reporting a comparison that never happened.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(format!("{:x}", Sha256::digest(&bytes)))
}

/// The corpora root holding `golden/`, the frozen corpora and `graph/packages`.
fn default_corpora() -> Option<PathBuf> {
    if let Some(c) = std::env::var_os("VEX_CORTEX_CORPORA") {
        return Some(PathBuf::from(c));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Documents/cortex-corpora"))
}

pub fn revision(root: &Path) -> Result<String, String> {
    let path = root.join("golden/REVISION");
    let rev =
        std::fs::read_to_string(&path).map_err(|e| format!("cannot read golden/REVISION: {e}"))?;
    let rev = rev.trim().to_string();
    if rev.len() != 40 || !rev.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("golden/REVISION is not a full commit sha: {rev:?}"));
    }
    Ok(rev)
}

fn golden_dir(root: &Path, rev: &str, corpora: Option<&Path>) -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("VEX_GOLDEN_DIR") {
        return Some(PathBuf::from(d));
    }
    let link = root.join("golden/data");
    if link.join("manifest.json").is_file() {
        return Some(link);
    }
    corpora.map(|c| c.join("golden").join(rev))
}

/// Where the manifest's source group `name` lives under the corpora root.
fn source_root(corpora: &Path, group: &str) -> PathBuf {
    match group.strip_prefix("graph/") {
        Some(pkg) => corpora.join("graph/packages").join(pkg),
        None => corpora.join(group).join("corpus"),
    }
}

#[derive(Debug, Default)]
pub struct VerifyReport {
    pub outputs: usize,
    pub sources: usize,
    pub problems: Vec<String>,
}

/// Verify a golden directory against its manifest and the pinned revision.
pub fn verify(golden: &Path, corpora: &Path, pinned: &str) -> VerifyReport {
    let mut report = VerifyReport::default();
    let manifest: Value = match std::fs::read(golden.join("manifest.json"))
        .map_err(|e| e.to_string())
        .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
    {
        Ok(m) => m,
        Err(e) => {
            report
                .problems
                .push(format!("no readable manifest in {}: {e}", golden.display()));
            return report;
        }
    };
    if manifest["cortex_revision"] != pinned {
        report.problems.push(format!(
            "manifest is revision {}, golden/REVISION pins {pinned}",
            manifest["cortex_revision"]
        ));
    }
    if manifest["dirty"] != false {
        report
            .problems
            .push("golden set was exported from a dirty Cortex tree".into());
    }
    let outputs: BTreeMap<String, String> =
        serde_json::from_value(manifest["outputs"].clone()).unwrap_or_default();
    if outputs.is_empty() {
        report.problems.push("manifest lists no outputs".into());
    }
    for (name, want) in &outputs {
        report.outputs += 1;
        match sha256_file(&golden.join(name)) {
            Ok(got) if &got == want => {}
            Ok(_) => report.problems.push(format!("output changed: {name}")),
            Err(e) => report.problems.push(format!("output missing: {e}")),
        }
    }
    let sources: BTreeMap<String, BTreeMap<String, String>> =
        serde_json::from_value(manifest["sources"].clone()).unwrap_or_default();
    for (group, files) in &sources {
        let base = source_root(corpora, group);
        for (rel, want) in files {
            report.sources += 1;
            match sha256_file(&base.join(rel)) {
                Ok(got) if &got == want => {}
                Ok(_) => report
                    .problems
                    .push(format!("source changed: {group}/{rel}")),
                Err(e) => report.problems.push(format!("source missing: {e}")),
            }
        }
    }
    report
}

/// The first JSON path at which `a` and `b` differ, with both values.
pub fn first_difference(a: &Value, b: &Value) -> Option<String> {
    diff_at("$", a, b)
}

fn short(v: &Value) -> String {
    let s = v.to_string();
    if s.chars().count() > 80 {
        format!("{}…", s.chars().take(80).collect::<String>())
    } else {
        s
    }
}

fn diff_at(path: &str, a: &Value, b: &Value) -> Option<String> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let mut keys: Vec<&String> = x.keys().chain(y.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                let p = format!("{path}.{k}");
                match (x.get(k), y.get(k)) {
                    (Some(l), Some(r)) => {
                        if let Some(d) = diff_at(&p, l, r) {
                            return Some(d);
                        }
                    }
                    (Some(l), None) => return Some(format!("{p}: only in left ({})", short(l))),
                    (None, Some(r)) => return Some(format!("{p}: only in right ({})", short(r))),
                    (None, None) => unreachable!(),
                }
            }
            None
        }
        (Value::Array(x), Value::Array(y)) => {
            for (i, (l, r)) in x.iter().zip(y).enumerate() {
                if let Some(d) = diff_at(&format!("{path}[{i}]"), l, r) {
                    return Some(d);
                }
            }
            (x.len() != y.len()).then(|| format!("{path}: length {} vs {}", x.len(), y.len()))
        }
        _ if a == b => None,
        _ => Some(format!("{path}: {} vs {}", short(a), short(b))),
    }
}

fn read_json(path: &str) -> Result<Value, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{path}: {e}"))
}

/// The golden export's `edges/` directory, if it can be located.
pub fn golden_edges_dir(root: &Path) -> Option<PathBuf> {
    let rev = revision(root).ok()?;
    let corpora = default_corpora();
    let dir = golden_dir(root, &rev, corpora.as_deref())?.join("edges");
    dir.is_dir().then_some(dir)
}

/// `--edges PKG... [--rows vex|cortex]`: CALLS edges against the golden
/// export, end to end by default (vex indexes the package itself).
fn edges_mode(root: &Path, args: &[String]) -> i32 {
    let Some(golden) = golden_edges_dir(root) else {
        eprintln!("golden edges unavailable");
        return 1;
    };
    let source = crate::bench::RowSource::from_args(args);
    let packages: Vec<&String> = args
        .iter()
        .enumerate()
        .filter(|(i, a)| !a.starts_with("--") && (*i == 0 || args[i - 1] != "--rows"))
        .map(|(_, a)| a)
        .collect();
    if packages.is_empty() {
        eprintln!("usage: cargo xtask parity --edges PKG... [--rows vex|cortex]");
        return 1;
    }
    println!("rows: {}", source.label());
    let mut failed = 0;
    for pkg in packages {
        let want = std::fs::read(golden.join(format!("{pkg}.json")))
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
        let (Some(want), Ok((rows, _))) = (want, crate::bench::package_rows(root, pkg, source))
        else {
            eprintln!("{pkg}: rows or golden edges unavailable");
            failed += 1;
            continue;
        };
        let got = vex_resolve::edges_json(&vex_resolve::rebuild_call_edges(&rows));
        if got == want {
            println!(
                "{pkg}: {} edges match golden",
                want.as_array().map_or(0, Vec::len)
            );
        } else {
            println!(
                "{pkg}: MISMATCH {}",
                first_difference(&got, &want).unwrap_or_default()
            );
            failed += 1;
        }
    }
    if failed > 0 { 1 } else { 0 }
}

/// The MVP's stable six: Python, TypeScript/JavaScript, Go, Java, Rust and
/// C/C++ (as `language_of` names them).
pub const MVP_LANGUAGES: &[&str] = &[
    "python",
    "typescript",
    "javascript",
    "go",
    "java",
    "rust",
    "c",
    "cpp",
];

pub fn main(root: &Path, args: &[String]) -> i32 {
    if let Some(i) = args.iter().position(|a| a == "--edges") {
        let packages: Vec<String> = args[i + 1..]
            .iter()
            .filter(|a| !a.starts_with("--"))
            .cloned()
            .collect();
        return edges_mode(root, &packages);
    }
    let flag = |name: &str| args.iter().position(|a| a == name);
    let value = |name: &str| flag(name).and_then(|i| args.get(i + 1)).map(String::as_str);

    if let Some(i) = flag("--diff") {
        let (Some(a), Some(b)) = (args.get(i + 1), args.get(i + 2)) else {
            eprintln!("usage: cargo xtask parity --diff LEFT.json RIGHT.json");
            return 1;
        };
        return match (read_json(a), read_json(b)) {
            (Ok(l), Ok(r)) => match first_difference(&l, &r) {
                None => {
                    println!("identical");
                    0
                }
                Some(d) => {
                    println!("{d}");
                    1
                }
            },
            (Err(e), _) | (_, Err(e)) => {
                eprintln!("{e}");
                1
            }
        };
    }

    let (pinned, corpora, golden) = match locate(root, value("--corpora"), value("--golden")) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };

    if flag("--verify-only").is_some() {
        let r = verify(&golden, &corpora, &pinned);
        for p in r.problems.iter().take(20) {
            eprintln!("{p}");
        }
        if !r.problems.is_empty() {
            eprintln!("golden set NOT verified: {} problem(s)", r.problems.len());
            return 1;
        }
        println!(
            "golden set verified at {pinned}: {} outputs, {} sources",
            r.outputs, r.sources
        );
        return 0;
    }

    let mut langs: Vec<String> = args
        .iter()
        .enumerate()
        .filter(|(_, a)| *a == "--lang")
        .filter_map(|(i, _)| args.get(i + 1).cloned())
        .collect();
    let mvp = flag("--mvp").is_some();
    if mvp {
        langs.extend(MVP_LANGUAGES.iter().map(|l| l.to_string()));
    }
    let r = compare(&golden, &corpora, &langs);
    for (key, diff) in r.mismatches.iter().take(25) {
        println!("MISMATCH {key}: {diff}");
    }
    let label = if mvp {
        "stable six".to_string()
    } else if langs.is_empty() {
        "all languages".to_string()
    } else {
        langs.join(", ")
    };
    if r.checked == 0 {
        eprintln!("parity ({label}): no golden file was compared; nothing is proved");
        return 1;
    }
    println!(
        "parity ({label}): {}/{} files match golden",
        r.checked - r.mismatches.len(),
        r.checked
    );
    if r.mismatches.is_empty() { 0 } else { 1 }
}

/// (pinned revision, corpora root, golden directory), from explicit paths
/// or the defaults.
pub fn locate(
    root: &Path,
    corpora: Option<&str>,
    golden: Option<&str>,
) -> Result<(String, PathBuf, PathBuf), String> {
    let pinned = revision(root)?;
    let corpora = corpora.map(PathBuf::from).or_else(default_corpora);
    let golden = golden
        .map(PathBuf::from)
        .or_else(|| golden_dir(root, &pinned, corpora.as_deref()));
    match (corpora, golden) {
        (Some(c), Some(g)) => Ok((pinned, c, g)),
        _ => Err("cannot locate the corpora: set VEX_CORTEX_CORPORA or HOME".into()),
    }
}

#[derive(Debug, Default)]
pub struct CompareReport {
    pub checked: usize,
    /// (golden key, first differing JSON path).
    pub mismatches: Vec<(String, String)>,
}

/// Run vex's extraction on every golden source file whose language is in
/// `langs` (all languages when empty) and compare its canonical Parsed with
/// the golden bytes.
pub fn compare(golden: &Path, corpora: &Path, langs: &[String]) -> CompareReport {
    let mut report = CompareReport::default();
    let manifest: Value = match std::fs::read(golden.join("manifest.json"))
        .map_err(|e| e.to_string())
        .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
    {
        Ok(m) => m,
        Err(e) => {
            report.mismatches.push(("manifest.json".into(), e));
            return report;
        }
    };
    let sources: BTreeMap<String, BTreeMap<String, String>> =
        serde_json::from_value(manifest["sources"].clone()).unwrap_or_default();
    for (group, files) in &sources {
        let base = source_root(corpora, group);
        let resolve = vex_app::parse::source_resolver(&base);
        for rel in files.keys() {
            let key = format!("{group}/{rel}");
            let Ok(bytes) = std::fs::read(base.join(rel)) else {
                report.mismatches.push((key, "source unreadable".into()));
                continue;
            };
            let head = &bytes[..bytes.len().min(vex_extract::language::HEAD_BYTES)];
            let lang = vex_extract::language::language_of(rel, Some(head));
            if !langs.is_empty() && !langs.iter().any(|l| l == lang) {
                continue;
            }
            report.checked += 1;
            let got = vex_core::canonical::parsed_bytes(Some(
                &vex_extract::engine::chunk_bytes_with(rel, lang, &bytes, &resolve),
            ));
            let want = std::fs::read(golden.join(format!("parsed/{key}.json"))).unwrap_or_default();
            if got != want {
                let diff = match (
                    serde_json::from_slice::<Value>(&got),
                    serde_json::from_slice::<Value>(&want),
                ) {
                    (Ok(g), Ok(w)) => {
                        first_difference(&g, &w).unwrap_or_else(|| "bytes differ".into())
                    }
                    _ => "unreadable output".into(),
                };
                report.mismatches.push((key, diff));
            }
        }
    }
    report
}
