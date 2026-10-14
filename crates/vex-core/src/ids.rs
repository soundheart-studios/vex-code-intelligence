//! Chunk ids and qualified names (port contract §2, §4).

use sha2::{Digest, Sha256};

fn short_sha(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    let mut hex = String::with_capacity(24);
    for b in &digest[..12] {
        hex.push_str(&format!("{b:02x}"));
    }
    hex
}

/// Id of an AST or line chunk: the first 24 hex characters of
/// `sha256("{rel}:{start_row0}:{content}")`, where `start_row0` is the 0-based
/// first row and `content` the chunk's source lines joined with `\n` BEFORE
/// the content cap is applied.
pub fn chunk_id(rel: &str, start_row0: usize, content: &str) -> String {
    short_sha(&format!("{rel}:{start_row0}:{content}"))
}

/// Id of a residual (gap-fill) chunk. Note the different formula: the
/// literal `residual` segment and a 1-based start line (Cortex
/// `chunker._window`).
pub fn residual_chunk_id(rel: &str, start_line1: usize, content: &str) -> String {
    short_sha(&format!("{rel}:residual:{start_line1}:{content}"))
}

/// The qualified-name prefix of a file: its relative path with `/` -> `.`.
pub fn qual_prefix(rel: &str) -> String {
    rel.replace('/', ".")
}

/// `prefix` + `.` + owners and name already joined with `.`.
pub fn qualify(prefix: &str, full: &str) -> String {
    format!("{prefix}.{full}")
}

/// Name of an anonymous function or class: `(anonymous@line:col)`, both
/// 1-based, the column being tree-sitter's (a BYTE column) plus one.
pub fn anonymous(row0: usize, byte_col0: usize) -> String {
    format!("(anonymous@{}:{})", row0 + 1, byte_col0 + 1)
}
