//! `cargo xtask bench [PKG...] [--edges] [--mvp]`: the hand-labelled
//! call-graph benchmark (a port of Cortex's docs/evaluations/graph/score.py),
//! checked site by site against Cortex's own scoring at the golden revision.
//!
//! Rows source: by default vex indexes each package itself (end to end:
//! extraction, pipeline, resolver, scorer). `--rows cortex` scores the rows
//! Cortex's own index held instead (crates/vex-resolve/tests/fixtures/rows),
//! which isolates the resolver and scorer when diagnosing a difference.

use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use vex_resolve::{Edge, edges_json, rebuild_call_edges, rows_from_json};

pub const PACKAGES: &[&str] = &[
    "chi",
    "express",
    "itsdangerous",
    "javapoet",
    "kotlinpoet",
    "ky",
    "leveldb",
    "mediatr",
    "psr7",
    "rack",
    "semver",
];
/// The stable-six packages the MVP gate scores.
pub const MVP_PACKAGES: &[&str] = &[
    "chi",
    "express",
    "itsdangerous",
    "javapoet",
    "ky",
    "leveldb",
    "semver",
];

fn fixtures(root: &Path) -> PathBuf {
    root.join("crates/vex-resolve/tests/fixtures")
}

fn read(path: &Path) -> Result<Value, String> {
    let b = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_slice(&b).map_err(|e| format!("{}: {e}", path.display()))
}

/// `callee_name`: the last name in a callee expression.
pub fn callee_name(text: &str) -> String {
    let mut t = text.trim().to_string();
    // re.sub(r"::<.*>$", "", t): from the first `::<` when t ends with `>`.
    if t.ends_with('>')
        && let Some(i) = t.find("::<")
    {
        t.truncate(i);
    }
    // re.sub(r"<[^<>()]*>$", "", t): only the last `<` can open it.
    if t.ends_with('>')
        && let Some(i) = t[..t.len() - 1].rfind('<')
        && !t[i + 1..t.len() - 1].contains(['<', '>', '(', ')'])
    {
        t.truncate(i);
    }
    // re.split(r"\.|->|::", t)[-1].strip(): scan left to right as the
    // regex does, remembering where the last separator ended.
    let mut cut = 0;
    let bytes = t.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'.' {
            cut = i + 1;
            i += 1;
        } else if t[i..].starts_with("->") || t[i..].starts_with("::") {
            cut = i + 2;
            i += 2;
        } else {
            i += 1;
        }
    }
    t[cut..].trim().to_string()
}

/// `path/file.py:Class.method` -> `path.file.py.Class.method`.
pub fn target_qn(target: &str) -> String {
    let (rel, local) = target.split_once(':').unwrap_or((target, ""));
    format!("{}.{local}", rel.replace('/', "."))
}

fn classify(label: &str, expected: &str, predicted: &BTreeMap<String, f64>) -> &'static str {
    if label == "internal" {
        if predicted.contains_key(expected) {
            "correct"
        } else if predicted.is_empty() {
            "missing"
        } else {
            "wrong"
        }
    } else if predicted.is_empty() {
        "abstained"
    } else {
        "wrong"
    }
}

/// `(anonymous@L:C)` at the end of a qualified name.
fn anonymous_position(qn: &str) -> Option<(i64, i64)> {
    let inner = qn.strip_suffix(')')?.rsplit_once("(anonymous@")?.1;
    let (l, c) = inner.split_once(':')?;
    if l.is_empty()
        || c.is_empty()
        || !l.chars().all(|x| x.is_ascii_digit())
        || !c.chars().all(|x| x.is_ascii_digit())
    {
        return None;
    }
    Some((l.parse().ok()?, c.parse().ok()?))
}

/// Where the rows come from.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RowSource {
    Vex,
    Cortex,
}

impl RowSource {
    pub fn from_args(args: &[String]) -> RowSource {
        match args
            .iter()
            .position(|a| a == "--rows")
            .and_then(|i| args.get(i + 1))
            .map(String::as_str)
        {
            Some("cortex") => RowSource::Cortex,
            _ => RowSource::Vex,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RowSource::Vex => "vex's own index (end to end)",
            RowSource::Cortex => "Cortex's recorded index (resolver + scorer only)",
        }
    }
}

type Site = (String, String, i64, i64);

/// The rows and definition sites of one benchmark package.
pub fn package_rows(
    root: &Path,
    pkg: &str,
    source: RowSource,
) -> Result<(vex_resolve::Rows, Vec<Site>), String> {
    match source {
        RowSource::Cortex => {
            let v = read(&fixtures(root).join(format!("rows/{pkg}.json")))?;
            let sites = v["sites"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|r| {
                    (
                        r[0].as_str().unwrap_or("").into(),
                        r[1].as_str().unwrap_or("").into(),
                        r[2].as_i64().unwrap_or(0),
                        r[3].as_i64().unwrap_or(0),
                    )
                })
                .collect();
            Ok((rows_from_json(&v), sites))
        }
        RowSource::Vex => index_package(root, pkg),
    }
}

/// Copy the package's listed files (hash-checked) into a scratch tree and
/// index it with vex's own pipeline.
fn index_package(root: &Path, pkg: &str) -> Result<(vex_resolve::Rows, Vec<Site>), String> {
    use sha2::{Digest, Sha256};
    let sites_json = read(&fixtures(root).join("graph/sites.json"))?;
    let indexed = sites_json["packages"][pkg]["indexed_files"]
        .as_object()
        .ok_or(format!("{pkg}: no indexed_files"))?;
    let corpora = std::env::var_os("VEX_CORTEX_CORPORA")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Documents/cortex-corpora"))
        })
        .ok_or("cannot locate the corpora")?;
    let package_dir = corpora.join("graph/packages").join(pkg);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let work = std::env::temp_dir().join(format!("vex-bench-{pkg}-{}-{nanos}", std::process::id()));
    let result = (|| {
        let src = work.join("src");
        for (rel, sha) in indexed {
            let data =
                std::fs::read(package_dir.join(rel)).map_err(|e| format!("{pkg}/{rel}: {e}"))?;
            if format!("{:x}", Sha256::digest(&data)) != sha.as_str().unwrap_or("") {
                return Err(format!("hash mismatch: {pkg}/{rel}"));
            }
            let dst = src.join(rel);
            std::fs::create_dir_all(dst.parent().unwrap()).map_err(|e| e.to_string())?;
            std::fs::write(dst, data).map_err(|e| e.to_string())?;
        }
        let idx = vex_store::db::Index::open(&work.join("index.db")).map_err(|e| e.to_string())?;
        vex_app::index::run(&idx, &src, false).map_err(|e| e.to_string())?;
        let rows = vex_app::index::read_rows(&idx, &src).map_err(|e| e.to_string())?;
        let mut stmt = idx
            .conn
            .prepare("SELECT qualified_name, file_path, start_line, end_line FROM symbol_sites")
            .map_err(|e| e.to_string())?;
        let sites: Vec<Site> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        Ok((rows, sites))
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}

struct Scored {
    caller: String,
    predicted: BTreeMap<String, f64>,
    outcome: &'static str,
}

fn score_site(
    site: &Value,
    sites: &[(String, String, i64, i64)],
    edges: &[Edge],
    names: &HashMap<String, String>,
) -> Scored {
    let file = site["file"].as_str().unwrap_or("");
    let line = site["line"].as_i64().unwrap_or(0);
    let col = site["col"].as_i64();
    let name = callee_name(site["callee"].as_str().unwrap_or(""));
    let label = site["label"].as_str().unwrap_or("");
    let expected = if label == "internal" {
        target_qn(site["target"].as_str().unwrap_or(""))
    } else {
        String::new()
    };
    let mut spanning: Vec<&(String, String, i64, i64)> = sites
        .iter()
        .filter(|(_, fp, s, e)| fp == file && *s <= line && *e >= line)
        .collect();
    spanning.sort_by_key(|(_, _, s, e)| e - s); // stable, as SQLite's ORDER BY over a scan
    let mut caller = format!("file:{file}");
    for (qn, _, _, _) in spanning {
        if let (Some(c), Some((al, ac))) = (col, anonymous_position(qn))
            && al == line
            && ac > c
        {
            continue;
        }
        caller = qn.clone();
        break;
    }
    let predicted: BTreeMap<String, f64> = edges
        .iter()
        .filter(|e| e.src == caller && names.get(&e.dst).map(String::as_str) == Some(name.as_str()))
        .map(|e| (e.dst.clone(), e.confidence))
        .collect();
    let outcome = classify(label, &expected, &predicted);
    Scored {
        caller,
        predicted,
        outcome,
    }
}

/// One package scored from vex's rows: outcome counts, per-site differences
/// from Cortex's recorded answers, and the CALLS edges.
pub struct PackageScore {
    pub counts: BTreeMap<&'static str, u64>,
    pub site_diffs: Vec<String>,
    pub edges: Vec<Edge>,
}

/// Score `pkg` against `meta` (its sites.json entry) and `expected`
/// (bench_expected.json).
pub fn score_package(
    root: &Path,
    pkg: &str,
    source: RowSource,
    meta: &Value,
    expected: &Value,
) -> Result<PackageScore, String> {
    let (rows, sites) = package_rows(root, pkg, source)?;
    let edges = rebuild_call_edges(&rows);
    let names: HashMap<String, String> = rows
        .symbols
        .iter()
        .map(|(qn, n, _, _)| (qn.clone(), n.clone()))
        .collect();
    let mut counts: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut site_diffs = Vec::new();
    for site in meta["sites"].as_array().into_iter().flatten() {
        let s = score_site(site, &sites, &edges, &names);
        *counts.entry(s.outcome).or_default() += 1;
        let id = site["id"].as_str().unwrap_or("");
        let want = &expected["sites"][id];
        let got = json!({"caller": s.caller, "predicted": s.predicted, "outcome": s.outcome});
        if got["caller"] != want["caller"]
            || got["outcome"] != want["outcome"]
            || got["predicted"] != want["predicted"]
        {
            site_diffs.push(format!(
                "{id}: vex {got} vs Cortex {{caller: {}, predicted: {}, outcome: {}}}",
                want["caller"], want["predicted"], want["outcome"]
            ));
        }
    }
    Ok(PackageScore {
        counts,
        site_diffs,
        edges,
    })
}

/// The benchmark's site and expectation files.
pub fn load_fixtures(root: &Path) -> Result<(Value, Value), String> {
    let fx = fixtures(root);
    Ok((
        read(&fx.join("graph/sites.json"))?,
        read(&fx.join("graph/bench_expected.json"))?,
    ))
}

pub fn main(root: &Path, args: &[String]) -> i32 {
    let check_edges = args.iter().any(|a| a == "--edges");
    let source = RowSource::from_args(args);
    let mut packages: Vec<String> = args
        .iter()
        .enumerate()
        .filter(|(i, a)| !a.starts_with("--") && (*i == 0 || args[i - 1] != "--rows"))
        .map(|(_, a)| a.clone())
        .collect();
    if args.iter().any(|a| a == "--mvp") {
        packages.extend(MVP_PACKAGES.iter().map(|s| s.to_string()));
    }
    if packages.is_empty() {
        packages = PACKAGES.iter().map(|s| s.to_string()).collect();
    }
    let (sites_json, expected) = match load_fixtures(root) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let golden = crate::parity::golden_edges_dir(root);
    let mut failures = 0;
    println!("rows: {}", source.label());
    for pkg in &packages {
        let Some(meta) = sites_json["packages"].get(pkg.as_str()) else {
            eprintln!("{pkg}: not a benchmark package");
            failures += 1;
            continue;
        };
        let PackageScore {
            counts,
            site_diffs,
            edges,
        } = match score_package(root, pkg, source, meta, &expected) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{pkg}: {e}");
                failures += 1;
                continue;
            }
        };
        let c = |k: &str| counts.get(k).copied().unwrap_or(0);
        println!(
            "{pkg}: {} correct / {} wrong / {} missing (+{} abstained)",
            c("correct"),
            c("wrong"),
            c("missing"),
            c("abstained")
        );
        let want_counts = &expected["outcomes"][pkg.as_str()];
        if json!(counts) != *want_counts {
            println!("  MISMATCH outcome counts: Cortex {want_counts}");
            failures += 1;
        }
        for d in site_diffs.iter().take(5) {
            println!("  MISMATCH {d}");
        }
        if !site_diffs.is_empty() {
            failures += 1;
        }
        if check_edges {
            match golden
                .as_ref()
                .map(|g| read(&g.join(format!("{pkg}.json"))))
            {
                Some(Ok(want)) if edges_json(&edges) == want => println!(
                    "  edges: {} match golden",
                    want.as_array().map_or(0, Vec::len)
                ),
                Some(Ok(_)) => {
                    println!("  MISMATCH edges differ from golden");
                    failures += 1;
                }
                Some(Err(e)) => {
                    eprintln!("  {e}");
                    failures += 1;
                }
                None => {
                    eprintln!("  golden edges unavailable");
                    failures += 1;
                }
            }
        }
    }
    if failures > 0 { 1 } else { 0 }
}
