//! `cargo xtask guards`: structural invariants about the SHAPE of the
//! codebase, which no amount of passing behaviour tests would catch.
//!
//! They only read source and manifests, so they run in a fraction of a
//! second, which is what makes a blocking pre-commit hook defensible
//! (scripts/install-git-hooks.sh). Each guard enforces an ADR.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Every guard, by the name the hook installer and the report use.
pub const GUARDS: &[(&str, &str)] = &[
    ("pure-crates", "vex-extract and vex-resolve do no I/O"),
    (
        "thin-adapters",
        "vex-cli, vex-daemon and vex-mcp reach logic only through vex-app",
    ),
    (
        "one-rrf",
        "exactly one reciprocal-rank fusion implementation exists",
    ),
    (
        "no-interpreters",
        "shipped crates never spawn python, node or uv",
    ),
    (
        "no-http",
        "only vex-embed may depend on an HTTP client (the model download)",
    ),
    (
        "golden-untracked",
        "golden data is never committed; only the pin and gate files are",
    ),
];

const PURE: &[&str] = &["vex-extract", "vex-resolve"];
const ADAPTERS: &[&str] = &["vex-cli", "vex-daemon", "vex-mcp"];
/// vex crates an adapter may depend on: the API and the shared types.
const ADAPTER_MAY_USE: &[&str] = &["vex-app", "vex-core"];
/// The `vex` binary hosts the other adapters (`vex mcp`, `vex daemon`), so
/// vex-cli may also depend on them; they still reach logic only through
/// vex-app.
const CLI_MAY_HOST: &[&str] = &["vex-mcp", "vex-daemon"];
const IO_MARKERS: &[&str] = &[
    "std::fs",
    "std::net",
    "std::process",
    "std::env::var",
    "File::open",
    "File::create",
];
const IO_CRATES: &[&str] = &[
    "rusqlite",
    "tokio",
    "reqwest",
    "ureq",
    "hyper",
    "notify",
    "interprocess",
];
const HTTP_CRATES: &[&str] = &["reqwest", "ureq", "hyper", "isahc", "attohttpc", "surf"];
const INTERPRETERS: &[&str] = &[
    "python", "python3", "node", "npx", "uv", "pip", "pip3", "ruby", "perl",
];
const GOLDEN_TRACKED: &[&str] = &[
    "golden/REVISION",
    "golden/ported.json",
    "golden/baseline.json",
    "golden/regression-review.json",
];

fn crates(root: &Path) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = std::fs::read_dir(root.join("crates"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("Cargo.toml").is_file())
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .collect();
    out.sort();
    out
}

/// Rust sources under a directory, recursively, sorted.
fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Names under `[dependencies]` (not dev or build dependencies).
fn dependencies(krate: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(krate.join("Cargo.toml")) else {
        return vec![];
    };
    let Ok(doc) = text.parse::<toml::Table>() else {
        return vec![];
    };
    doc.get("dependencies")
        .and_then(|d| d.as_table())
        .map(|t| t.keys().cloned().collect())
        .unwrap_or_default()
}

/// Source lines with `//` comments stripped, so a doc mentioning `std::fs`
/// does not trip a guard about code.
fn code_lines(file: &Path) -> Vec<(usize, String)> {
    std::fs::read_to_string(file)
        .unwrap_or_default()
        .lines()
        .enumerate()
        .map(|(i, l)| (i + 1, l.split("//").next().unwrap_or("").to_string()))
        .collect()
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).display().to_string()
}

fn pure_crates(root: &Path) -> Vec<String> {
    let mut v = Vec::new();
    for (name, dir) in crates(root)
        .into_iter()
        .filter(|(n, _)| PURE.contains(&n.as_str()))
    {
        for dep in dependencies(&dir)
            .iter()
            .filter(|d| IO_CRATES.contains(&d.as_str()))
        {
            v.push(format!("{name} depends on I/O crate {dep}"));
        }
        for f in rust_files(&dir.join("src")) {
            for (n, line) in code_lines(&f) {
                if let Some(m) = IO_MARKERS.iter().find(|m| line.contains(*m)) {
                    v.push(format!("{}:{n} uses {m}", rel(root, &f)));
                }
            }
        }
    }
    v
}

fn thin_adapters(root: &Path) -> Vec<String> {
    let mut v = Vec::new();
    for (name, dir) in crates(root)
        .into_iter()
        .filter(|(n, _)| ADAPTERS.contains(&n.as_str()))
    {
        for dep in dependencies(&dir) {
            let hosted = name == "vex-cli" && CLI_MAY_HOST.contains(&dep.as_str());
            if dep.starts_with("vex-") && !ADAPTER_MAY_USE.contains(&dep.as_str()) && !hosted {
                v.push(format!(
                    "{name} depends on {dep}; adapters reach logic only through vex-app"
                ));
            }
        }
    }
    v
}

fn one_rrf(root: &Path) -> Vec<String> {
    let mut defs = Vec::new();
    for (_, dir) in crates(root) {
        for f in rust_files(&dir.join("src")) {
            for (n, line) in code_lines(&f) {
                let Some(after) = line.split("fn ").nth(1) else {
                    continue;
                };
                let ident: String = after
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                let lower = ident.to_lowercase();
                if lower.contains("rrf") || lower.contains("reciprocal_rank") {
                    defs.push(format!("{}:{n} {ident}", rel(root, &f)));
                }
            }
        }
    }
    if defs.len() > 1 {
        vec![format!(
            "{} reciprocal-rank implementations: {}",
            defs.len(),
            defs.join(", ")
        )]
    } else {
        vec![]
    }
}

fn no_interpreters(root: &Path) -> Vec<String> {
    let mut v = Vec::new();
    for (name, dir) in crates(root) {
        if name == "vex-test-support" {
            continue;
        }
        for f in rust_files(&dir.join("src")) {
            for (n, line) in code_lines(&f) {
                for part in line.split("Command::new(").skip(1) {
                    let arg = part.trim_start().trim_start_matches('"');
                    let prog: String = arg
                        .chars()
                        .take_while(|c| !matches!(c, '"' | ')' | ' '))
                        .collect();
                    if INTERPRETERS.contains(&prog.as_str()) {
                        v.push(format!("{}:{n} spawns {prog}", rel(root, &f)));
                    }
                }
            }
        }
    }
    v
}

fn no_http(root: &Path) -> Vec<String> {
    let mut v = Vec::new();
    for (name, dir) in crates(root) {
        if name == "vex-embed" {
            continue;
        }
        for dep in dependencies(&dir)
            .iter()
            .filter(|d| HTTP_CRATES.contains(&d.as_str()))
        {
            v.push(format!("{name} depends on HTTP client {dep}"));
        }
    }
    v
}

fn golden_untracked(root: &Path) -> Vec<String> {
    let out = Command::new("git")
        .args(["ls-files", "--", "golden"])
        .current_dir(root)
        .output();
    let Ok(out) = out else {
        return vec!["cannot run git ls-files".into()];
    };
    if !out.status.success() {
        return vec![]; // not a git checkout: nothing can be tracked
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|f| !GOLDEN_TRACKED.contains(f))
        .map(|f| format!("{f} is tracked; golden data must never be committed"))
        .collect()
}

/// Run every guard; (name, violations) in GUARDS order.
pub fn check(root: &Path) -> Vec<(&'static str, Vec<String>)> {
    GUARDS
        .iter()
        .map(|(name, _)| {
            let found = match *name {
                "pure-crates" => pure_crates(root),
                "thin-adapters" => thin_adapters(root),
                "one-rrf" => one_rrf(root),
                "no-interpreters" => no_interpreters(root),
                "no-http" => no_http(root),
                "golden-untracked" => golden_untracked(root),
                _ => unreachable!(),
            };
            (*name, found)
        })
        .collect()
}

pub fn main(default_root: &Path, args: &[String]) -> i32 {
    if args.first().map(String::as_str) == Some("--list") {
        for (name, what) in GUARDS {
            println!("{name}\t{what}");
        }
        return 0;
    }
    let root = args
        .iter()
        .position(|a| a == "--root")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| default_root.to_path_buf());
    let mut failed = 0;
    for (name, violations) in check(&root) {
        if violations.is_empty() {
            println!("ok   {name}");
        } else {
            failed += 1;
            println!("FAIL {name}");
            for v in violations {
                println!("     {v}");
            }
        }
    }
    if failed > 0 {
        eprintln!("{failed} structural guard(s) failed");
        1
    } else {
        0
    }
}
