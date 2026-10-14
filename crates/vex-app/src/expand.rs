//! `vex expand`: a symbol's or chunk's source (Cortex
//! `application/symbols.py` expand/expand_many/apply_detail and the `expand`
//! MCP tool).
//!
//! A symbol is read from its recorded span in the file, at one of three
//! detail levels; a name with several definition sites lists every site. A
//! chunk id is read from the file too, falling back to the stored (possibly
//! capped) body with a recovery pointer when the file is gone. A list of
//! targets shares one byte budget, measured on the compact UTF-8 JSON the
//! way Cortex measures it, and names what it had to leave out.
//!
//! Lines are what Python's `read_text().splitlines()` yields, which also
//! breaks on form feeds and the other Unicode line separators: a span after
//! one of those reads shifted, exactly as it does in Cortex.

use crate::freshness;
use crate::pyjson::{self, Json, repr_str};
use crate::recovery;
use crate::search::Answer;
use crate::symbols;
use rusqlite::{Connection, OptionalExtension};
use std::path::{Component, Path, PathBuf};
use vex_extract::text::{decode_replace, py_strip, splitlines};
use vex_store::db::Index;

pub const DETAIL_LEVELS: [&str; 3] = ["signature", "minimal", "full"];
/// The MCP tool's default budget for a list.
pub const DEFAULT_MAX_BYTES: i64 = 32_000;
const MAX_SOURCE_BYTES: u64 = 512 * 1024;
/// Declaration terminators: Python/Ruby end a signature with `:`, C-family,
/// Go, Rust, Java and JS open a block with `{`.
const DECL_END: [char; 2] = [':', '{'];
const DOC_OPEN: [&str; 5] = ["\"\"\"", "'''", "/**", "///", "//!"];
/// A signature longer than this means the heuristic lost the boundary.
const MAX_SIGNATURE_LINES: usize = 12;

/// A refused target: Cortex's `CortexError` code and message.
#[derive(Debug, Clone, PartialEq)]
pub struct Refusal {
    pub code: &'static str,
    pub message: String,
}

impl Refusal {
    pub(crate) fn new(code: &'static str, message: String) -> Refusal {
        Refusal { code, message }
    }
}

/// An expand request: one target, or a list (`list` set) of them.
#[derive(Debug, Clone)]
pub struct Request {
    pub targets: Vec<String>,
    pub list: bool,
    pub detail: String,
    pub max_bytes: i64,
}

impl Request {
    pub fn single(target: &str) -> Request {
        Request {
            targets: vec![target.to_string()],
            list: false,
            detail: "full".into(),
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

/// The answer plus the freshness verdict (for the CLI's exit code).
#[derive(Debug, Clone)]
pub struct Outcome {
    pub answer: Answer,
    pub freshness: freshness::Verdict,
}

impl Outcome {
    /// The CLI's `--json` document: the answer's (schema first), with a
    /// `freshness` block when the index is behind the working tree.
    pub fn cli_json(&self) -> Json {
        let mut doc = self.answer.cli_json();
        if self.freshness.index_stale {
            doc.set("freshness", self.freshness.to_json());
        }
        doc
    }
}

/// `vex expand` in the registered project containing `start`.
pub fn run(
    home: &Path,
    start: &Path,
    req: &Request,
) -> Result<Outcome, crate::project::ProjectError> {
    let (id, root) = crate::project::registered(home, start)?;
    let idx = Index::open(&vex_store::registry::index_path(home, &id))?;
    let verdict = freshness::check(&idx, &root);
    let answer = answer(&idx, &root, req)?;
    Ok(Outcome {
        answer,
        freshness: verdict,
    })
}

/// What the MCP tool answers for a request.
pub fn answer(idx: &Index, root: &Path, req: &Request) -> rusqlite::Result<Answer> {
    let result = if req.list {
        expand_many(idx, root, &req.targets, req.max_bytes, &req.detail)?
    } else {
        let target = req.targets.first().map(String::as_str).unwrap_or("");
        expand(&idx.conn, root, target, &req.detail)?
    };
    Ok(match result {
        Ok(payload) => Answer::Payload(payload),
        Err(r) => Answer::Error {
            code: r.code.to_string(),
            message: r.message,
        },
    })
}

pub(crate) fn detail_refusal(detail: &str) -> Option<Refusal> {
    if DETAIL_LEVELS.contains(&detail) {
        return None;
    }
    Some(Refusal::new(
        "invalid_request",
        format!(
            "'detail' must be one of ('signature', 'minimal', 'full'), got {}",
            repr_str(detail)
        ),
    ))
}

/// `Path.resolve()` (non-strict): symlinks followed where the path exists,
/// `..` folded lexically where it does not.
pub(crate) fn py_resolve(path: &Path) -> PathBuf {
    if let Ok(p) = path.canonicalize() {
        return p;
    }
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    // The longest existing ancestor, canonicalized, plus the rest.
    let mut rest = Vec::new();
    let mut base = out.as_path();
    while let Some(parent) = base.parent() {
        rest.push(base.file_name().unwrap_or_default().to_owned());
        if let Ok(p) = parent.canonicalize() {
            return rest.iter().rev().fold(p, |acc, part| acc.join(part));
        }
        base = parent;
    }
    out
}

/// A repo file's lines, only if its canonical path stays inside the root.
/// Unreadable files read as no lines; escapes, symlinks and oversized
/// files are refused.
pub(crate) fn contained_read(root: &Path, rel_path: &str) -> Result<Vec<String>, Refusal> {
    let target = py_resolve(&root.join(rel_path));
    let root_resolved = py_resolve(root);
    if !target.starts_with(&root_resolved) {
        return Err(Refusal::new(
            "forbidden_path",
            format!("path escapes project root: {rel_path}"),
        ));
    }
    if target
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err(Refusal::new(
            "forbidden_path",
            format!("symlinked source refused: {rel_path}"),
        ));
    }
    let Ok(meta) = std::fs::metadata(&target) else {
        return Ok(Vec::new());
    };
    if meta.len() > MAX_SOURCE_BYTES {
        return Err(Refusal::new(
            "forbidden_path",
            format!("source too large: {rel_path}"),
        ));
    }
    let Ok(bytes) = std::fs::read(&target) else {
        return Ok(Vec::new());
    };
    Ok(splitlines(&decode_replace(&bytes))
        .into_iter()
        .map(str::to_string)
        .collect())
}

/// `lines[start - 1:end]` with Python's slice clamping (and negative
/// indexes counted from the end).
fn span(lines: &[String], start: i64, end: i64) -> &[String] {
    let n = lines.len() as i64;
    let clamp = |i: i64| -> usize {
        let i = if i < 0 { (i + n).max(0) } else { i.min(n) };
        i as usize
    };
    let (a, b) = (clamp(start - 1), clamp(end));
    if a >= b { &[] } else { &lines[a..b] }
}

/// Trim a symbol's source to a detail level: `(source, exact)`, where
/// `exact` is false when no declaration boundary was found and the whole
/// body came back instead (the ladder fails open).
pub fn apply_detail(lines: &[String], detail: &str) -> (String, bool) {
    if detail == "full" || lines.is_empty() {
        return (lines.join("\n"), true);
    }
    let Some(end) = lines.iter().take(MAX_SIGNATURE_LINES).position(|l| {
        l.trim_end_matches(vex_extract::text::is_py_whitespace)
            .ends_with(DECL_END)
    }) else {
        return (lines.join("\n"), false);
    };
    if detail == "signature" {
        return (lines[..=end].join("\n"), true);
    }
    // "minimal": the signature plus a leading doc comment, when present.
    let rest = &lines[end + 1..];
    let mut doc_end = 0;
    let mut opener = "";
    for (index, line) in rest.iter().enumerate() {
        let stripped = py_strip(line);
        let triple = opener == "\"\"\"" || opener == "'''";
        if index == 0 {
            let Some(found) = DOC_OPEN.iter().find(|d| stripped.starts_with(**d)) else {
                break;
            };
            opener = found;
            let triple = opener == "\"\"\"" || opener == "'''";
            if triple && stripped.matches(opener).count() >= 2 {
                doc_end = 1;
                break;
            }
            if !triple {
                doc_end = 1;
            }
            continue;
        }
        if triple {
            if stripped.contains(opener) {
                doc_end = index + 1;
                break;
            }
        } else if ["*", "///", "//!", "//"]
            .iter()
            .any(|p| stripped.starts_with(p))
        {
            doc_end = index + 1;
        } else {
            break;
        }
    }
    let mut kept: Vec<&str> = lines[..=end].iter().map(String::as_str).collect();
    kept.extend(rest[..doc_end].iter().map(String::as_str));
    (kept.join("\n"), true)
}

struct ChunkRow {
    id: String,
    file_path: String,
    start_line: i64,
    end_line: i64,
    content: String,
    full_chars: i64,
}

fn chunk_by_id(conn: &Connection, id: &str) -> rusqlite::Result<Option<ChunkRow>> {
    conn.prepare_cached(
        "SELECT id, file_path, start_line, end_line, content, full_chars \
         FROM chunks WHERE id IN (?1)",
    )?
    .query_row([id], |r| {
        Ok(ChunkRow {
            id: r.get(0)?,
            file_path: r.get(1)?,
            start_line: r.get(2)?,
            end_line: r.get(3)?,
            content: r.get(4)?,
            full_chars: r.get(5)?,
        })
    })
    .optional()
}

/// One target's payload, or why it was refused.
pub fn expand(
    conn: &Connection,
    root: &Path,
    target: &str,
    detail: &str,
) -> rusqlite::Result<Result<Json, Refusal>> {
    if let Some(r) = detail_refusal(detail) {
        return Ok(Err(r));
    }
    if let Some(s) = symbols::resolve(conn, target)?.into_iter().next() {
        let lines = match contained_read(root, &s.file_path) {
            Ok(lines) => lines,
            Err(r) => return Ok(Err(r)),
        };
        let (source, _exact) = apply_detail(span(&lines, s.start_line, s.end_line), detail);
        let sites: Vec<Json> = conn
            .prepare_cached(
                "SELECT start_line, end_line, declaration FROM symbol_sites \
                 WHERE qualified_name = ?1 AND file_path = ?2 \
                 ORDER BY declaration, start_line",
            )?
            .query_map([&s.qualified_name, &s.file_path], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|(a, b, decl)| {
                Json::obj(vec![
                    ("lines", Json::from(format!("{a}-{b}"))),
                    ("declaration", Json::from(decl != 0)),
                    (
                        "source",
                        Json::from(apply_detail(span(&lines, a, b), detail).0),
                    ),
                ])
            })
            .collect();
        let mut payload = vec![
            ("symbol", Json::from(&s.qualified_name)),
            ("file", Json::from(&s.file_path)),
            (
                "lines",
                Json::from(format!("{}-{}", s.start_line, s.end_line)),
            ),
            ("source", Json::from(source)),
        ];
        if sites.len() > 1 {
            payload.push(("sites", Json::Arr(sites)));
        }
        return Ok(Ok(Json::obj(payload)));
    }
    let Some(ch) = chunk_by_id(conn, target)? else {
        return Ok(Err(Refusal::new(
            "not_found",
            format!("'{target}' is neither a known symbol nor a chunk id. Try search() first."),
        )));
    };
    // The file's span, not the stored body: that is capped at index time.
    let from_file = contained_read(root, &ch.file_path).ok().and_then(|lines| {
        let s = span(&lines, ch.start_line, ch.end_line);
        (!s.is_empty()).then(|| s.join("\n"))
    });
    let stored = ch.content.chars().count() as i64;
    let mut payload = vec![
        ("chunk", Json::from(&ch.id)),
        ("file", Json::from(&ch.file_path)),
        (
            "lines",
            Json::from(format!("{}-{}", ch.start_line, ch.end_line)),
        ),
    ];
    match from_file {
        Some(body) => payload.push(("source", Json::from(body))),
        None => {
            payload.push(("source", Json::from(&ch.content)));
            if ch.full_chars > stored {
                payload.push(("truncated", Json::from(true)));
                payload.push((
                    "recovery",
                    recovery::note(
                        "chunk_body_cap",
                        (ch.full_chars - stored) as usize,
                        Json::from(recovery::CAP_CHUNK_CHARS),
                        vec![format!(
                            "grep(pattern=<a literal from this span>, path={})",
                            repr_str(&ch.file_path)
                        )],
                        Some(Json::obj(vec![
                            ("unit", Json::from("characters")),
                            (
                                "reason_detail",
                                Json::from(
                                    "the file could not be read from disk, so only \
                                     the capped index copy was available",
                                ),
                            ),
                        ])),
                    ),
                ));
            }
        }
    }
    Ok(Ok(Json::obj(payload)))
}

/// `len(json.dumps(v, ensure_ascii=False, separators=(",", ":")).encode())`.
fn compact_bytes(v: &Json) -> usize {
    pyjson::dumps(v, pyjson::COMPACT_RAW).len()
}

fn repr_list(items: &[String]) -> String {
    let parts: Vec<String> = items.iter().map(|s| repr_str(s)).collect();
    format!("[{}]", parts.join(", "))
}

/// Several targets under one shared byte budget.
pub fn expand_many(
    idx: &Index,
    root: &Path,
    targets: &[String],
    max_bytes: i64,
    detail: &str,
) -> rusqlite::Result<Result<Json, Refusal>> {
    let mut cleaned: Vec<String> = Vec::new();
    for t in targets {
        let t = py_strip(t);
        if !t.is_empty() && !cleaned.iter().any(|c| c == t) {
            cleaned.push(t.to_string());
        }
    }
    if cleaned.is_empty() {
        return Ok(Err(Refusal::new(
            "invalid_request",
            "'targets' must contain a non-empty evidence target".into(),
        )));
    }
    if cleaned.len() > recovery::CAP_BATCH_TARGETS {
        return Ok(Err(Refusal::new(
            "invalid_request",
            format!(
                "'targets' supports at most {} evidence targets",
                recovery::CAP_BATCH_TARGETS
            ),
        )));
    }
    if let Some(r) = detail_refusal(detail) {
        return Ok(Err(r));
    }
    let budget = max_bytes.clamp(
        recovery::CAP_BATCH_BYTES_MIN as i64,
        recovery::CAP_BATCH_BYTES_MAX as i64,
    );
    let index_version: String = idx
        .conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'index_version'",
            [],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or_default();
    let mut sources: Vec<Json> = Vec::new();
    let mut errors: Vec<Json> = Vec::new();
    let mut omitted: Vec<String> = Vec::new();
    let build = |sources: Vec<Json>, errors: Vec<Json>, truncated: bool, included: usize| {
        Json::obj(vec![
            ("sources", Json::Arr(sources)),
            ("errors", Json::Arr(errors)),
            ("truncated", Json::from(truncated)),
            (
                "limits",
                Json::obj(vec![
                    ("targets", Json::from(recovery::CAP_BATCH_TARGETS)),
                    ("max_bytes", Json::from(budget)),
                    ("detail", Json::from(detail)),
                ]),
            ),
            (
                "stats",
                Json::obj(vec![
                    ("requested", Json::from(cleaned.len())),
                    ("included", Json::from(included)),
                    ("response_bytes", Json::from(0i64)),
                ]),
            ),
            ("index_version", Json::from(index_version.as_str())),
        ])
    };
    for target in &cleaned {
        let item = match expand(&idx.conn, root, target, detail)? {
            Ok(item) => item,
            Err(r) => {
                errors.push(error_entry(target, r.code, &r.message));
                continue;
            }
        };
        let mut trial_sources = sources.clone();
        trial_sources.push(item.clone());
        let trial = build(trial_sources, errors.clone(), !omitted.is_empty(), 0);
        if compact_bytes(&trial) as i64 > budget - 32 {
            omitted.push(target.clone());
            errors.push(error_entry(
                target,
                "response_budget",
                "evidence omitted because the shared byte budget was exhausted",
            ));
            continue;
        }
        sources.push(item);
    }
    let included = sources.len();
    let mut payload = build(sources, errors, !omitted.is_empty(), included);
    if !omitted.is_empty() {
        payload.set("recovery", budget_note(&omitted, budget, detail));
    }
    let size = compact_bytes(&payload);
    if let Some(stats) = payload_stats(&mut payload) {
        stats.set("response_bytes", Json::from(size));
    }
    Ok(Ok(payload))
}

fn payload_stats(payload: &mut Json) -> Option<&mut Json> {
    match payload {
        Json::Obj(pairs) => pairs.iter_mut().find(|(k, _)| k == "stats").map(|(_, v)| v),
        _ => None,
    }
}

fn error_entry(target: &str, code: &str, message: &str) -> Json {
    Json::obj(vec![
        ("target", Json::from(target)),
        ("error", Json::from(code)),
        ("message", Json::from(message)),
    ])
}

/// `recovery.budget_note`, with the cheaper-detail retry offered first.
fn budget_note(omitted: &[String], budget: i64, detail: &str) -> Json {
    let mut next = vec![
        format!(
            "expand_many(targets={})  # fetch the remainder",
            repr_list(&omitted[..omitted.len().min(6)])
        ),
        format!(
            "expand_many(targets=[...], max_bytes={})  # or raise the budget",
            (budget * 2).min(recovery::CAP_BATCH_BYTES_MAX as i64)
        ),
    ];
    let cheaper = match detail {
        "full" => Some("minimal"),
        "minimal" => Some("signature"),
        _ => None,
    };
    if let Some(cheaper) = cheaper {
        next.insert(
            0,
            format!(
                "expand_many(targets=[...same...], detail='{cheaper}')  # same budget, less per symbol"
            ),
        );
    }
    let mut note = recovery::note(
        "response_budget",
        omitted.len(),
        Json::from(budget),
        next,
        Some(Json::obj(vec![(
            "omitted_targets",
            Json::from(omitted.to_vec()),
        )])),
    );
    if let Some(cheaper) = cheaper {
        note.set(
            "hint",
            Json::from(format!(
                "{} target(s) omitted at {budget} bytes. Retry the whole batch with \
                 detail='{cheaper}' before splitting it up or raising the budget.",
                omitted.len()
            )),
        );
    }
    note
}
