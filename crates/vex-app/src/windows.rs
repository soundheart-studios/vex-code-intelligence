//! Sub-chunk windows: narrower views of a chunk too big for one embedding
//! (Cortex `indexer/windows.py`). Additive: the parent keeps its row; each
//! window records it in `parent_id`.

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use vex_core::Chunk;

/// Only split a chunk estimated above this many tokens.
pub const SPLIT_ABOVE_TOKENS: usize = 400;
pub const WINDOW_TOKENS: f64 = 350.0;
pub const STRIDE_TOKENS: f64 = 260.0;
const CHARS_PER_TOKEN: f64 = 3.3;

/// `max(1, int(len(text) / 3.3))`, len in characters.
pub fn tokens(text: &str) -> usize {
    ((text.chars().count() as f64 / CHARS_PER_TOKEN) as usize).max(1)
}

/// Python `str.splitlines(keepends=True)`.
fn splitlines_keepends(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let is_break = matches!(
            c,
            '\n' | '\r'
                | '\u{0b}'
                | '\u{0c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if !is_break {
            continue;
        }
        let mut end = i + c.len_utf8();
        if c == '\r' && it.peek().map(|&(_, n)| n) == Some('\n') {
            it.next();
            end += 1;
        }
        out.push(&s[start..end]);
        start = end;
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// Narrower views of `chunk`, or none when it is already small enough.
/// Splits on line boundaries only, so every window's line span is truthful.
pub fn windows_for(chunk: &Chunk) -> Vec<Chunk> {
    if tokens(&chunk.content) <= SPLIT_ABOVE_TOKENS {
        return Vec::new();
    }
    let lines = splitlines_keepends(&chunk.content);
    if lines.len() < 2 {
        return Vec::new();
    }
    let len = |l: &str| l.chars().count() as f64;
    let mut out = Vec::new();
    let mut start = 0usize;
    while start < lines.len() {
        let mut size = 0.0;
        let mut end = start;
        while end < lines.len() && size < WINDOW_TOKENS * CHARS_PER_TOKEN {
            size += len(lines[end]);
            end += 1;
        }
        let body: String = lines[start..end].concat();
        let first_line = chunk.start_line + start;
        let digest = Sha256::digest(format!("{}:{first_line}:{body}", chunk.file_path).as_bytes());
        let id: String = digest[..12].iter().map(|b| format!("{b:02x}")).collect();
        let mut meta = BTreeMap::new();
        if let Some(sym) = chunk
            .metadata
            .get("symbols")
            .filter(|v| v.as_str().is_some_and(|s| !s.is_empty()))
        {
            meta.insert("symbols".to_string(), sym.clone());
        }
        meta.insert("parent_id".to_string(), chunk.id.clone().into());
        out.push(Chunk::with_metadata(
            id,
            chunk.file_path.clone(),
            first_line,
            chunk.start_line + end - 1,
            body,
            chunk.kind.clone(),
            chunk.language.clone(),
            meta,
        ));
        if end >= lines.len() {
            break;
        }
        let step_chars = STRIDE_TOKENS * CHARS_PER_TOKEN;
        let mut moved = 0.0;
        let mut next = start;
        while next + 1 < end && moved < step_chars {
            moved += len(lines[next]);
            next += 1;
        }
        start = if next > start { next } else { end };
    }
    if out.len() > 1 { out } else { Vec::new() }
}
