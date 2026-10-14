//! `vex changes`: changed symbols, their upstream blast radius and, against
//! a base, the shape of each change (Cortex's changes.py and
//! application/changes.py, without the savings ledger).
//!
//! Cortex shells out to git and reads its output as text; vex runs the same
//! git commands and reads them the same way: Python `splitlines`, universal
//! newlines for file contents, `str.split()` whitespace for construct text.
//! Cortex walks the hit symbols of a file as a Python set, so the caller it
//! reports `via` and the order of equally ranked callers follow the
//! interpreter's hash seed; vex walks them in index row order, which is one
//! of the orders Cortex can produce.
//!
//! `--staged` diffs git's index against HEAD. Its symbols and graph come
//! from a scratch index of the staged tree (exported with checkout-index),
//! never from the project's index, so the answer equals Cortex's
//! detect_changes on a checkout holding only the staged changes.

use crate::freshness;
use crate::pyjson::{self, Json, py_split, repr_str};
use indexmap::IndexMap;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::process::Command;
use vex_extract::language::{HEAD_BYTES, language_of};
use vex_extract::text::{py_strip, splitlines};
use vex_store::db::Index;

pub const DEFAULT_MAX_DEPTH: i64 = 2;
const AFFECTED_CAP: usize = 30;
const HIGH_CONFIDENCE: f64 = 0.7;
const NO_CHANGES: &str = "no uncommitted changes detected (or not a git repo)";
const NOTE: &str = "affected = upstream callers of changed symbols; verify confidence < 0.7 hits before assuming breakage";
/// Constructs whose origin is worth attributing: they change whether the
/// code below them runs.
const ORIGIN_CONSTRUCTS: [&str; 4] = [
    "if_statement",
    "return_statement",
    "throw_statement",
    "switch_statement",
];

/// A changes request: the working tree against git's index (the default),
/// against a base ref, or git's index against HEAD (`staged`).
#[derive(Debug, Clone)]
pub struct Request {
    pub base: String,
    pub staged: bool,
    pub max_depth: i64,
}

/// A changes report.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// Cortex's detect_changes payload.
    pub result: Json,
    /// The project index's freshness; None for `--staged`, which reads a
    /// scratch index of the staged tree instead.
    pub freshness: Option<freshness::Verdict>,
}

impl Outcome {
    /// The wire text, as Cortex's detect_changes tool returns it.
    pub fn wire(&self) -> String {
        pyjson::dumps(&self.result, pyjson::DEFAULT)
    }

    /// The CLI's `--json` document: schema 1, the payload's keys, and a
    /// `freshness` block when stale.
    pub fn cli_json(&self) -> Json {
        let mut pairs = vec![("schema".to_string(), Json::from(crate::SCHEMA as i64))];
        if let Json::Obj(p) = &self.result {
            pairs.extend(p.iter().cloned());
        }
        let mut doc = Json::Obj(pairs);
        if let Some(f) = self.freshness.as_ref().filter(|f| f.index_stale) {
            doc.set("freshness", f.to_json());
        }
        doc
    }
}

/// `vex changes` in the registered project containing `start`.
pub fn run(
    home: &Path,
    start: &Path,
    req: &Request,
) -> Result<Outcome, crate::project::ProjectError> {
    let (id, root) = crate::project::registered(home, start)?;
    if req.staged {
        return Ok(Outcome {
            result: staged(&root, req.max_depth)?,
            freshness: None,
        });
    }
    let idx = Index::open(&vex_store::registry::index_path(home, &id))?;
    let verdict = freshness::check(&idx, &root);
    Ok(Outcome {
        result: detect(&idx.conn, &root, &req.base, req.max_depth)?,
        freshness: Some(verdict),
    })
}

/// Cortex `changes_svc.detect`: the working tree (or `base`) against the
/// index in `conn`, with shapes and construct origins when a base is given.
pub fn detect(
    conn: &Connection,
    root: &Path,
    base: &str,
    max_depth: i64,
) -> rusqlite::Result<Json> {
    let mut args = vec!["diff", "-U0", "--no-color"];
    if !base.is_empty() {
        args.push(base);
    }
    let mut result = detect_changes(conn, &changed_lines(root, &args), max_depth)?;
    if !base.is_empty() {
        let (shapes, abstention) = symbol_shapes(root, base);
        result.set("symbol_shapes", Json::Arr(shapes));
        result.set("constructs", Json::Arr(construct_origins(root, base)));
        if let Some(reason) = abstention {
            result.set("shape_abstention", Json::from(reason));
        }
    }
    Ok(result)
}

/// The staged report: git's index against HEAD, over a scratch index of
/// the staged tree.
pub fn staged(root: &Path, max_depth: i64) -> rusqlite::Result<Json> {
    let scratch = Scratch::new();
    let tree = scratch.0.join("tree");
    std::fs::create_dir_all(&tree).map_err(io_error)?;
    let prefix = format!("{}/", tree.display());
    let exported = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["checkout-index", "-a", "-f"])
        .arg(format!("--prefix={prefix}"))
        .output()
        .map_err(io_error)?;
    if !exported.status.success() {
        // Not a git repo (or no index): Cortex's diff would come back empty.
        return Ok(Json::obj(vec![("summary", Json::from(NO_CHANGES))]));
    }
    let idx = Index::open(&scratch.0.join("index.db"))?;
    crate::index::run(&idx, &tree, false)?;
    let changed = changed_lines(root, &["diff", "-U0", "--no-color", "--cached"]);
    detect_changes(&idx.conn, &changed, max_depth)
}

fn io_error(e: std::io::Error) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(e))
}

/// A temporary directory, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        Scratch(std::env::temp_dir().join(format!("vex-staged-{}-{nanos}", std::process::id())))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `git -C root <args>`: its exit status and stdout as text.
fn git(root: &Path, args: &[&str]) -> Option<(bool, String)> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    Some((
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    ))
}

/// Text as Python's universal-newlines mode reads it.
fn universal_newlines(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

/// Cortex `_changed_lines`: {path: changed line numbers in the new
/// version}, from hunk headers only. A `+++` line naming no `b/` path (a
/// deletion, a quoted path) leaves the previous file current, as Cortex's
/// does.
fn changed_lines(root: &Path, args: &[&str]) -> IndexMap<String, Vec<i64>> {
    let out = git(root, args).map(|(_, out)| out).unwrap_or_default();
    let mut changes: IndexMap<String, Vec<i64>> = IndexMap::new();
    let mut current: Option<String> = None;
    for line in splitlines(&out) {
        if let Some(path) = line.strip_prefix("+++ b/").filter(|p| !p.is_empty()) {
            current = Some(path.to_string());
            continue;
        }
        if let (Some((start, count)), Some(file)) = (hunk(line), &current) {
            changes
                .entry(file.clone())
                .or_default()
                .extend(start..start + count.max(1));
        }
    }
    changes
}

/// `^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@`: the new start and count
/// (a missing count is 1). Public so its edge cases can be checked against
/// Cortex's regex directly; git itself only prints well-formed headers.
pub fn hunk(line: &str) -> Option<(i64, i64)> {
    fn digits(s: &str) -> (&str, &str) {
        let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
        s.split_at(end)
    }
    fn number_pair(s: &str) -> Option<(&str, Option<&str>, &str)> {
        let (first, rest) = digits(s);
        if first.is_empty() {
            return None;
        }
        match rest.strip_prefix(',') {
            // A comma without digits: `(?:,\d+)?` backs off and the comma
            // then matches nothing that may follow, so the line fails.
            Some(rest) => {
                let (second, rest) = digits(rest);
                (!second.is_empty()).then_some((first, Some(second), rest))
            }
            None => Some((first, None, rest)),
        }
    }
    let rest = line.strip_prefix("@@ -")?;
    let (_, _, rest) = number_pair(rest)?;
    let rest = rest.strip_prefix(" +")?;
    let (start, count, rest) = number_pair(rest)?;
    rest.starts_with(" @@").then_some(())?;
    Some((
        start.parse().ok()?,
        count.map_or(Some(1), |c| c.parse().ok())?,
    ))
}

/// Cortex `detect_changes`: the symbols each changed line falls in and the
/// upstream callers of those symbols, with a risk level.
fn detect_changes(
    conn: &Connection,
    changed: &IndexMap<String, Vec<i64>>,
    max_depth: i64,
) -> rusqlite::Result<Json> {
    if changed.is_empty() {
        return Ok(Json::obj(vec![("summary", Json::from(NO_CHANGES))]));
    }
    let mut changed_symbols = Vec::new();
    let mut affected: IndexMap<String, (String, i64, f64)> = IndexMap::new();
    let mut stmt = conn.prepare(
        "SELECT qualified_name, name, kind, start_line, end_line FROM symbols WHERE file_path = ?",
    )?;
    for (fp, lines) in changed {
        let rows: Vec<(String, String, i64, i64)> = stmt
            .query_map([fp], |r| Ok((r.get(0)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut hits = Vec::new();
        for (qn, kind, start, end) in rows {
            if lines.iter().any(|&ln| start <= ln && ln <= end) {
                changed_symbols.push(Json::obj(vec![
                    ("symbol", Json::from(qn.as_str())),
                    ("kind", Json::from(kind)),
                    ("file", Json::from(fp.as_str())),
                ]));
                hits.push(qn);
            }
        }
        for qn in &hits {
            for (depth, hit) in crate::graph::bfs(conn, qn, "upstream", max_depth)? {
                let symbol = hit
                    .get("symbol")
                    .and_then(Json::as_str)
                    .unwrap_or("")
                    .to_string();
                let confidence = match hit.get("confidence") {
                    Some(Json::Float(c)) => *c,
                    _ => 0.0,
                };
                let replace = affected.get(&symbol).is_none_or(|prev| confidence > prev.2);
                if replace {
                    affected.insert(symbol, (qn.clone(), depth, confidence));
                }
            }
        }
    }
    let high = affected.values().filter(|a| a.2 >= HIGH_CONFIDENCE).count();
    let risk = if affected.is_empty() {
        "low"
    } else if high <= 3 {
        "medium"
    } else {
        "high"
    };
    let mut ranked: Vec<(&String, &(String, i64, f64))> = affected.iter().collect();
    ranked.sort_by(|a, b| {
        (-a.1.2)
            .partial_cmp(&-b.1.2)
            .unwrap()
            .then(a.1.1.cmp(&b.1.1))
    });
    let affected_json = ranked
        .into_iter()
        .take(AFFECTED_CAP)
        .map(|(symbol, (via, depth, confidence))| {
            Json::obj(vec![
                ("symbol", Json::from(symbol.as_str())),
                ("via", Json::from(via.as_str())),
                ("depth", Json::from(*depth)),
                ("confidence", Json::from(*confidence)),
            ])
        })
        .collect();
    Ok(Json::obj(vec![
        (
            "summary",
            Json::obj(vec![
                ("changed_files", Json::from(changed.len() as i64)),
                ("changed_symbols", Json::from(changed_symbols.len() as i64)),
                ("affected_symbols", Json::from(affected.len() as i64)),
                ("risk_level", Json::from(risk)),
            ]),
        ),
        ("changed", Json::Arr(changed_symbols)),
        ("affected", Json::Arr(affected_json)),
        ("note", Json::from(NOTE)),
    ]))
}

fn base_exists(root: &Path, base: &str) -> bool {
    !base.is_empty() && git(root, &["rev-parse", "--verify", base]).is_some_and(|(ok, _)| ok)
}

/// Cortex `_changed_files`: `git diff --name-only base`, blank lines dropped.
fn changed_files(root: &Path, base: &str) -> Vec<String> {
    let out = git(root, &["diff", "--name-only", base])
        .map(|(_, out)| out)
        .unwrap_or_default();
    splitlines(&out)
        .into_iter()
        .filter(|l| !py_strip(l).is_empty())
        .map(str::to_string)
        .collect()
}

/// A file as of `base`, or None when it did not exist there.
fn blob(root: &Path, base: &str, rel: &str) -> Option<String> {
    match git(root, &["show", &format!("{base}:{rel}")]) {
        Some((true, out)) => Some(universal_newlines(&out)),
        _ => None,
    }
}

/// The file as it is now (`Path.read_text`), or None when it cannot be read.
fn current(root: &Path, rel: &str) -> Option<String> {
    let bytes = std::fs::read(root.join(rel)).ok()?;
    Some(universal_newlines(&String::from_utf8_lossy(&bytes)))
}

/// `Path(rel).name`.
fn base_name(rel: &str) -> &str {
    rel.trim_end_matches('/').rsplit('/').next().unwrap_or(rel)
}

#[derive(Debug, Clone, PartialEq)]
struct Site {
    signature: String,
    body: String,
    start: i64,
    end: i64,
    kind: String,
}

/// `lines[a:b]` with Python's slice clamping.
fn py_slice<'a>(lines: &'a [&'a str], a: i64, b: i64) -> &'a [&'a str] {
    let n = lines.len() as i64;
    let clamp = |i: i64| if i < 0 { (i + n).max(0) } else { i.min(n) } as usize;
    let (a, b) = (clamp(a), clamp(b));
    if a < b { &lines[a..b] } else { &[] }
}

/// Cortex `_symbols_of_source`: {name: [site, ...]} from parsing `source`
/// as a file named like `rel`.
fn symbols_of_source(source: &str, rel: &str) -> IndexMap<String, Vec<Site>> {
    let name = base_name(rel);
    let bytes = source.as_bytes();
    let lang = language_of(name, Some(&bytes[..bytes.len().min(HEAD_BYTES)]));
    let parsed = vex_extract::engine::chunk_bytes(name, lang, bytes);
    let lines = splitlines(source);
    let n = lines.len() as i64;
    let mut out: IndexMap<String, Vec<Site>> = IndexMap::new();
    for sym in parsed.symbols {
        let (start, end) = (sym.start_line as i64, sym.end_line as i64);
        let signature = if start <= n {
            let i = if start - 1 < 0 {
                start - 1 + n
            } else {
                start - 1
            };
            lines.get(i as usize).copied().unwrap_or("").to_string()
        } else {
            String::new()
        };
        out.entry(sym.name).or_default().push(Site {
            signature,
            body: py_slice(&lines, start - 1, end).join("\n"),
            start,
            end,
            kind: sym.kind,
        });
    }
    out
}

/// Cortex `_pair_sites`: identical body first, then identical signature,
/// then by order.
fn pair_sites(before: &[Site], after: &[Site]) -> Vec<(Option<Site>, Option<Site>)> {
    let (mut before, mut after) = (before.to_vec(), after.to_vec());
    let mut pairs = Vec::new();
    let keys: [fn(&Site) -> &str; 2] = [|s| &s.body, |s| &s.signature];
    for key in keys {
        for b in before.clone() {
            if let Some(m) = after.iter().position(|a| key(a) == key(&b)) {
                let a = after.remove(m);
                if let Some(i) = before.iter().position(|x| *x == b) {
                    before.remove(i);
                }
                pairs.push((Some(b), Some(a)));
            }
        }
    }
    let common = before.len().min(after.len());
    for (b, a) in before.iter().zip(&after) {
        pairs.push((Some(b.clone()), Some(a.clone())));
    }
    for b in &before[common..] {
        pairs.push((Some(b.clone()), None));
    }
    for a in &after[common..] {
        pairs.push((None, Some(a.clone())));
    }
    pairs
}

/// Cortex `symbol_shapes`: added, removed, moved, body-edited or
/// signature-edited per changed symbol, or the abstention reason.
fn symbol_shapes(root: &Path, base: &str) -> (Vec<Json>, Option<String>) {
    if !base_exists(root, base) {
        return (
            Vec::new(),
            Some(format!(
                "base {} cannot be resolved, so there is nothing to compare against; \
                 shape is not guessed from one side",
                repr_str(base)
            )),
        );
    }
    let mut shapes = Vec::new();
    for rel in changed_files(root, base) {
        let before_src = blob(root, base, &rel);
        let after_src = current(root, &rel);
        if before_src.is_none() && after_src.is_none() {
            continue;
        }
        let before = before_src
            .map(|s| symbols_of_source(&s, &rel))
            .unwrap_or_default();
        let after = after_src
            .map(|s| symbols_of_source(&s, &rel))
            .unwrap_or_default();
        let mut names: Vec<&String> = before.keys().chain(after.keys()).collect();
        names.sort();
        names.dedup();
        for name in names {
            let none = Vec::new();
            let pairs = pair_sites(
                before.get(name).unwrap_or(&none),
                after.get(name).unwrap_or(&none),
            );
            for (b, a) in pairs {
                let entry = |shape: &str, rest: Vec<(&str, Json)>| {
                    let mut pairs = vec![
                        ("name", Json::from(name.as_str())),
                        ("file", Json::from(rel.as_str())),
                        ("shape", Json::from(shape)),
                    ];
                    pairs.extend(rest);
                    Json::obj(pairs)
                };
                let edited = |shape: &str, b: &Site, a: &Site| {
                    entry(
                        shape,
                        vec![
                            ("from_line", Json::from(b.start)),
                            ("to_line", Json::from(a.start)),
                            ("kind", Json::from(a.kind.as_str())),
                        ],
                    )
                };
                match (b, a) {
                    (None, Some(a)) => shapes.push(entry(
                        "added",
                        vec![
                            ("to_line", Json::from(a.start)),
                            ("kind", Json::from(a.kind.as_str())),
                        ],
                    )),
                    (Some(b), None) => shapes.push(entry(
                        "removed",
                        vec![
                            ("from_line", Json::from(b.start)),
                            ("kind", Json::from(b.kind.as_str())),
                        ],
                    )),
                    (Some(b), Some(a)) if b.body == a.body => {
                        if b.start != a.start {
                            shapes.push(edited("moved", &b, &a));
                        }
                    }
                    (Some(b), Some(a)) if b.signature != a.signature => {
                        shapes.push(edited("signature-edited", &b, &a));
                    }
                    (Some(b), Some(a)) => shapes.push(edited("body-edited", &b, &a)),
                    (None, None) => {}
                }
            }
        }
    }
    (shapes, None)
}

/// Cortex `_constructs_of_source`: every origin construct, depth first,
/// with its whitespace-normalised text.
fn constructs_of_source(source: &str, rel: &str) -> Vec<(String, i64, String)> {
    let name = base_name(rel);
    let bytes = source.as_bytes();
    let lang = language_of(name, Some(&bytes[..bytes.len().min(HEAD_BYTES)]));
    let Some(tree) = vex_extract::engine::parse_tree(lang, bytes) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        if ORIGIN_CONSTRUCTS.contains(&n.kind()) {
            let text = String::from_utf8_lossy(&bytes[n.start_byte()..n.end_byte()]);
            found.push((
                n.kind().to_string(),
                n.start_position().row as i64 + 1,
                py_split(&text).collect::<Vec<_>>().join(" "),
            ));
        }
        let mut cursor = n.walk();
        stack.extend(n.named_children(&mut cursor));
    }
    found
}

/// Cortex `construct_origins`: whether the change introduced each
/// construct or it was already in the base (matched on text, not line).
fn construct_origins(root: &Path, base: &str) -> Vec<Json> {
    if !base_exists(root, base) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for rel in changed_files(root, base) {
        let before_src = blob(root, base, &rel);
        let Some(after_src) = current(root, &rel) else {
            continue;
        };
        let before_texts: Vec<String> = before_src
            .map(|s| {
                constructs_of_source(&s, &rel)
                    .into_iter()
                    .map(|c| c.2)
                    .collect()
            })
            .unwrap_or_default();
        for (kind, line, text) in constructs_of_source(&after_src, &rel) {
            let origin = if before_texts.contains(&text) {
                "pre-existing"
            } else {
                "introduced"
            };
            out.push(Json::obj(vec![
                ("type", Json::from(kind)),
                ("line", Json::from(line)),
                ("text", Json::from(text)),
                ("file", Json::from(rel.as_str())),
                ("origin", Json::from(origin)),
            ]));
        }
    }
    out
}
