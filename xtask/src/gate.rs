//! `cargo xtask gate`: nothing that passed before may silently stop passing.
//!
//! A port of converged's scripts/regression_gate.py to vex's parity tiers:
//! 1. the full test suite;
//! 2. parity mismatches for every language declared ported in
//!    golden/ported.json (the baseline is zero; parity is exact);
//! 3. call-graph benchmark scores per package (correct must not fall, wrong
//!    must not rise).
//!
//! A loss fails unless golden/regression-review.json declares that exact new
//! value with a reason. Gains are reported, never failed. A baseline key that
//! is no longer measured is a loss: silence is not a pass. `--write-snapshot`
//! advances the baseline only when every comparison already passes, so an
//! unreviewed loss can never be written into it.

use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Whether a lower or a higher value is better for a measurement key.
fn lower_is_better(key: &str) -> bool {
    !key.ends_with(".correct")
}

/// `{"parity": {"python": {"mismatches": 0}}}` -> `{"parity.python.mismatches": 0}`.
pub fn flatten(v: &Value) -> BTreeMap<String, f64> {
    fn walk(prefix: &str, v: &Value, out: &mut BTreeMap<String, f64>) {
        match v {
            Value::Object(m) => {
                for (k, x) in m {
                    let p = if prefix.is_empty() {
                        k.clone()
                    } else {
                        format!("{prefix}.{k}")
                    };
                    walk(&p, x, out);
                }
            }
            Value::Number(n) => {
                out.insert(prefix.to_string(), n.as_f64().unwrap_or(f64::NAN));
            }
            _ => {}
        }
    }
    let mut out = BTreeMap::new();
    walk("", v, &mut out);
    out
}

fn read(path: &Path) -> Result<Value, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

#[derive(Debug, Default)]
pub struct Outcome {
    pub losses: Vec<String>,
    pub reviewed: Vec<String>,
    pub gains: Vec<String>,
    pub new: Vec<String>,
}

/// Compare a measurement against the baseline, honouring reviewed exceptions.
pub fn compare(baseline: &Value, current: &Value, review: &Value) -> Outcome {
    let base = flatten(baseline);
    let cur = flatten(current);
    let exceptions: BTreeMap<String, (f64, String)> = review["exceptions"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|e| {
                    let reason = e["reason"].as_str().unwrap_or("").trim().to_string();
                    Some((
                        e["key"].as_str()?.to_string(),
                        (e["value"].as_f64()?, reason),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    let mut out = Outcome::default();
    for (key, was) in &base {
        let Some(now) = cur.get(key) else {
            out.losses
                .push(format!("{key}: was {was}, no longer measured"));
            continue;
        };
        let worse = if lower_is_better(key) {
            now > was
        } else {
            now < was
        };
        let better = if lower_is_better(key) {
            now < was
        } else {
            now > was
        };
        if worse {
            match exceptions.get(key) {
                Some((value, reason)) if value == now && !reason.is_empty() => out
                    .reviewed
                    .push(format!("{key}: {was} -> {now} (reviewed: {reason})")),
                _ => out.losses.push(format!("{key}: {was} -> {now}")),
            }
        } else if better {
            out.gains.push(format!("{key}: {was} -> {now}"));
        }
    }
    for key in cur.keys().filter(|k| !base.contains_key(*k)) {
        out.new
            .push(format!("{key}: {} (not yet in the baseline)", cur[key]));
    }
    out
}

/// Measure the tree. Each declared-ported language and package must be
/// measurable; one that is not fails the gate rather than being skipped.
/// Parity counts mismatching golden files per language (a language with no
/// golden file proves nothing and fails); the benchmark scores each
/// package's correct, wrong and missing sites from vex's own index.
fn measure(root: &Path, ported: &Value) -> Result<Value, String> {
    let declared = |key: &str| -> Vec<String> {
        ported[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(String::from)
            .collect()
    };
    let languages = declared("languages");
    let packages = declared("packages");
    let mut parity = Map::new();
    if !languages.is_empty() {
        let (pinned, corpora, golden) = crate::parity::locate(root, None, None)?;
        let verified = crate::parity::verify(&golden, &corpora, &pinned);
        if let Some(p) = verified.problems.first() {
            return Err(format!("the golden set does not verify: {p}"));
        }
        for lang in &languages {
            let r = crate::parity::compare(&golden, &corpora, std::slice::from_ref(lang));
            if r.checked == 0 {
                return Err(format!(
                    "language {lang} is declared ported but no golden file is in it"
                ));
            }
            parity.insert(lang.clone(), json!({"mismatches": r.mismatches.len()}));
        }
    }
    let mut bench = Map::new();
    if !packages.is_empty() {
        let (sites, expected) = crate::bench::load_fixtures(root)?;
        let source = crate::bench::RowSource::from_args(&[]);
        for pkg in &packages {
            let meta = sites["packages"].get(pkg.as_str()).ok_or_else(|| {
                format!("package {pkg} is declared ported but not a benchmark package")
            })?;
            let score = crate::bench::score_package(root, pkg, source, meta, &expected)
                .map_err(|e| format!("package {pkg}: {e}"))?;
            let c = |k: &str| score.counts.get(k).copied().unwrap_or(0);
            bench.insert(
                pkg.clone(),
                json!({"correct": c("correct"), "wrong": c("wrong"), "missing": c("missing")}),
            );
        }
    }
    Ok(json!({"parity": parity, "bench": bench}))
}

pub fn main(root: &Path, args: &[String]) -> i32 {
    let flag = |n: &str| args.iter().any(|a| a == n);
    let value = |n: &str| {
        args.iter()
            .position(|a| a == n)
            .and_then(|i| args.get(i + 1))
            .map(PathBuf::from)
    };
    let dir = value("--golden-dir").unwrap_or_else(|| root.join("golden"));

    if !flag("--skip-suite") {
        let ok = Command::new(env!("CARGO"))
            .args(["test", "--workspace", "--quiet"])
            .current_dir(root)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            eprintln!("gate: the test suite failed");
            return 1;
        }
    }

    let load = |name: &str| read(&dir.join(name));
    let (ported, baseline, review) = match (
        load("ported.json"),
        load("baseline.json"),
        load("regression-review.json"),
    ) {
        (Ok(p), Ok(b), Ok(r)) => (p, b, r),
        (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => {
            eprintln!("gate: {e}");
            return 1;
        }
    };
    let current = match value("--measurements") {
        Some(p) => read(&p),
        None => measure(root, &ported),
    };
    let current = match current {
        Ok(c) => c,
        Err(e) => {
            eprintln!("gate: {e}");
            return 1;
        }
    };

    let out = compare(&baseline, &current, &review);
    for g in &out.gains {
        println!("gain      {g}");
    }
    for n in &out.new {
        println!("new       {n}");
    }
    for r in &out.reviewed {
        println!("reviewed  {r}");
    }
    for l in &out.losses {
        println!("LOSS      {l}");
    }
    if !out.losses.is_empty() {
        eprintln!("gate: {} unreviewed loss(es)", out.losses.len());
        if flag("--write-snapshot") {
            eprintln!("gate: refusing to write a snapshot while a comparison fails");
        }
        return 1;
    }
    if flag("--write-snapshot") {
        let text = serde_json::to_string_pretty(&current).unwrap() + "\n";
        if let Err(e) = std::fs::write(dir.join("baseline.json"), text) {
            eprintln!("gate: cannot write baseline: {e}");
            return 1;
        }
        println!("gate: baseline advanced");
    }
    println!("gate: passed");
    0
}
