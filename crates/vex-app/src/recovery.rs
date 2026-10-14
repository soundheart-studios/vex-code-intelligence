//! Recovery pointers (Cortex `application/recovery.py`): every cap answers
//! what was withheld, why, and the exact next call that retrieves it. The
//! caps live here so the limit an agent is told about is the one enforced.

use crate::pyjson::Json;

pub const CAP_GRAPH_NODES: usize = 150;
pub const CAP_GRAPH_EDGES: usize = 300;
pub const CAP_BATCH_TARGETS: usize = 12;
pub const CAP_BATCH_BYTES_MAX: usize = 48_000;
pub const CAP_BATCH_BYTES_MIN: usize = 2_000;
pub const CAP_SEARCH_PER_FILE: usize = vex_search::retriever::MAX_PER_FILE;
pub const CAP_SEARCH_PER_SYMBOL: usize = vex_search::retriever::MAX_PER_SYMBOL;
pub const CAP_PREVIEW_CHARS: usize = 400;
pub const CAP_CHUNK_CHARS: usize = 5_000;
pub const CAP_GREP_LINE_CHARS: usize = 300;
pub const DEFAULT_CONFIDENCE_MIN: f64 = 0.7;
pub const CAP_CONTEXT_EDGES: usize = 25;

/// `recovery.note(...)`. `limit` is pre-rendered: Cortex formats it with
/// str() in the hint, so callers pass both the JSON value and its text.
pub fn note(
    reason: &str,
    withheld: usize,
    limit: Json,
    next_calls: Vec<String>,
    detail: Option<Json>,
) -> Json {
    assert!(
        !next_calls.is_empty(),
        "a recovery pointer needs a next call"
    );
    let limit_text = match &limit {
        Json::Int(i) => i.to_string(),
        Json::Float(f) => crate::pyjson::float_repr(*f),
        Json::Str(s) => s.clone(),
        other => crate::pyjson::dumps(other, crate::pyjson::DEFAULT),
    };
    let hint = format!(
        "{withheld} item(s) withheld at limit {limit_text}. Retrieve them with: {} \
         -- this is reachable through Cortex; no file read needed.",
        next_calls[0]
    );
    let mut pairs = vec![
        ("reason", Json::from(reason)),
        ("withheld", Json::from(withheld)),
        ("limit", limit),
        ("next", Json::from(next_calls)),
        ("hint", Json::from(hint)),
    ];
    if let Some(d) = detail.filter(Json::truthy) {
        pairs.push(("detail", d));
    }
    Json::obj(pairs)
}
