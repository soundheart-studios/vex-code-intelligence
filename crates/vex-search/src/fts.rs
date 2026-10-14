//! FTS5 keyword retrieval over `chunks_fts` (Cortex `store/chunks.py`).

use regex::Regex;
use rusqlite::Connection;
use std::collections::HashSet;
use std::sync::LazyLock;

const MAX_FTS_TERMS: usize = 12;
static FTS_TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[A-Za-z_][A-Za-z0-9_]*|[0-9]+").unwrap());
const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "do", "does", "for", "from", "how", "in",
    "is", "it", "of", "on", "or", "the", "to", "what", "when", "where", "which", "with",
];

/// A safe, recall-oriented FTS5 query: plain tokens, prose stopwords and
/// repeats (case-insensitively) dropped, at most twelve, each quoted and
/// OR-ed. A query with no usable token becomes `""`, which matches nothing.
pub fn escape_query(query: &str) -> String {
    let mut terms: Vec<&str> = Vec::new();
    let mut seen = HashSet::new();
    for m in FTS_TOKEN.find_iter(query) {
        // Tokens are ASCII by construction, so casefold is ASCII lowercase.
        let folded = m.as_str().to_ascii_lowercase();
        if STOPWORDS.contains(&folded.as_str()) || !seen.insert(folded) {
            continue;
        }
        terms.push(m.as_str());
        if terms.len() >= MAX_FTS_TERMS {
            break;
        }
    }
    if terms.is_empty() {
        return "\"\"".to_string();
    }
    terms
        .iter()
        .map(|t| format!("\"{t}\""))
        .collect::<Vec<_>>()
        .join(" OR ")
}

/// `[(chunk_id, bm25 rank)]`, best (lowest rank) first. A query FTS5 rejects
/// answers nothing rather than failing the search, as in Cortex.
pub fn search(conn: &Connection, query: &str, limit: usize) -> Vec<(String, f64)> {
    let run = || -> rusqlite::Result<Vec<(String, f64)>> {
        let mut stmt = conn.prepare_cached(
            "SELECT id, rank FROM chunks_fts WHERE chunks_fts MATCH ?1 ORDER BY rank LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![escape_query(query), limit as i64], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        rows.collect()
    };
    run().unwrap_or_default()
}
