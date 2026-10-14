//! Shared pieces of Cortex's per-language adapter modules
//! (`languages/*.py`), which build `Parsed` without the generic engine.

use crate::text::{decode_replace, py_strip, splitlines};
use std::collections::BTreeMap;
use tree_sitter::Node;
use vex_core::Chunk;
use vex_core::ids::chunk_id;

/// `source.decode(errors="replace").splitlines()`.
pub fn lines_of(source: &[u8]) -> Vec<String> {
    splitlines(&decode_replace(source))
        .into_iter()
        .map(String::from)
        .collect()
}

/// The adapters' `add_chunk` once `end` is clamped: `lines[start:end+1]`
/// joined, None when blank; `symbols` metadata unless `name` is None.
pub fn chunk(
    rel: &str,
    lines: &[String],
    start: usize,
    end: usize,
    kind: &str,
    language: &str,
    name: Option<&str>,
) -> Option<Chunk> {
    let hi = (end + 1).min(lines.len());
    let content = if start < hi {
        lines[start..hi].join("\n")
    } else {
        String::new()
    };
    if py_strip(&content).is_empty() {
        return None;
    }
    let mut meta = BTreeMap::new();
    if let Some(n) = name {
        meta.insert("symbols".to_string(), n.into());
    }
    Some(Chunk::with_metadata(
        chunk_id(rel, start, &content),
        rel,
        start + 1,
        end + 1,
        content,
        kind,
        language,
        meta,
    ))
}

/// Every child, named or not (`node.children`).
pub fn kids<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    crate::ast::children(node)
}

/// Python `str.rsplit(".", 1)[-1]`.
pub fn last_segment(s: &str) -> &str {
    s.rsplit('.').next().unwrap_or(s)
}

/// Rebuild a refs Counter without the file's own definitions.
pub fn drop_own(refs: &mut indexmap::IndexMap<String, u64>, names: &[String]) {
    for n in names {
        refs.shift_remove(n);
    }
}
