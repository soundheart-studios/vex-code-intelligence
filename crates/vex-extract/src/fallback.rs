//! The line chunker: Cortex's `chunker._chunk_lines`, used when a language
//! has no grammar or the AST pass yields no chunks. Fixed 60-line windows,
//! snapped to a definition boundary, overlapping by 5 lines, with heuristic
//! symbol and import regexes.
//!
//! The regexes are Python `re` patterns over `str`, so `\w` is
//! `[\p{L}\p{N}_]` (Python's str.isalnum() or `_`) and `\s` is Unicode
//! White_Space plus `\x1c`-`\x1f` (str.isspace()). Rust's own `\w`/`\s`
//! differ on combining marks, connector punctuation and those separators, so
//! the classes are spelled out.

use crate::text::{decode_replace, py_strip, splitlines};
use regex::Regex;
use std::collections::BTreeMap;
use std::sync::OnceLock;
use vex_core::ids::chunk_id;
use vex_core::{Chunk, Parsed, Symbol};

const CHUNK_LINES: usize = 60;
const OVERLAP: usize = 5;
/// Snap a window's end to a def boundary at least this far into it.
const MIN_SNAP: usize = 20;
/// A def opens a new boundary only this far past the previous one.
const BOUNDARY_GAP: usize = 10;

const W: &str = r"[\p{L}\p{N}_]";
const S: &str = r"[\s\x1c-\x1f]";

fn py(pattern: &str) -> Regex {
    // `\b` after a literal word: a following non-word character or the end.
    let p = pattern
        .replace(r"\w", W)
        .replace(r"\s", S)
        .replace(r"\b", r"(?:[^\p{L}\p{N}_]|$)");
    Regex::new(&p).expect("valid translated pattern")
}

struct Patterns {
    defs: Vec<(&'static str, Regex)>,
    imports: Vec<(&'static str, Regex)>,
}

fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| Patterns {
        defs: vec![
            ("python", py(r"^\s*(?:async\s+)?(def|class)\s+(\w+)")),
            ("javascript", py(r"^\s*(?:export\s+)?(?:async\s+)?(function|class)\s+(\w+)")),
            ("typescript", py(r"^\s*(?:export\s+)?(?:async\s+)?(function|class|interface)\s+(\w+)")),
            ("go", py(r"^func\s+(?:\([^)]*\)\s+)?(\w+)")),
            ("rust", py(r"^\s*(?:pub\s+)?(?:async\s+)?(fn|struct|trait|impl)\s+(\w+)")),
            ("java", py(r"^\s*(?:public|private|protected)?\s*(?:static\s+)?(class|interface|enum)\s+(\w+)")),
        ],
        imports: vec![
            ("python", py(r"^\s*(?:from\s+([\w.]+)\s+import|import\s+([\w.]+))")),
            ("javascript", py(r#"^\s*import\b.*?from\s+['"]([^'"]+)['"]"#)),
            ("typescript", py(r#"^\s*import\b.*?from\s+['"]([^'"]+)['"]"#)),
            ("go", py(r#"^\s*(?:import\s+)?"([\w./-]+)"$"#)),
        ],
    })
}

fn lookup<'a>(table: &'a [(&str, Regex)], lang: &str) -> Option<&'a Regex> {
    table.iter().find(|(l, _)| *l == lang).map(|(_, r)| r)
}

/// The source lines as Cortex reads them: decode with replacement, then
/// Python `str.splitlines()` (equivalent to `read_text()` plus splitlines,
/// whose universal-newline translation only affects `\r`/`\r\n`).
pub fn source_lines(bytes: &[u8]) -> Vec<String> {
    splitlines(&decode_replace(bytes))
        .into_iter()
        .map(String::from)
        .collect()
}

/// Cortex `_chunk_lines(root, path, lang)` over a file's bytes.
pub fn line_chunks(rel: &str, lang: &str, bytes: &[u8]) -> Parsed {
    let lines = source_lines(bytes);
    let p = patterns();
    let def_re = lookup(&p.defs, lang);
    let imp_re = lookup(&p.imports, lang);
    let prefix = rel.replace('/', ".");
    let mut symbols: Vec<Symbol> = Vec::new();
    let mut imports: Vec<String> = Vec::new();
    let mut boundaries: Vec<usize> = vec![0];
    for (i, line) in lines.iter().enumerate() {
        if let Some(caps) = def_re.and_then(|r| r.captures(line)) {
            // Python's m.lastindex: the last group that took part.
            let last = (1..caps.len())
                .rev()
                .find(|&g| caps.get(g).is_some())
                .unwrap_or(0);
            let name = caps.get(last).map_or("", |m| m.as_str()).to_string();
            let is_class = last >= 2
                && matches!(
                    caps.get(1).map(|m| m.as_str()),
                    Some("class" | "interface" | "struct" | "trait")
                );
            symbols.push(Symbol {
                qualified_name: format!("{prefix}.{name}"),
                name,
                kind: if is_class { "class" } else { "function" }.to_string(),
                file_path: rel.to_string(),
                start_line: i + 1,
                end_line: i + 1,
                declaration: false,
            });
            if i - boundaries[boundaries.len() - 1] >= BOUNDARY_GAP {
                boundaries.push(i);
            }
        }
        // `next(g for g in m.groups() if g)`: the first non-empty group.
        let first = imp_re.and_then(|r| r.captures(line)).and_then(|caps| {
            (1..caps.len())
                .filter_map(|g| caps.get(g))
                .find(|m| !m.as_str().is_empty())
                .map(|m| m.as_str().to_string())
        });
        if let Some(module) = first {
            imports.push(module);
        }
    }

    let mut chunks = Vec::new();
    let mut start = 0usize;
    while start < lines.len() {
        let mut end = (start + CHUNK_LINES).min(lines.len());
        if let Some(&b) = boundaries
            .iter()
            .find(|&&b| start + MIN_SNAP <= b && b < end)
        {
            end = b;
        }
        let content = lines[start..end].join("\n");
        if !py_strip(&content).is_empty() {
            let local: Vec<&str> = symbols
                .iter()
                .filter(|s| start < s.start_line && s.start_line <= end)
                .map(|s| s.name.as_str())
                .collect();
            let mut meta = BTreeMap::new();
            meta.insert("symbols".to_string(), local.join(" ").into());
            let kind = if start == 0 { "module" } else { "block" };
            chunks.push(Chunk::with_metadata(
                chunk_id(rel, start, &content),
                rel,
                start + 1,
                end,
                content,
                kind,
                lang,
                meta,
            ));
        }
        start = if end < lines.len() {
            end.saturating_sub(OVERLAP).max(start + 1)
        } else {
            end
        };
    }
    Parsed {
        chunks,
        symbols,
        imports,
        ..Parsed::default()
    }
}
