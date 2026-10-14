//! `vex search`: one query or a list of them (Cortex `application/search.py`,
//! `application/investigation.py` and the `search` MCP tool).
//!
//! A single query returns ranked chunks with their source, a `ranking` note
//! saying what the scores mean (and whether a named symbol exists), a
//! `suppressed` pointer when the diversity caps held results back, and a
//! `freshness` block when the index is behind the working tree. A list of
//! questions returns a source-free bundle of symbol leads, chosen by the
//! rules-v1 selector, with `fetch_targets` for one follow-up expand.
//!
//! Payloads are built in Cortex's key order and written with
//! `pyjson::dumps`, because byte budgets are measured on that exact text.
//! Savings accounting is not ported (no ledger).

use crate::freshness;
use crate::pyjson::{self, Json, py_round, py_split, repr_str, take_chars};
use crate::recovery;
use crate::selector::{self, Lead};
use std::collections::HashMap;
use std::path::Path;
use vex_search::retriever::{self, ChunkRow, Diagnostics, Hit};
use vex_store::db::Index;

/// Below this gap between the top two scores there is no clear winner.
const DECISIVE_GAP: f64 = 0.15;
pub const MAX_QUERIES: usize = 6;
pub const MAX_TOP_K: i64 = 5;
pub const DISCOVERY_TOP_K: i64 = 10;
pub const MIN_BYTES: i64 = 2_000;
pub const MAX_BYTES: i64 = 20_000;
pub const MAX_LEADS: usize = 12;
/// A list query's default byte budget (the MCP tool's `_BUNDLE_BYTES`).
pub const BUNDLE_BYTES: i64 = 12_000;

/// What `search` answers: a JSON payload, Cortex's plain-text reply for an
/// empty result, or a refused request.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    Payload(Json),
    Text(String),
    Error { code: String, message: String },
}

impl Answer {
    /// The wire text, as the MCP tool returns it.
    pub fn wire(&self) -> String {
        match self {
            Answer::Payload(p) => pyjson::dumps(p, pyjson::DEFAULT),
            Answer::Text(t) => t.clone(),
            Answer::Error { code, message } => format!("CORTEX ERROR ({code}): {message}"),
        }
    }

    /// The CLI's `--json` document: schema 1, then the payload's own keys
    /// (or `message`, or `error` and `message`).
    pub fn cli_json(&self) -> Json {
        let mut pairs = vec![("schema".to_string(), Json::from(crate::SCHEMA as i64))];
        match self {
            Answer::Payload(Json::Obj(p)) => pairs.extend(p.iter().cloned()),
            Answer::Payload(other) => pairs.push(("result".into(), other.clone())),
            Answer::Text(t) => pairs.push(("message".into(), Json::from(t))),
            Answer::Error { code, message } => {
                pairs.push(("error".into(), Json::from(code)));
                pairs.push(("message".into(), Json::from(message)));
            }
        }
        Json::Obj(pairs)
    }
}

/// A search request. `queries` with one element is a single query unless
/// `list` is set (the MCP tool distinguishes `str` from `list[str]`).
#[derive(Debug, Clone)]
pub struct Request {
    pub queries: Vec<String>,
    pub list: bool,
    pub top_k: i64,
    pub max_bytes: i64,
    pub path: String,
    pub select: String,
    /// Bring the index up to date before answering when it is behind the
    /// working tree (Cortex's force_fresh; vex refreshes in place).
    pub force_fresh: bool,
}

impl Request {
    pub fn single(query: &str) -> Request {
        Request {
            queries: vec![query.to_string()],
            list: false,
            top_k: 5,
            max_bytes: 0,
            path: String::new(),
            select: String::new(),
            force_fresh: false,
        }
    }
}

/// The answer plus the freshness verdict (for the CLI's exit code).
#[derive(Debug, Clone)]
pub struct Outcome {
    pub answer: Answer,
    pub freshness: freshness::Verdict,
    /// Set when search fell back to keyword-only because the embedding
    /// model is unavailable; also carried in the payload as `notice`.
    pub notice: Option<String>,
}

/// Milliseconds since an arbitrary origin, like `time.perf_counter() * 1000`.
/// Injected so tests can freeze it.
pub type Clock<'a> = &'a dyn Fn() -> f64;

/// The real clock.
pub fn wall_clock() -> f64 {
    use std::sync::OnceLock;
    static ORIGIN: OnceLock<std::time::Instant> = OnceLock::new();
    ORIGIN
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_secs_f64()
        * 1000.0
}

/// `tok()`: Cortex's token estimate, characters / 3.3.
pub fn tok(text: &str) -> i64 {
    ((text.chars().count() as f64 / 3.3) as i64).max(1)
}

fn cortex_error(code: &str, message: &str) -> Answer {
    Answer::Error {
        code: code.to_string(),
        message: message.to_string(),
    }
}

/// `vex search`: the registered project containing `start`, searched with
/// the wall clock.
pub fn run(
    home: &Path,
    start: &Path,
    req: &Request,
) -> Result<Outcome, crate::project::ProjectError> {
    run_with(home, start, req, &crate::embed::Settings::from_env(home))
}

/// `run` under explicit embedding settings.
pub fn run_with(
    home: &Path,
    start: &Path,
    req: &Request,
    settings: &crate::embed::Settings,
) -> Result<Outcome, crate::project::ProjectError> {
    let (id, root) = crate::project::registered(home, start)?;
    let idx = Index::open(&vex_store::registry::index_path(home, &id))?;
    let mut session = crate::embed::Session::open(settings);
    if req.force_fresh && crate::freshness::probe(&idx, &root) {
        // Wait for the one writer (a background refresh may hold it), then
        // refresh only if that did not already bring the index up to date.
        let _writer = crate::project::lock_index(&idx.path, crate::project::WRITE_WAIT)?;
        if crate::freshness::probe(&idx, &root) {
            crate::index::run_with(&idx, &root, false, session.as_mut(), false)?;
        }
    }
    Ok(search_with(
        &idx,
        &root,
        req,
        &wall_clock,
        session.as_mut(),
    )?)
}

/// Run a request against an open index, keyword-only.
pub fn search(idx: &Index, root: &Path, req: &Request, clock: Clock) -> rusqlite::Result<Outcome> {
    search_with(idx, root, req, clock, None)
}

/// Run a request against an open index. With a session every question is
/// embedded in one model call; if the model is unavailable the answer is
/// keyword-only and says so.
pub fn search_with(
    idx: &Index,
    root: &Path,
    req: &Request,
    clock: Clock,
    session: Option<&mut crate::embed::Session>,
) -> rusqlite::Result<Outcome> {
    let mut notice = None;
    let mut semantic = None;
    if let Some(s) = session {
        if let Err(e) = vex_store::vec::assert_compatible(idx, s.dim()) {
            return Ok(Outcome {
                answer: cortex_error("index_dimension_mismatch", &e.to_string()),
                freshness: freshness::check(idx, root),
                notice: None,
            });
        }
        // Exactly the strings the retriever will see, in one model call.
        let texts = retrieval_queries(req);
        if texts.is_empty() {
            semantic = Some(Semantic::default());
        } else {
            let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
            match s.embed(&refs) {
                Some(vecs) => {
                    semantic = Some(Semantic {
                        vectors: texts.into_iter().zip(vecs).collect(),
                        warmup: warmup_note(idx)?,
                    })
                }
                None => notice = s.search_notice(),
            }
        }
    }
    let verdict = freshness::check(idx, root);
    let sem = semantic.as_ref();
    let answer = if req.list {
        investigate(idx, req, &verdict, clock, sem)?
    } else {
        let query = req.queries.first().map(String::as_str).unwrap_or("");
        let response = single(
            idx,
            query,
            req.top_k,
            &req.path,
            req.max_bytes,
            &verdict,
            sem,
        )?;
        if response.hits.is_empty() {
            no_results(idx, root)?
        } else {
            Answer::Payload(response.payload())
        }
    };
    let answer = match (answer, &notice) {
        (Answer::Payload(mut p), Some(n)) => {
            p.set("notice", Json::from(n.as_str()));
            Answer::Payload(p)
        }
        (a, _) => a,
    };
    Ok(Outcome {
        answer,
        freshness: verdict,
        notice,
    })
}

/// The vector channel's inputs for one request: each retrieval query's
/// embedding, and the warm-up note while vectors are still being filled.
#[derive(Debug, Default)]
pub struct Semantic {
    vectors: HashMap<String, Vec<f32>>,
    warmup: Option<String>,
}

/// The strings a request hands the retriever: the query itself, or a
/// list's stripped non-empty questions (none when the list is refused).
fn retrieval_queries(req: &Request) -> Vec<String> {
    if !req.list {
        return vec![req.queries.first().cloned().unwrap_or_default()];
    }
    let cleaned: Vec<String> = req
        .queries
        .iter()
        .map(|q| vex_extract::text::py_strip(q).to_string())
        .filter(|q| !q.is_empty())
        .collect();
    if cleaned.len() > MAX_QUERIES {
        return Vec::new();
    }
    cleaned
}

/// Cortex `warmup_note`: how much of the index has vectors, while some
/// chunks still have none.
fn warmup_note(idx: &Index) -> rusqlite::Result<Option<String>> {
    let pending = vex_store::vec::count_pending(&idx.conn)?;
    if pending == 0 {
        return Ok(None);
    }
    let count: i64 = idx
        .conn
        .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))?;
    let total = count.max(1);
    let pct = ((total - pending) as f64 / total as f64 * 100.0) as i64;
    Ok(Some(format!(
        "index {pct}% embedded \u{2014} semantic recall still improving; keyword matching covers everything already"
    )))
}

/// Say which of "nothing matched" and "the index is broken or empty" this is.
fn no_results(idx: &Index, root: &Path) -> rusqlite::Result<Answer> {
    let count = |sql: &str| -> rusqlite::Result<i64> { idx.conn.query_row(sql, [], |r| r.get(0)) };
    let chunks = count("SELECT COUNT(*) FROM chunks")?;
    let files = count("SELECT COUNT(*) FROM files")?;
    let root = root.display();
    Ok(Answer::Text(if chunks > 0 {
        format!(
            "No results, but the index is populated ({chunks} chunks over {files} files) — this is \
             an answer, not a failure. Retry with the vocabulary the code uses, or `grep` the \
             literal if it is text no symbol contains."
        )
    } else if files > 0 {
        format!(
            "INDEX BROKEN: 0 chunks, but {files} files are marked as indexed, so an incremental \
             `reindex` will do nothing. This needs a full rebuild the agent cannot do from here: \
             run `cortex index --force --root {root}` in a shell, or tell the user Cortex is \
             unusable for this repository. Do not silently fall back to reading files — say that \
             Cortex is broken."
        )
    } else {
        format!(
            "Index is empty (0 files, 0 chunks) at {root} — nothing has been indexed yet. Call \
             `reindex`."
        )
    }))
}

/// One single-query response, before serialisation.
struct Response {
    hits: Vec<Json>,
    ranking: Option<Json>,
    warmup: Option<String>,
    suppressed: Option<Json>,
    freshness: Option<Json>,
}

impl Response {
    fn payload(&self) -> Json {
        let mut pairs = vec![("results", Json::Arr(self.hits.clone()))];
        if let Some(r) = self.ranking.as_ref().filter(|r| r.truthy()) {
            pairs.push(("ranking", r.clone()));
        }
        if let Some(w) = &self.warmup {
            pairs.push(("warmup", Json::from(w.as_str())));
        }
        if let Some(s) = &self.suppressed {
            pairs.push(("suppressed", s.clone()));
        }
        if let Some(f) = &self.freshness {
            pairs.push(("freshness", f.clone()));
        }
        Json::obj(pairs)
    }
}

/// `token_est` of every parent a window references.
fn parent_sizes(idx: &Index, hits: &[Hit]) -> rusqlite::Result<HashMap<String, i64>> {
    let parents: Vec<String> = hits
        .iter()
        .filter(|h| !h.chunk.parent_id.is_empty())
        .map(|h| h.chunk.parent_id.clone())
        .collect();
    let mut out = HashMap::new();
    if parents.is_empty() {
        return Ok(out);
    }
    let marks = vec!["?"; parents.len()].join(",");
    let mut stmt = idx.conn.prepare(&format!(
        "SELECT id, token_est FROM chunks WHERE id IN ({marks})"
    ))?;
    let rows = stmt.query_map(rusqlite::params_from_iter(&parents), |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (id, t) = row?;
        out.insert(id, t);
    }
    Ok(out)
}

/// `symbols.split(" ")[0]`.
fn first_symbol(symbols: &str) -> &str {
    symbols.split(' ').next().unwrap_or("")
}

/// The note on a hit that is a window of a larger chunk, priced when the
/// parent's size is known.
fn window_note(c: &ChunkRow, parent_tokens: &HashMap<String, i64>) -> Option<Json> {
    if c.parent_id.is_empty() {
        return None;
    }
    let symbol = first_symbol(&c.symbols);
    let target = if symbol.is_empty() {
        c.parent_id.as_str()
    } else {
        symbol
    };
    let mut text = "you have the part that matched, not the whole chunk. Expand only if this span is not enough".to_string();
    let mut pairs = vec![
        ("reason", Json::from("symbol_window")),
        ("note", Json::Null),
        ("symbol", Json::from(target)),
        (
            "whole",
            Json::from(format!("expand(target={})", repr_str(target))),
        ),
    ];
    if let Some(&whole) = parent_tokens.get(&c.parent_id).filter(|t| **t != 0) {
        let here = tok(&c.content);
        pairs.push(("tokens_here", Json::from(here)));
        pairs.push(("tokens_whole", Json::from(whole)));
        if whole > here {
            let ratio = py_round(whole as f64 / here.max(1) as f64, 1);
            text.push_str(&format!(
                " -- it costs {} more tokens ({}x)",
                whole - here,
                pyjson::float_repr(ratio)
            ));
        }
    }
    pairs[1].1 = Json::from(text);
    Some(Json::obj(pairs))
}

/// The pointer on a body that was capped at index time.
fn body_recovery(c: &ChunkRow) -> Option<Json> {
    let stored = c.content.chars().count() as i64;
    if c.full_chars <= stored {
        return None;
    }
    let symbol = first_symbol(&c.symbols);
    let target = if symbol.is_empty() {
        c.id.as_str()
    } else {
        symbol
    };
    Some(recovery::note(
        "chunk_body_cap",
        (c.full_chars - stored) as usize,
        Json::from(recovery::CAP_CHUNK_CHARS),
        vec![format!("expand(target={})", repr_str(target))],
        Some(Json::obj(vec![
            ("unit", Json::from("characters")),
            ("stored_chars", Json::from(stored)),
            ("full_chars", Json::from(c.full_chars)),
        ])),
    ))
}

fn hit_payload(h: &Hit, parent_tokens: &HashMap<String, i64>) -> Json {
    let c = &h.chunk;
    let matched: Vec<String> = if h.channels.is_empty() {
        vec!["full_text".to_string()]
    } else {
        h.channels.clone()
    };
    let mut pairs = vec![
        ("file", Json::from(&c.file_path)),
        (
            "lines",
            Json::from(format!("{}-{}", c.start_line, c.end_line)),
        ),
        ("score", Json::from(h.score)),
        ("content", Json::from(&c.content)),
        ("matched_by", Json::from(matched)),
    ];
    if let Some(r) = body_recovery(c) {
        pairs.push(("truncated", Json::from(true)));
        pairs.push(("recovery", r));
    }
    if let Some(p) = window_note(c, parent_tokens) {
        pairs.push(("part_of", p));
    }
    Json::obj(pairs)
}

/// Keep the best hits whose serialised size fits the budget; one always.
fn within_budget(hits: Vec<Json>, max_bytes: i64) -> (Vec<Json>, usize) {
    if max_bytes <= 0 || hits.is_empty() {
        return (hits, 0);
    }
    let total = hits.len();
    let mut kept = Vec::new();
    let mut used = 0i64;
    for hit in hits {
        let size = pyjson::dumps(&hit, pyjson::DEFAULT).chars().count() as i64;
        if !kept.is_empty() && used + size > max_bytes {
            break;
        }
        kept.push(hit);
        used += size;
    }
    let withheld = total - kept.len();
    (kept, withheld)
}

/// What the scores mean, the top-two gap, and the missing-symbol verdict.
fn ranking_note(idx: &Index, query: &str, scores: &[f64]) -> rusqlite::Result<Option<Json>> {
    if scores.is_empty() {
        return Ok(None);
    }
    let mut note = Json::obj(vec![(
        "score",
        Json::from(
            "relative to the best hit in THIS response; 1.0 is the top result, not a confidence",
        ),
    )]);
    if scores.len() < 2 {
        note.set("top_gap", Json::Null);
        note.set(
            "gap_note",
            Json::from("only one result: nothing to compare the top hit against, so decisiveness is unmeasured"),
        );
    } else {
        let gap = py_round(scores[0] - scores[1], 3);
        note.set("top_gap", Json::from(gap));
        if gap < DECISIVE_GAP {
            note.set(
                "verdict",
                Json::from(format!(
                    "no clear winner: the top results scored within {} of each other, which means \
                     the ranking is ambiguous, not that nothing is relevant",
                    pyjson::float_repr(gap)
                )),
            );
        }
    }
    symbol_check(idx, query, &mut note)?;
    Ok(Some(note))
}

/// When every identifier the query named resolves to nothing, say so, or
/// name the indexed symbols it is part of.
fn symbol_check(idx: &Index, query: &str, note: &mut Json) -> rusqlite::Result<()> {
    let named = retriever::symbol_candidates(query);
    if named.is_empty() {
        return Ok(());
    }
    let mut missing = Vec::new();
    for t in &named {
        if crate::symbols::resolve(&idx.conn, t)?.is_empty() {
            missing.push(t.clone());
        }
    }
    if missing.is_empty() || missing.len() != named.len() {
        return Ok(());
    }
    let mut suggestions = Vec::new();
    let mut absent = Vec::new();
    for token in missing.iter().take(3) {
        let near = crate::symbols::near_symbols(&idx.conn, token, 3)?;
        if near.is_empty() {
            absent.push(token.clone());
        } else {
            suggestions.push(format!(
                "{} is part of {}",
                repr_str(token),
                near.join(", ")
            ));
        }
    }
    if !suggestions.is_empty() {
        note.set(
            "exact_symbol",
            Json::from(format!(
                "no symbol is named exactly that, but the name appears inside indexed symbols -- {}",
                suggestions.join("; ")
            )),
        );
    } else if !absent.is_empty() {
        let names: Vec<String> = absent.iter().map(|t| repr_str(t)).collect();
        note.set(
            "exact_symbol",
            Json::from(format!(
                "no symbol named {} exists in this index -- these are approximate matches",
                names.join(", ")
            )),
        );
    }
    Ok(())
}

/// The pointer for results the diversity caps held back.
fn suppression_note(d: &Diagnostics) -> Option<Json> {
    if d.suppressed_total == 0 {
        return None;
    }
    let mut reasons = Vec::new();
    if !d.capped_files.is_empty() {
        reasons.push(format!("per_file_cap({})", d.max_per_file));
    }
    if !d.capped_symbols.is_empty() {
        reasons.push(format!("per_symbol_cap({})", d.max_per_symbol));
    }
    let target = match (d.capped_files.first(), d.capped_symbols.first()) {
        (Some(f), _) => f.clone(),
        (None, Some(s)) => s.rsplit("::").next().unwrap_or("").to_string(),
        _ => String::new(),
    };
    let counts = |m: &indexmap::IndexMap<String, usize>| {
        Json::Obj(m.iter().map(|(k, v)| (k.clone(), Json::from(*v))).collect())
    };
    Some(Json::obj(vec![
        (
            "reason",
            Json::from(if reasons.is_empty() {
                "diversity_cap".to_string()
            } else {
                reasons.join("+")
            }),
        ),
        ("max_per_file", Json::from(d.max_per_file)),
        ("max_per_symbol", Json::from(d.max_per_symbol)),
        ("hidden", Json::from(d.suppressed_total)),
        ("files", counts(&d.suppressed)),
        ("symbols", counts(&d.suppressed_symbols)),
        (
            "recover",
            Json::from(format!(
                "More matches existed than the diversity caps allow, so the extras were held back \
                 to keep the slate broad. To see the rest, call search() scoped to '{target}', or \
                 expand() it directly -- the chunks are indexed and reachable without grep."
            )),
        ),
    ]))
}

/// A single query (Cortex `search_svc.search`), keyword-only until vectors.
fn single(
    idx: &Index,
    query: &str,
    top_k: i64,
    path: &str,
    max_bytes: i64,
    verdict: &freshness::Verdict,
    sem: Option<&Semantic>,
) -> rusqlite::Result<SingleResult> {
    let semantic = match sem.and_then(|s| s.vectors.get(query)) {
        Some(v) => vex_search::vector::search(idx, v, retriever::semantic_depth(top_k, path))
            .unwrap_or_default(),
        None => Vec::new(),
    };
    let (hits, diagnostics) = retriever::search(&idx.conn, semantic, query, top_k, path)?;
    let parents = parent_sizes(idx, &hits)?;
    let raw: Vec<Json> = hits.iter().map(|h| hit_payload(h, &parents)).collect();
    let (kept, over_budget) = within_budget(raw, max_bytes);
    let scores: Vec<f64> = hits.iter().take(kept.len()).map(|h| h.score).collect();
    let mut ranking = ranking_note(idx, query, &scores)?;
    if over_budget > 0 {
        let mut r = ranking.unwrap_or_else(|| Json::Obj(Vec::new()));
        r.set(
            "budget",
            recovery::note(
                "response_byte_budget",
                over_budget,
                Json::from(max_bytes),
                vec![format!(
                    "search(query={}, top_k={top_k}, max_bytes={})",
                    repr_str(query),
                    max_bytes * 2
                )],
                Some(Json::obj(vec![
                    ("unit", Json::from("results")),
                    (
                        "note",
                        Json::from(
                            "ranked but not returned, best-first: these are the next results, not \
                             worse matches for a different question",
                        ),
                    ),
                ])),
            ),
        );
        ranking = Some(r);
    }
    let response = Response {
        hits: kept,
        ranking,
        warmup: sem.and_then(|s| s.warmup.clone()),
        suppressed: suppression_note(&diagnostics),
        freshness: verdict.index_stale.then(|| verdict.to_json()),
    };
    Ok(SingleResult {
        hit_rows: hits,
        response,
    })
}

struct SingleResult {
    hit_rows: Vec<Hit>,
    response: Response,
}

impl std::ops::Deref for SingleResult {
    type Target = Response;
    fn deref(&self) -> &Response {
        &self.response
    }
}

/// `len(json.dumps(payload, ensure_ascii=False, separators=(",", ":")).encode())`.
fn size(payload: &Json) -> usize {
    pyjson::dumps(payload, pyjson::COMPACT_RAW).len()
}

fn next_action(has_records: bool, rankings: &[Option<Json>], queries: &[String]) -> String {
    if !has_records {
        return "no leads found: retry short identifier-oriented search queries; do not \
                broad-grep the repository"
            .to_string();
    }
    let mut base = "call expand exactly once with fetch_targets as its target list; do not \
                    search or read first. Search only if that batch reports a concrete missing \
                    evidence gap"
        .to_string();
    let unanswered: Vec<usize> = rankings
        .iter()
        .enumerate()
        .filter(|(_, r)| {
            r.as_ref()
                .and_then(|r| r.get("exact_symbol"))
                .is_some_and(Json::truthy)
        })
        .map(|(i, _)| i)
        .collect();
    if !unanswered.is_empty() {
        let named: Vec<String> = unanswered
            .iter()
            .take(3)
            .map(|&i| repr_str(&queries[i]))
            .collect();
        base.push_str(&format!(
            ". NOTE: {} named no symbol that exists in this index, so those leads are near \
             matches rather than the thing asked about -- see `ranking` before treating them as \
             evidence",
            named.join(", ")
        ));
    }
    base
}

/// A list query (Cortex `investigation.investigate`).
fn investigate(
    idx: &Index,
    req: &Request,
    verdict: &freshness::Verdict,
    clock: Clock,
    sem: Option<&Semantic>,
) -> rusqlite::Result<Answer> {
    let cleaned: Vec<String> = req
        .queries
        .iter()
        .map(|q| vex_extract::text::py_strip(q).to_string())
        .filter(|q| !q.is_empty())
        .collect();
    if cleaned.is_empty() {
        return Ok(cortex_error(
            "invalid_request",
            "'queries' must contain at least one non-empty string",
        ));
    }
    if cleaned.len() > MAX_QUERIES {
        return Ok(cortex_error(
            "invalid_request",
            &format!("'queries' supports at most {MAX_QUERIES} items"),
        ));
    }
    let top_k = req.top_k.clamp(1, MAX_TOP_K);
    let budget = if req.max_bytes == 0 {
        BUNDLE_BYTES
    } else {
        req.max_bytes
    };
    let max_bytes = budget.clamp(MIN_BYTES, MAX_BYTES);
    let started = clock();
    let discovery = top_k.max(DISCOVERY_TOP_K);

    let mut responses = Vec::with_capacity(cleaned.len());
    for q in &cleaned {
        responses.push(single(idx, q, discovery, "", 0, verdict, sem)?);
    }
    let per_query: Vec<&[Hit]> = responses
        .iter()
        .map(|r| &r.hit_rows[..r.hits.len()])
        .collect();
    let candidates: usize = per_query.iter().map(|h| h.len()).sum();

    struct Record {
        lead: Lead,
        symbol: String,
        lines: String,
    }
    let mut order: Vec<String> = Vec::new();
    let mut records: HashMap<String, Record> = HashMap::new();
    for rank in 0..discovery as usize {
        for (qi, hits) in per_query.iter().enumerate() {
            let Some(hit) = hits.get(rank) else { continue };
            let c = &hit.chunk;
            let site = crate::symbols::at_span(&idx.conn, &c.file_path, c.start_line, c.end_line)?;
            let target = match &site {
                Some(s) => s.qualified_name.clone(),
                None if !c.id.is_empty() => c.id.clone(),
                None => format!("file:{}", c.file_path),
            };
            let record = records.entry(target.clone()).or_insert_with(|| {
                order.push(target.clone());
                let preview: String = py_split(&c.content).collect::<Vec<_>>().join(" ");
                Record {
                    lead: Lead {
                        target: target.clone(),
                        name: site
                            .as_ref()
                            .map_or(c.file_path.clone(), |s| s.name.clone()),
                        kind: site.as_ref().map_or(
                            if c.id.is_empty() {
                                "file".into()
                            } else {
                                "chunk".into()
                            },
                            |s| s.kind.clone(),
                        ),
                        file: c.file_path.clone(),
                        matched_queries: Vec::new(),
                        score: hit.score,
                        source_available: site.is_some() || !c.id.is_empty(),
                        preview: take_chars(&preview, 180).to_string(),
                    },
                    symbol: site
                        .as_ref()
                        .map_or(String::new(), |s| s.qualified_name.clone()),
                    lines: site
                        .as_ref()
                        .map_or(format!("{}-{}", c.start_line, c.end_line), |s| {
                            format!("{}-{}", s.start_line, s.end_line)
                        }),
                }
            });
            if !record.lead.matched_queries.contains(&qi) {
                record.lead.matched_queries.push(qi);
            }
            if hit.score > record.lead.score {
                record.lead.score = hit.score;
            }
        }
    }
    let ordered: Vec<&Record> = order.iter().map(|t| &records[t]).collect();
    let leads: Vec<Lead> = ordered.iter().map(|r| r.lead.clone()).collect();

    let use_model = vex_extract::text::py_strip(&req.select).to_lowercase() == "model";
    let (selected, selector_block) = if leads.is_empty() {
        (
            Vec::new(),
            Json::obj(vec![
                ("provider", Json::from("none")),
                ("model", Json::from("none")),
                ("status", Json::from("empty")),
                ("selector_ms", Json::from(0usize)),
                ("fallback_reason", Json::from("no candidates")),
                ("assignments", Json::Arr(Vec::new())),
            ]),
        )
    } else {
        let t0 = clock();
        let (sel, mut block) = selector::select_leads(&cleaned, &leads, use_model, 0);
        block.set("selector_ms", Json::from((clock() - t0) as i64));
        (sel, block)
    };

    let lead_json = |r: &Record| -> Json {
        let l = &r.lead;
        Json::obj(vec![
            ("target", Json::from(&l.target)),
            ("symbol", Json::from(&r.symbol)),
            ("name", Json::from(&l.name)),
            ("kind", Json::from(&l.kind)),
            ("file", Json::from(&l.file)),
            ("lines", Json::from(&r.lines)),
            ("matched_queries", Json::from(l.matched_queries.clone())),
            ("score", Json::from(l.score)),
            ("source_available", Json::from(l.source_available)),
        ])
    };
    let rankings: Vec<Option<Json>> = responses.iter().map(|r| r.ranking.clone()).collect();
    let index_version: String = idx
        .conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'index_version'",
            [],
            |r| r.get(0),
        )
        .unwrap_or_default();
    let mut payload = Json::obj(vec![
        ("queries", Json::from(cleaned.clone())),
        ("leads", Json::Arr(Vec::new())),
        ("fetch_targets", Json::Arr(Vec::new())),
        ("truncated", Json::from(false)),
        (
            "limits",
            Json::obj(vec![
                ("queries", Json::from(MAX_QUERIES)),
                ("top_k", Json::from(top_k)),
                ("discovery_top_k", Json::from(discovery)),
                ("max_bytes", Json::from(max_bytes)),
                ("max_leads", Json::from(MAX_LEADS)),
                (
                    "max_selected_leads",
                    Json::from(selector::MAX_SELECTED_LEADS),
                ),
            ]),
        ),
        (
            "stats",
            Json::obj(vec![
                ("candidates", Json::from(candidates)),
                ("deduplicated_leads", Json::from(leads.len())),
                ("selected_leads", Json::from(selected.len())),
                ("included_leads", Json::from(0usize)),
                ("response_bytes", Json::from(0usize)),
            ]),
        ),
        ("selector", selector_block),
        (
            "ranking",
            Json::Arr(
                rankings
                    .iter()
                    .map(|r| r.clone().unwrap_or(Json::Null))
                    .collect(),
            ),
        ),
        // The first question's warm-up note (they all share one index).
        (
            "warming",
            responses
                .iter()
                .find_map(|r| r.warmup.as_deref())
                .map_or(Json::Null, Json::from),
        ),
        (
            "next_action",
            Json::from(next_action(!leads.is_empty(), &rankings, &cleaned)),
        ),
        ("index_version", Json::from(index_version)),
        ("took_ms", Json::from(0usize)),
    ]);

    let mut included: Vec<Json> = Vec::new();
    let mut truncated = false;
    for &i in &selected {
        if included.len() >= MAX_LEADS {
            truncated = true;
            break;
        }
        let record = lead_json(ordered[i]);
        let mut trial_leads = included.clone();
        trial_leads.push(record.clone());
        let mut trial = payload.clone();
        let targets: Vec<Json> = trial_leads
            .iter()
            .filter_map(|l| l.get("target").cloned())
            .collect();
        trial.set("leads", Json::Arr(trial_leads));
        trial.set("fetch_targets", Json::Arr(targets));
        if size(&trial) as i64 > max_bytes {
            truncated = true;
            continue;
        }
        included.push(record);
    }
    let targets: Vec<Json> = included
        .iter()
        .filter_map(|l| l.get("target").cloned())
        .collect();
    let n = included.len();
    payload.set("leads", Json::Arr(included));
    payload.set("fetch_targets", Json::Arr(targets));
    payload.set("truncated", Json::from(truncated));
    if let Some(Json::Obj(stats)) = payload_get_mut(&mut payload, "stats") {
        stats[3].1 = Json::from(n);
    }
    payload.set("took_ms", Json::from((clock() - started) as i64));
    let bytes = size(&payload);
    if let Some(Json::Obj(stats)) = payload_get_mut(&mut payload, "stats") {
        stats[4].1 = Json::from(bytes);
    }
    Ok(Answer::Payload(payload))
}

fn payload_get_mut<'a>(payload: &'a mut Json, key: &str) -> Option<&'a mut Json> {
    match payload {
        Json::Obj(pairs) => pairs.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v),
        _ => None,
    }
}
