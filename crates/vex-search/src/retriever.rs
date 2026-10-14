//! Hybrid retrieval (Cortex `rank/retriever.py`): semantic and keyword
//! channels merged by RRF, re-fused with definition and source preferences,
//! then diversity-capped per file and per symbol with backfill, so the caps
//! shape a slate but never shrink it, and what they held back is reported.
//!
//! The semantic channel is supplied by the caller (vector search lives with
//! the embedder); an empty one gives Cortex's keyword-only retrieval.

use crate::fusion::fuse;
use crate::paths::is_non_source;
use indexmap::IndexMap;
use regex::Regex;
use rusqlite::Connection;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
use vex_extract::text::{is_py_whitespace, py_strip};

pub const MAX_PER_FILE: usize = 3;
pub const MAX_PER_SYMBOL: usize = 2;
pub const OVERFETCH_FACTOR: usize = 8;
pub const SCOPED_OVERFETCH: usize = 6;
pub const FETCH_FLOOR: usize = 48;
const DEFINITION_WEIGHT: f64 = 3.0;
const TEST_DEFINITION_WEIGHT: f64 = 1.0;
const SOURCE_WEIGHT: f64 = 1.5;
/// Channels that say how a chunk matched, as against ranking preferences.
const MATCH_CHANNELS: &[&str] = &["semantic", "full_text", "definition"];

static QUERY_IDENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[A-Za-z_][A-Za-z0-9_]{2,}").unwrap());

/// A `chunks` row as retrieval sees it. Empty `symbols` / `parent_id` and a
/// zero `full_chars` mean absent, as Cortex's row reader treats them.
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkRow {
    pub id: String,
    pub file_path: String,
    pub start_line: i64,
    pub end_line: i64,
    pub content: String,
    pub kind: String,
    pub language: String,
    pub symbols: String,
    pub full_chars: i64,
    pub parent_id: String,
}

/// A ranked chunk: its relative score, the channels that matched it, and the
/// preference channels that moved it.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub chunk: ChunkRow,
    pub score: f64,
    pub channels: Vec<String>,
    pub ranking_signals: Vec<String>,
}

/// What the diversity caps held back (Cortex's diagnostics dict).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Diagnostics {
    pub suppressed: IndexMap<String, usize>,
    pub suppressed_total: usize,
    pub backfilled: usize,
    pub capped_files: Vec<String>,
    pub suppressed_symbols: IndexMap<String, usize>,
    pub capped_symbols: Vec<String>,
    /// The candidate pool ran out before `top_k` was filled.
    pub exhausted: bool,
    pub max_per_file: usize,
    pub max_per_symbol: usize,
}

/// Python `str.split()`.
fn split_py(s: &str) -> impl Iterator<Item = &str> {
    s.split(is_py_whitespace).filter(|p| !p.is_empty())
}

/// Python `str.casefold()` when the result can equal an ASCII candidate:
/// these eleven are every non-ASCII character whose casefold is ASCII.
/// Anything else folds to something non-ASCII, so `None` (no match).
pub fn casefold_ascii(s: &str) -> Option<String> {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            c if c.is_ascii() => out.push(c.to_ascii_lowercase()),
            '\u{df}' | '\u{1e9e}' => out.push_str("ss"),
            '\u{17f}' => out.push('s'),
            '\u{212a}' => out.push('k'),
            '\u{fb00}' => out.push_str("ff"),
            '\u{fb01}' => out.push_str("fi"),
            '\u{fb02}' => out.push_str("fl"),
            '\u{fb03}' => out.push_str("ffi"),
            '\u{fb04}' => out.push_str("ffl"),
            '\u{fb05}' | '\u{fb06}' => out.push_str("st"),
            _ => return None,
        }
    }
    Some(out)
}

/// Written the way code is written: an underscore or a lower-to-upper step.
fn looks_like_code(token: &str) -> bool {
    if token.contains('_') {
        return true;
    }
    let b = token.as_bytes();
    b.windows(2)
        .any(|w| w[0].is_ascii_lowercase() && w[1].is_ascii_uppercase())
}

/// Words in a query that plausibly name a symbol: the code-shaped ones when
/// there are any, else every identifier when there are at most two.
pub fn symbol_candidates(query: &str) -> Vec<String> {
    let tokens: Vec<&str> = QUERY_IDENT.find_iter(query).map(|m| m.as_str()).collect();
    let code: Vec<String> = tokens
        .iter()
        .filter(|t| looks_like_code(t))
        .map(|t| t.to_string())
        .collect();
    if !code.is_empty() {
        return code;
    }
    if tokens.len() <= 2 {
        tokens.into_iter().map(String::from).collect()
    } else {
        Vec::new()
    }
}

/// Keep the better-ranked of a parent/window pair; two windows of one parent
/// both survive. Ids with no chunk pass through.
fn drop_redundant_windows(ranked: Vec<String>, chunks: &HashMap<String, ChunkRow>) -> Vec<String> {
    let mut parent_kept = HashSet::new();
    let mut window_kept = HashSet::new();
    let mut out = Vec::new();
    for cid in ranked {
        if let Some(c) = chunks.get(&cid) {
            if !c.parent_id.is_empty() {
                if parent_kept.contains(&c.parent_id) {
                    continue;
                }
                window_kept.insert(c.parent_id.clone());
            } else {
                if window_kept.contains(&cid) {
                    continue;
                }
                parent_kept.insert(cid.clone());
            }
        }
        out.push(cid);
    }
    out
}

/// Chunks defining a name the query asked for, in `ranked` order, split into
/// source definitions and test/doc definitions.
fn definition_channels(
    ranked: &[String],
    chunks: &HashMap<String, ChunkRow>,
    query: &str,
) -> (Vec<String>, Vec<String>) {
    let wanted: HashSet<String> = symbol_candidates(query)
        .iter()
        .map(|t| t.to_ascii_lowercase())
        .collect();
    let (mut source, mut other) = (Vec::new(), Vec::new());
    if wanted.is_empty() {
        return (source, other);
    }
    for cid in ranked {
        let Some(c) = chunks.get(cid) else { continue };
        let defines =
            split_py(&c.symbols).any(|n| casefold_ascii(n).is_some_and(|f| wanted.contains(&f)));
        if defines {
            if is_non_source(&c.file_path) {
                other.push(cid.clone());
            } else {
                source.push(cid.clone());
            }
        }
    }
    (source, other)
}

/// The per-symbol cap's identity for a chunk: windows share their parent's,
/// symbol-less chunks get a key of their own.
fn symbol_key(c: &ChunkRow) -> String {
    if !c.parent_id.is_empty() {
        return format!("{}::parent:{}", c.file_path, c.parent_id);
    }
    match split_py(&c.symbols).next() {
        Some(first) => format!("{}::{first}", c.file_path),
        None => format!("{}::#{}", c.file_path, c.id),
    }
}

const COLUMNS: &str =
    "id, file_path, start_line, end_line, content, kind, language, symbols, full_chars, parent_id";

fn row(r: &rusqlite::Row) -> rusqlite::Result<ChunkRow> {
    Ok(ChunkRow {
        id: r.get(0)?,
        file_path: r.get(1)?,
        start_line: r.get(2)?,
        end_line: r.get(3)?,
        content: r.get(4)?,
        kind: r.get(5)?,
        language: r.get(6)?,
        symbols: r.get::<_, Option<String>>(7)?.unwrap_or_default(),
        full_chars: r.get::<_, Option<i64>>(8)?.unwrap_or_default(),
        parent_id: r.get::<_, Option<String>>(9)?.unwrap_or_default(),
    })
}

/// The chunks at these rowids, in the order given (missing ones skipped).
pub fn chunks_by_rowids(conn: &Connection, rowids: &[i64]) -> rusqlite::Result<Vec<ChunkRow>> {
    let mut found: HashMap<i64, ChunkRow> = HashMap::new();
    for batch in rowids.chunks(500) {
        let marks = vec!["?"; batch.len()].join(",");
        let mut stmt = conn.prepare(&format!(
            "SELECT rowid, {COLUMNS} FROM chunks WHERE rowid IN ({marks})"
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(batch), |r| {
            let rowid: i64 = r.get(0)?;
            Ok((
                rowid,
                ChunkRow {
                    id: r.get(1)?,
                    file_path: r.get(2)?,
                    start_line: r.get(3)?,
                    end_line: r.get(4)?,
                    content: r.get(5)?,
                    kind: r.get(6)?,
                    language: r.get(7)?,
                    symbols: r.get::<_, Option<String>>(8)?.unwrap_or_default(),
                    full_chars: r.get::<_, Option<i64>>(9)?.unwrap_or_default(),
                    parent_id: r.get::<_, Option<String>>(10)?.unwrap_or_default(),
                },
            ))
        })?;
        for r in rows {
            let (rowid, c) = r?;
            found.insert(rowid, c);
        }
    }
    Ok(rowids.iter().filter_map(|r| found.remove(r)).collect())
}

/// The chunks with these ids that exist (Cortex `get_by_ids`).
pub fn chunks_by_ids(conn: &Connection, ids: &[String]) -> rusqlite::Result<Vec<ChunkRow>> {
    let mut out = Vec::new();
    // SQLite caps bound parameters; batches keep any id list legal.
    for batch in ids.chunks(500) {
        let marks = vec!["?"; batch.len()].join(",");
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM chunks WHERE id IN ({marks})"
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(batch), row)?;
        for r in rows {
            out.push(r?);
        }
    }
    Ok(out)
}

/// How many candidates the vector channel is asked for, for a request's
/// `top_k` and `path` (the depth `search` itself uses).
pub fn semantic_depth(top_k: i64, path: &str) -> usize {
    let scope = py_strip(path).trim_matches('/');
    fetch_depth(top_k.max(1) as usize, !scope.is_empty())
}

/// How many candidates each channel is asked for.
pub fn fetch_depth(top_k: usize, scoped: bool) -> usize {
    let fetch = (top_k * OVERFETCH_FACTOR).max(FETCH_FLOOR);
    if scoped {
        fetch * SCOPED_OVERFETCH
    } else {
        fetch
    }
}

/// Ranked chunks plus what the diversity caps held back. `semantic` is the
/// vector channel's answer at `fetch_depth`, best first (empty for
/// keyword-only); `top_k` below 1 means 1; `path` scopes to a subtree or file.
pub fn search(
    conn: &Connection,
    semantic: Vec<ChunkRow>,
    query: &str,
    top_k: i64,
    path: &str,
) -> rusqlite::Result<(Vec<Hit>, Diagnostics)> {
    let top_k = top_k.max(1) as usize;
    let scope = py_strip(path).trim_matches('/');
    let in_scope = |fp: &str| {
        scope.is_empty()
            || fp == scope
            || fp.strip_prefix(scope).is_some_and(|r| r.starts_with('/'))
    };
    let fetch = fetch_depth(top_k, !scope.is_empty());

    let vec_results: Vec<ChunkRow> = semantic
        .into_iter()
        .filter(|c| in_scope(&c.file_path))
        .collect();
    let semantic_ids: Vec<String> = vec_results.iter().map(|c| c.id.clone()).collect();
    let mut chunks: HashMap<String, ChunkRow> =
        vec_results.into_iter().map(|c| (c.id.clone(), c)).collect();

    let mut fts_ids: Vec<String> = crate::fts::search(conn, query, fetch)
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    let missing: Vec<String> = fts_ids
        .iter()
        .filter(|id| !chunks.contains_key(*id))
        .cloned()
        .collect();
    for c in chunks_by_ids(conn, &missing)? {
        if in_scope(&c.file_path) {
            chunks.insert(c.id.clone(), c);
        }
    }
    if !scope.is_empty() {
        fts_ids.retain(|id| chunks.contains_key(id));
    }

    let first = fuse(&[("semantic", &semantic_ids), ("full_text", &fts_ids)], &[]);
    let ordered: Vec<String> = first.iter().map(|f| f.key.clone()).collect();
    let (defs, test_defs) = definition_channels(&ordered, &chunks, query);
    let source: Vec<String> = ordered
        .iter()
        .filter(|id| {
            chunks
                .get(*id)
                .is_some_and(|c| !is_non_source(&c.file_path))
        })
        .cloned()
        .collect();
    let fused = if defs.is_empty() && test_defs.is_empty() && source.is_empty() {
        first
    } else {
        fuse(
            &[
                ("semantic", &semantic_ids),
                ("full_text", &fts_ids),
                ("definition", &defs),
                ("test_definition", &test_defs),
                ("source", &source),
            ],
            &[
                ("definition", DEFINITION_WEIGHT),
                ("test_definition", TEST_DEFINITION_WEIGHT),
                ("source", SOURCE_WEIGHT),
            ],
        )
    };

    let by_id: HashMap<&str, (f64, &[String])> = fused
        .iter()
        .map(|f| (f.key.as_str(), (f.relative, f.channels.as_slice())))
        .collect();
    let annotate = |c: &ChunkRow| -> Hit {
        let (score, found) = by_id[c.id.as_str()];
        let (matched, ranking): (Vec<String>, Vec<String>) = found
            .iter()
            .cloned()
            .partition(|ch| MATCH_CHANNELS.contains(&ch.as_str()));
        Hit {
            chunk: c.clone(),
            score,
            channels: matched,
            ranking_signals: ranking,
        }
    };

    let ranked = drop_redundant_windows(fused.iter().map(|f| f.key.clone()).collect(), &chunks);

    // Pass 1: diversity caps, holding what they push out.
    let mut out: Vec<Hit> = Vec::new();
    let mut per_file: HashMap<&str, usize> = HashMap::new();
    let mut per_symbol: HashMap<String, usize> = HashMap::new();
    let mut held: Vec<&ChunkRow> = Vec::new();
    let mut suppressed: IndexMap<String, usize> = IndexMap::new();
    let mut suppressed_symbols: IndexMap<String, usize> = IndexMap::new();
    let mut pool = 0;
    for cid in &ranked {
        let Some(c) = chunks.get(cid) else { continue };
        pool += 1;
        if out.len() >= top_k {
            continue;
        }
        let skey = symbol_key(c);
        let over_file = per_file.get(c.file_path.as_str()).copied().unwrap_or(0) >= MAX_PER_FILE;
        let over_symbol = per_symbol.get(&skey).copied().unwrap_or(0) >= MAX_PER_SYMBOL;
        if over_file || over_symbol {
            held.push(c);
            if over_file {
                *suppressed.entry(c.file_path.clone()).or_insert(0) += 1;
            } else {
                *suppressed_symbols.entry(skey).or_insert(0) += 1;
            }
            continue;
        }
        out.push(annotate(c));
        *per_file.entry(c.file_path.as_str()).or_insert(0) += 1;
        *per_symbol.entry(skey).or_insert(0) += 1;
    }

    // Pass 2: backfill held chunks in rank order; each retires its cap count.
    let mut backfilled = 0;
    for c in held {
        if out.len() >= top_k {
            break;
        }
        out.push(annotate(c));
        let skey = symbol_key(c);
        if let Some(n) = suppressed.get_mut(&c.file_path).filter(|n| **n > 0) {
            *n -= 1;
            if *n == 0 {
                suppressed.shift_remove(&c.file_path);
            }
        } else if let Some(n) = suppressed_symbols.get_mut(&skey).filter(|n| **n > 0) {
            *n -= 1;
            if *n == 0 {
                suppressed_symbols.shift_remove(&skey);
            }
        }
        backfilled += 1;
    }

    let mut capped_files: Vec<String> = suppressed.keys().cloned().collect();
    capped_files.sort();
    let mut capped_symbols: Vec<String> = suppressed_symbols.keys().cloned().collect();
    capped_symbols.sort();
    let diagnostics = Diagnostics {
        suppressed_total: suppressed.values().sum::<usize>()
            + suppressed_symbols.values().sum::<usize>(),
        exhausted: out.len() < top_k && pool <= out.len(),
        suppressed,
        backfilled,
        capped_files,
        suppressed_symbols,
        capped_symbols,
        max_per_file: MAX_PER_FILE,
        max_per_symbol: MAX_PER_SYMBOL,
    };
    Ok((out, diagnostics))
}
