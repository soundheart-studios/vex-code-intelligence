//! `vex grep`: exact text search over the project (Cortex's `grep` MCP
//! tool, on its ripgrep path).
//!
//! Cortex shells out to `rg` and parses what it prints. vex runs ripgrep's
//! own library crates in-process, configured as that rg command line
//! configures them (smart case, line numbers, the unit-separator field
//! separators, a per-file match cap, context, binary and ignore handling),
//! then parses the printed text exactly as Cortex does: Python's
//! `splitlines`, a cap on printed lines that counts the `--` separators,
//! and lines clipped at 300 characters with a recovery pointer to the chunk
//! that holds them. Files are visited in path order, as `rg --sort path`
//! does; plain `rg` searches in parallel and its file order is not fixed.
//!
//! The wire payload is Cortex's. The CLI adds, per hit, the symbol whose
//! definition encloses the line (`owners`), so results read keyed by
//! owning symbol.
//!
//! When rg itself would fail (a target that does not exist), Cortex falls
//! back to a Python scanner; that fallback's only reachable answer for a
//! missing target, an empty result, is reproduced. A pattern the regex
//! engine rejects is reported as a bad pattern (Cortex would retry it with
//! Python's `re`, a different regex dialect, which is not ported).

use crate::expand::py_resolve;
use crate::freshness;
use crate::pyjson::{Json, repr_str, take_chars};
use crate::recovery;
use crate::symbols;
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{BinaryDetection, SearcherBuilder};
use rusqlite::{Connection, OptionalExtension};
use std::path::Path;
use vex_extract::text::{decode_replace, splitlines};
use vex_store::db::Index;

/// The field separator Cortex asks rg for: it cannot occur in a path or a
/// source line, so a path containing `-` or `:` splits correctly.
pub const SEP: &str = "\x1f";
pub const DEFAULT_MAX_RESULTS: i64 = 40;
const MAX_RESULTS_CAP: i64 = 200;
const MAX_CONTEXT: i64 = 40;

#[derive(Debug, Clone)]
pub struct Request {
    pub pattern: String,
    pub path: String,
    pub literal: bool,
    pub max_results: i64,
    pub context_lines: i64,
}

impl Request {
    pub fn new(pattern: &str) -> Request {
        Request {
            pattern: pattern.to_string(),
            path: String::new(),
            literal: false,
            max_results: DEFAULT_MAX_RESULTS,
            context_lines: 0,
        }
    }
}

/// The payload (or `{"error": ...}`), each hit's owning symbol, and the
/// freshness verdict.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub payload: Json,
    pub owners: Vec<Option<String>>,
    pub freshness: freshness::Verdict,
}

impl Outcome {
    /// Refused (`{"error": ...}`) rather than answered.
    pub fn refused(&self) -> bool {
        self.payload.get("error").is_some()
    }

    /// The CLI's `--json` document (see `annotate`), with a `freshness`
    /// block when the index is behind the working tree.
    pub fn cli_json(&self) -> Json {
        let mut doc = annotate(&self.payload, &self.owners);
        if self.freshness.index_stale {
            doc.set("freshness", self.freshness.to_json());
        }
        doc
    }
}

/// `vex grep` in the registered project containing `start`.
pub fn run(
    home: &Path,
    start: &Path,
    req: &Request,
) -> Result<Outcome, crate::project::ProjectError> {
    let (id, root) = crate::project::registered(home, start)?;
    let idx = Index::open(&vex_store::registry::index_path(home, &id))?;
    let verdict = freshness::check(&idx, &root);
    let payload = grep(&idx.conn, &root, req)?;
    let owners = owners(&idx.conn, &payload)?;
    Ok(Outcome {
        payload,
        owners,
        freshness: verdict,
    })
}

fn error(message: String) -> Json {
    Json::obj(vec![("error", Json::from(message))])
}

/// What the MCP tool answers.
pub fn grep(conn: &Connection, root: &Path, req: &Request) -> rusqlite::Result<Json> {
    let root = py_resolve(root);
    let target = if req.path.is_empty() {
        root.clone()
    } else {
        py_resolve(&root.join(&req.path))
    };
    // A string prefix, as Cortex checks it.
    if !target
        .to_string_lossy()
        .starts_with(&*root.to_string_lossy())
    {
        return Ok(error("path escapes project root".into()));
    }
    let cap = req.max_results.clamp(1, MAX_RESULTS_CAP);
    let context = req.context_lines.min(MAX_CONTEXT);
    let matcher = match RegexMatcherBuilder::new()
        .multi_line(true)
        .unicode(true)
        .octal(false)
        .fixed_strings(req.literal)
        .case_smart(true)
        .line_terminator(Some(b'\n'))
        .dot_matches_new_line(false)
        .ban_byte(Some(b'\x00'))
        .build_many(&[req.pattern.as_str()])
    {
        Ok(m) => m,
        Err(e) => return Ok(error(format!("bad pattern: {e}"))),
    };
    if std::fs::metadata(&target).is_err() {
        // rg exits 2 on a missing path; Cortex's fallback scanner then
        // walks nothing.
        return Ok(payload(&req.pattern, "python", Vec::new(), None));
    }
    let printed = search(&matcher, &target, cap as u64, context.max(0) as usize);
    let limit = cap * (2 * context + 1);
    let text = decode_replace(&printed);
    let lines = splitlines(&text);
    let mut hits = Vec::new();
    for line in head(&lines, limit) {
        if *line == "--" {
            continue;
        }
        let parts: Vec<&str> = line.splitn(3, SEP).collect();
        let [fp, ln, text] = parts[..] else {
            continue;
        };
        hits.push(hit(&rel(fp, &root), ln, text));
    }
    let pointer = grep_recovery(conn, &hits)?;
    Ok(payload(&req.pattern, "rg", hits, pointer))
}

fn payload(pattern: &str, engine: &str, hits: Vec<Hit>, pointer: Option<Json>) -> Json {
    let mut pairs = vec![
        ("pattern", Json::from(pattern)),
        ("engine", Json::from(engine)),
        ("matches", Json::from(hits.len())),
        ("hits", Json::Arr(hits.iter().map(Hit::to_json).collect())),
    ];
    if let Some(p) = pointer {
        pairs.push(("recovery", p));
    }
    Json::obj(pairs)
}

/// `seq[:limit]` with Python's negative-index semantics.
fn head<T>(seq: &[T], limit: i64) -> &[T] {
    let n = seq.len() as i64;
    let end = if limit < 0 {
        (n + limit).max(0)
    } else {
        limit.min(n)
    };
    &seq[..end as usize]
}

/// The rg run, in-process: what `rg --no-heading --line-number
/// --with-filename --smart-case --field-match-separator SEP
/// --field-context-separator SEP --max-count CAP [--context N]
/// [--fixed-strings] --sort path PATTERN TARGET` writes to stdout.
fn search(matcher: &grep_regex::RegexMatcher, target: &Path, cap: u64, context: usize) -> Vec<u8> {
    let separator = (context > 0).then(|| b"--".to_vec());
    let mut printer = grep_printer::StandardBuilder::new()
        .heading(false)
        .path(true)
        .max_matches(Some(cap))
        .per_match_one_line(true)
        .separator_context(Some(b"--".to_vec()))
        .separator_field_match(SEP.as_bytes().to_vec())
        .separator_field_context(SEP.as_bytes().to_vec())
        .separator_search(separator)
        .build_no_color(Vec::new());
    let mut searcher = SearcherBuilder::new()
        .line_terminator(grep_matcher::LineTerminator::byte(b'\n'))
        .line_number(true)
        .before_context(context)
        .after_context(context)
        .build();
    let mut walker = ignore::WalkBuilder::new(target);
    walker
        .hidden(true)
        .parents(true)
        .ignore(true)
        .git_global(true)
        .git_ignore(true)
        .git_exclude(true)
        .require_git(true)
        .follow_links(false)
        .add_custom_ignore_filename(".rgignore")
        .sort_by_file_name(|a, b| a.cmp(b));
    for entry in walker.build().flatten() {
        let is_dir = entry.file_type().is_some_and(|t| t.is_dir())
            || (entry.path_is_symlink() && entry.path().is_dir());
        let explicit = entry.depth() == 0 && !is_dir;
        if !explicit && !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        searcher.set_binary_detection(if explicit {
            BinaryDetection::convert(b'\x00')
        } else {
            BinaryDetection::quit(b'\x00')
        });
        let path = entry.path();
        let _ = searcher.search_path(matcher, path, printer.sink_with_path(matcher, path));
    }
    printer.into_inner().into_inner()
}

/// `str(Path(fp).resolve().relative_to(root))`, else `fp` unchanged.
fn rel(fp: &str, root: &Path) -> String {
    match py_resolve(Path::new(fp)).strip_prefix(root) {
        Ok(p) if p.as_os_str().is_empty() => ".".into(),
        Ok(p) => p.to_string_lossy().into_owned(),
        Err(_) => fp.to_string(),
    }
}

/// One hit, honest about whether its line was clipped.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub file: String,
    pub line: String,
    pub text: String,
    /// The line's length in characters when it was clipped.
    pub full_chars: Option<usize>,
}

impl Hit {
    fn to_json(&self) -> Json {
        let mut pairs = vec![
            ("file", Json::from(&self.file)),
            ("line", Json::from(&self.line)),
            ("text", Json::from(&self.text)),
        ];
        if let Some(n) = self.full_chars {
            pairs.push(("truncated", Json::from(true)));
            pairs.push(("full_chars", Json::from(n)));
        }
        Json::obj(pairs)
    }
}

fn hit(file: &str, line: &str, text: &str) -> Hit {
    let n = text.chars().count();
    Hit {
        file: file.to_string(),
        line: line.to_string(),
        text: take_chars(text, recovery::CAP_GREP_LINE_CHARS).to_string(),
        full_chars: (n > recovery::CAP_GREP_LINE_CHARS).then_some(n),
    }
}

/// A pointer for clipped lines, aimed at the chunk holding the first one
/// (of five) that an indexed chunk covers.
fn grep_recovery(conn: &Connection, hits: &[Hit]) -> rusqlite::Result<Option<Json>> {
    let clipped: Vec<&Hit> = hits.iter().filter(|h| h.full_chars.is_some()).collect();
    if clipped.is_empty() {
        return Ok(None);
    }
    let mut target = None;
    let mut stmt = conn.prepare_cached(
        "SELECT id FROM chunks WHERE file_path = ?1 AND start_line <= ?2 \
         AND end_line >= ?2 LIMIT 1",
    )?;
    for h in clipped.iter().take(5) {
        let Ok(line) = h.line.parse::<i64>() else {
            continue;
        };
        if let Some(id) = stmt
            .query_row(rusqlite::params![h.file, line], |r| r.get::<_, String>(0))
            .optional()?
        {
            target = Some(id);
            break;
        }
    }
    let Some(target) = target else {
        return Ok(None);
    };
    let withheld = clipped
        .iter()
        .map(|h| h.full_chars.unwrap_or(0) - recovery::CAP_GREP_LINE_CHARS)
        .sum();
    Ok(Some(recovery::note(
        "grep_line_cap",
        withheld,
        Json::from(recovery::CAP_GREP_LINE_CHARS),
        vec![format!("expand(target={})", repr_str(&target))],
        Some(Json::obj(vec![
            ("unit", Json::from("characters")),
            ("clipped_lines", Json::from(clipped.len())),
        ])),
    )))
}

/// Per hit of a payload, the qualified name of the innermost symbol whose
/// definition encloses that line (`None` outside any symbol).
pub fn owners(conn: &Connection, payload: &Json) -> rusqlite::Result<Vec<Option<String>>> {
    let Some(Json::Arr(hits)) = payload.get("hits") else {
        return Ok(Vec::new());
    };
    hits.iter()
        .map(|h| {
            let file = h.get("file").and_then(Json::as_str).unwrap_or("");
            let line = h
                .get("line")
                .and_then(Json::as_str)
                .and_then(|l| l.parse::<i64>().ok());
            match line {
                Some(n) => Ok(symbols::at_span(conn, file, n, n)?.map(|s| s.qualified_name)),
                None => Ok(None),
            }
        })
        .collect()
}

/// The CLI's `--json` document for a payload: schema 1, the payload's
/// keys, each hit carrying its owning `symbol`.
pub fn cli_json(conn: &Connection, payload: &Json) -> rusqlite::Result<Json> {
    Ok(annotate(payload, &owners(conn, payload)?))
}

fn annotate(payload: &Json, owners: &[Option<String>]) -> Json {
    let mut pairs = vec![("schema".to_string(), Json::from(crate::SCHEMA as i64))];
    if let Json::Obj(p) = payload {
        for (k, v) in p {
            let v = match (k.as_str(), v) {
                ("hits", Json::Arr(hits)) => Json::Arr(
                    hits.iter()
                        .zip(owners)
                        .map(|(h, owner)| {
                            let mut h = h.clone();
                            h.set("symbol", Json::from(owner.clone()));
                            h
                        })
                        .collect(),
                ),
                _ => v.clone(),
            };
            pairs.push((k.clone(), v));
        }
    }
    Json::Obj(pairs)
}
