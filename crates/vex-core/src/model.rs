//! The per-file extraction result and its parts, mirroring Cortex's
//! `store.chunks.Chunk`, `store.graph.Symbol` and `indexer.astchunk.Parsed`
//! field for field (port contract §1–§3).

use indexmap::IndexMap;
use serde_json::Value;
use std::collections::BTreeMap;

/// Stored chunk bodies are cut at this many characters (Unicode code
/// points, as Python's `len(str)` counts them). Cortex:
/// `application.recovery.CAP_CHUNK_CHARS`.
pub const CAP_CHUNK_CHARS: usize = 5_000;

/// A searchable span of a file.
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    /// Computed from the UNCAPPED content (see `ids::chunk_id`).
    pub id: String,
    pub file_path: String,
    /// 1-based, inclusive.
    pub start_line: usize,
    pub end_line: usize,
    /// Capped at [`CAP_CHUNK_CHARS`] code points.
    pub content: String,
    pub kind: String,
    pub language: String,
    /// `symbols` (space-joined names) for AST and line chunks, absent for
    /// residual chunks; `full_chars` when the content was capped.
    pub metadata: BTreeMap<String, Value>,
}

impl Chunk {
    /// A chunk with Cortex's defaults (`kind="block"`, `language=""`, empty
    /// metadata), with the content cap applied.
    pub fn new(
        id: impl Into<String>,
        file_path: impl Into<String>,
        start_line: usize,
        end_line: usize,
        content: impl Into<String>,
    ) -> Chunk {
        Chunk::with_metadata(
            id,
            file_path,
            start_line,
            end_line,
            content,
            "block",
            "",
            BTreeMap::new(),
        )
    }

    /// Every field explicit. Mirrors `Chunk.__post_init__`: when the content
    /// exceeds the cap, `full_chars` is set only if not already present
    /// (`setdefault`) and the content is cut to the first CAP code points.
    #[allow(clippy::too_many_arguments)]
    pub fn with_metadata(
        id: impl Into<String>,
        file_path: impl Into<String>,
        start_line: usize,
        end_line: usize,
        content: impl Into<String>,
        kind: impl Into<String>,
        language: impl Into<String>,
        mut metadata: BTreeMap<String, Value>,
    ) -> Chunk {
        let mut content = content.into();
        let chars = content.chars().count();
        if chars > CAP_CHUNK_CHARS {
            metadata
                .entry("full_chars".to_string())
                .or_insert(Value::from(chars));
            let cut = content
                .char_indices()
                .nth(CAP_CHUNK_CHARS)
                .map(|(i, _)| i)
                .unwrap_or(content.len());
            content.truncate(cut);
        }
        Chunk {
            id: id.into(),
            file_path: file_path.into(),
            start_line,
            end_line,
            content,
            kind: kind.into(),
            language: language.into(),
            metadata,
        }
    }
}

/// One definition site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub qualified_name: String,
    pub name: String,
    pub kind: String,
    pub file_path: String,
    pub start_line: usize,
    pub end_line: usize,
    /// True for a bodiless site (an interface method, a prototype).
    pub declaration: bool,
}

/// Everything extracted from one file. Maps keep insertion order, as Python
/// dicts and Counters do, so no later step can diverge on iteration order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Parsed {
    pub chunks: Vec<Chunk>,
    pub symbols: Vec<Symbol>,
    /// Module strings as written; duplicates are kept.
    pub imports: Vec<String>,
    /// Referenced name -> count.
    pub refs: IndexMap<String, u64>,
    /// Local name -> (module as written, original name).
    pub bindings: IndexMap<String, (String, String)>,
    /// (caller qualified name or `file:<rel>`, callee name or marker) -> count.
    pub calls: IndexMap<(String, String), u64>,
    /// (owner without file prefix, member, type).
    pub members: Vec<(String, String, String)>,
}
