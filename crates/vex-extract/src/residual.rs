//! Residual chunks: Cortex's `chunker._fill_gaps` and `_window`. Line spans
//! no AST chunk covers become `residual` chunks, so file-scope tables and
//! macro blocks are searchable. Language-agnostic by construction.

use vex_core::ids::residual_chunk_id;
use vex_core::{Chunk, Parsed};

/// Gaps shorter than this are closing braces and blank lines.
pub const MIN_GAP_LINES: usize = 3;
/// Uncovered spans are windowed into chunks of at most this many lines.
pub const RESIDUAL_WINDOW: usize = 80;

/// Append residual chunks for every uncovered run of at least
/// [`MIN_GAP_LINES`] lines. `lines` are the file's source lines.
pub fn fill_gaps(rel: &str, lang: &str, lines: &[String], parsed: &mut Parsed) {
    if lines.is_empty() {
        return;
    }
    let n = lines.len();
    let mut covered = vec![false; n + 2];
    for c in &parsed.chunks {
        let lo = c.start_line.max(1);
        let hi = c.end_line.min(n);
        for slot in covered.iter_mut().take(hi + 1).skip(lo) {
            *slot = true;
        }
    }
    let mut residual = Vec::new();
    let mut run_start: Option<usize> = None;
    // Mirrors Cortex's `for ln in range(1, len(lines) + 2)` line for line,
    // including the sentinel past the end that closes a trailing run.
    #[allow(clippy::needless_range_loop)]
    for ln in 1..=n + 1 {
        let filled = ln <= n && covered[ln];
        if !filled && ln <= n {
            run_start.get_or_insert(ln);
            continue;
        }
        let Some(rs) = run_start.take() else { continue };
        let run_end = ln - 1;
        if run_end + 1 - rs >= MIN_GAP_LINES {
            residual.extend(window(rel, lang, lines, rs, run_end));
        }
    }
    parsed.chunks.extend(residual);
}

/// Split one uncovered span (1-based, inclusive) into window chunks.
fn window(rel: &str, lang: &str, lines: &[String], start: usize, end: usize) -> Vec<Chunk> {
    let mut out = Vec::new();
    let mut pos = start;
    while pos <= end {
        let stop = (pos + RESIDUAL_WINDOW - 1).min(end);
        let content = lines[pos - 1..stop].join("\n");
        if !crate::text::py_strip(&content).is_empty() {
            out.push(Chunk::with_metadata(
                residual_chunk_id(rel, pos, &content),
                rel,
                pos,
                stop,
                content,
                "residual",
                lang,
                Default::default(),
            ));
        }
        pos = stop + 1;
    }
    out
}
