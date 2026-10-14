//! `vex context` and `vex impact` (Cortex `application/symbols.py`
//! context_view and structure view, `application/graph.py` impact,
//! `application/views.py` neighborhood_view, and the MCP tools over them).
//!
//! context: per matching symbol (at most three), its incoming and outgoing
//! edges, own and its file's, with name-match CALLS below the confidence
//! floor held back and at most 25 per direction kept (strongest first),
//! both counted in one recovery pointer; and its own source at a detail
//! level. `detail="structure"` instead re-parses the file and lists the
//! control skeleton of the symbol's body, or abstains by name.
//!
//! impact: upstream or downstream, a breadth-first walk from the symbol and
//! from its file, depth-attenuated confidences, grouped into direct and
//! transitive. `both` walks the neighbourhood of up to six roots inside a
//! 24-node, 48-edge envelope.
//!
//! Edge rows come back in the order SQLite returns them for the same
//! queries on the same schema, which is what Cortex reports.

use crate::expand::{Refusal, contained_read, detail_refusal};
use crate::freshness;
use crate::pyjson::{Json, py_round, repr_str};
use crate::recovery;
use crate::search::Answer;
use crate::symbols::{self, Symbol};
use indexmap::IndexMap;
use rusqlite::{Connection, OptionalExtension};
use std::path::Path;
use vex_store::db::Index;

const CONFIDENCE_KEY: &str = "0.95 import-bound, 0.9 same-file, 0.7 class-method, \
                              <=0.6 name-match only — verify low-confidence hits";
/// The floor applies to CALLS only: REFS runs a different confidence scale.
const CONFIDENCE_SCALED_EDGE_TYPES: [&str; 1] = ["CALLS"];
const CONTEXT_MATCHES: usize = 3;
const NEIGHBORHOOD_ROOTS: usize = 6;
const NEIGHBORHOOD_MAX_DEPTH: i64 = 3;
const NEIGHBORHOOD_NODES: usize = 24;
const NEIGHBORHOOD_EDGES: usize = 48;
const NODE_KINDS: [&str; 6] = ["function", "method", "class", "module", "file", "external"];
const BLOCK_OPENERS: [&str; 14] = [
    "if_statement",
    "else_clause",
    "for_statement",
    "for_in_statement",
    "while_statement",
    "do_statement",
    "try_statement",
    "catch_clause",
    "finally_clause",
    "switch_statement",
    "switch_case",
    "switch_default",
    "with_statement",
    "labeled_statement",
];

/// The answer plus the freshness verdict (for the CLI's exit code).
#[derive(Debug, Clone)]
pub struct Outcome {
    pub answer: Answer,
    pub freshness: freshness::Verdict,
}

impl Outcome {
    /// The CLI's `--json` document, with a `freshness` block when stale.
    pub fn cli_json(&self) -> Json {
        let mut doc = self.answer.cli_json();
        if self.freshness.index_stale {
            doc.set("freshness", self.freshness.to_json());
        }
        doc
    }
}

fn open(
    home: &Path,
    start: &Path,
) -> Result<(Index, std::path::PathBuf, freshness::Verdict), crate::project::ProjectError> {
    let (id, root) = crate::project::registered(home, start)?;
    let idx = Index::open(&vex_store::registry::index_path(home, &id))?;
    let verdict = freshness::check(&idx, &root);
    Ok((idx, root, verdict))
}

/// `vex context` in the registered project containing `start`.
pub fn run_context(
    home: &Path,
    start: &Path,
    symbol: &str,
    detail: &str,
    confidence_min: f64,
) -> Result<Outcome, crate::project::ProjectError> {
    let (idx, root, freshness) = open(home, start)?;
    let answer = context(&idx.conn, &root, symbol, detail, confidence_min)?;
    Ok(Outcome { answer, freshness })
}

/// An impact request: one root or a list of them.
#[derive(Debug, Clone)]
pub struct ImpactRequest {
    pub roots: Vec<String>,
    pub list: bool,
    pub direction: String,
    pub depth: i64,
    pub confidence_min: f64,
}

/// `vex impact` in the registered project containing `start`.
pub fn run_impact(
    home: &Path,
    start: &Path,
    req: &ImpactRequest,
) -> Result<Outcome, crate::project::ProjectError> {
    let (idx, root, freshness) = open(home, start)?;
    let _ = root;
    let answer = impact(&idx.conn, req)?;
    Ok(Outcome { answer, freshness })
}

fn refused(r: Refusal) -> Answer {
    Answer::Error {
        code: r.code.to_string(),
        message: r.message,
    }
}

fn not_found(message: String) -> Answer {
    refused(Refusal::new("not_found", message))
}

/// One edge row: `(other, type, confidence, other_file)`.
#[derive(Debug, Clone)]
struct Edge {
    other: String,
    kind: String,
    confidence: f64,
    file: String,
}

impl Edge {
    fn to_json(&self) -> Json {
        Json::Arr(vec![
            Json::from(&self.other),
            Json::from(&self.kind),
            Json::from(self.confidence),
            Json::from(&self.file),
        ])
    }
}

/// `GraphStore.neighbors`: (incoming, outgoing).
fn neighbors(conn: &Connection, qualified_name: &str) -> rusqlite::Result<(Vec<Edge>, Vec<Edge>)> {
    let rows = |sql: &str| -> rusqlite::Result<Vec<Edge>> {
        conn.prepare_cached(sql)?
            .query_map([qualified_name], |r| {
                Ok(Edge {
                    other: r.get(0)?,
                    kind: r.get(1)?,
                    confidence: r.get(2)?,
                    file: r.get(3)?,
                })
            })?
            .collect()
    };
    Ok((
        rows("SELECT src, type, confidence, src_file FROM edges WHERE dst = ?")?,
        rows("SELECT dst, type, confidence, dst_file FROM edges WHERE src = ?")?,
    ))
}

/// A name-match guess weak enough to hold back.
fn below_floor(kind: &str, confidence: f64, floor: f64) -> bool {
    floor > 0.0
        && CONFIDENCE_SCALED_EDGE_TYPES.contains(&kind.to_uppercase().as_str())
        && confidence < floor
}

/// Split rows on the floor: (kept, dropped).
fn above(rows: Vec<Edge>, floor: f64) -> (Vec<Edge>, usize) {
    if floor <= 0.0 {
        return (rows, 0);
    }
    let n = rows.len();
    let kept: Vec<Edge> = rows
        .into_iter()
        .filter(|e| !below_floor(&e.kind, e.confidence, floor))
        .collect();
    let dropped = n - kept.len();
    (kept, dropped)
}

/// Keep the strongest edges (stable on ties) and say how many were left.
fn top_edges(mut rows: Vec<Edge>) -> (Vec<Edge>, usize) {
    let cap = recovery::CAP_CONTEXT_EDGES;
    if rows.len() <= cap {
        return (rows, 0);
    }
    rows.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let over = rows.len() - cap;
    rows.truncate(cap);
    (rows, over)
}

fn context_recovery(symbol: &str, floor: f64, below: usize, over: usize) -> Option<Json> {
    if below == 0 && over == 0 {
        return None;
    }
    let mut next = vec![format!(
        "impact(symbol={}, direction='both', depth=1)",
        repr_str(symbol)
    )];
    if below > 0 {
        next.push(format!(
            "context(symbol={}, confidence_min=0.0)",
            repr_str(symbol)
        ));
    }
    Some(recovery::note(
        "context_edges_withheld",
        below + over,
        Json::from(recovery::CAP_CONTEXT_EDGES),
        next,
        Some(Json::obj(vec![
            ("below_confidence_floor", Json::from(below)),
            ("over_edge_cap", Json::from(over)),
            ("floor", Json::from(floor)),
            (
                "meaning",
                Json::from(
                    "below the floor an edge is a name match with no receiver type; \
                     over the cap it is a real edge that did not fit, strongest kept",
                ),
            ),
        ])),
    ))
}

/// What the `context` MCP tool answers.
pub fn context(
    conn: &Connection,
    root: &Path,
    symbol: &str,
    detail: &str,
    confidence_min: f64,
) -> rusqlite::Result<Answer> {
    if detail == "structure" {
        return structure_view(conn, root, symbol);
    }
    if let Some(r) = detail_refusal(detail) {
        return Ok(refused(r));
    }
    let matches = symbols::resolve(conn, symbol)?;
    if matches.is_empty() {
        return Ok(not_found(format!(
            "Symbol '{symbol}' not found. Try search() or reindex."
        )));
    }
    let floor = confidence_min.clamp(0.0, 1.0);
    let mut views = Vec::new();
    for s in matches.iter().take(CONTEXT_MATCHES) {
        let (mut incoming, mut outgoing) = neighbors(conn, &s.qualified_name)?;
        let (file_in, file_out) = neighbors(conn, &format!("file:{}", s.file_path))?;
        let snippets = match own_source(root, s, detail) {
            Ok(snippets) => snippets,
            Err(r) => return Ok(refused(r)),
        };
        incoming.extend(file_in);
        outgoing.extend(file_out);
        let (incoming, dropped_in) = above(incoming, floor);
        let (outgoing, dropped_out) = above(outgoing, floor);
        let (incoming, capped_in) = top_edges(incoming);
        let (outgoing, capped_out) = top_edges(outgoing);
        let mut view = vec![
            ("symbol", Json::from(&s.qualified_name)),
            ("kind", Json::from(&s.kind)),
            ("file", Json::from(&s.file_path)),
            (
                "lines",
                Json::from(format!("{}-{}", s.start_line, s.end_line)),
            ),
            (
                "incoming",
                Json::Arr(incoming.iter().map(Edge::to_json).collect()),
            ),
            (
                "outgoing",
                Json::Arr(outgoing.iter().map(Edge::to_json).collect()),
            ),
            ("snippets", Json::from(snippets)),
        ];
        if let Some(note) = context_recovery(
            &s.qualified_name,
            floor,
            dropped_in + dropped_out,
            capped_in + capped_out,
        ) {
            view.push(("recovery", note));
        }
        views.push(Json::obj(view));
    }
    Ok(Answer::Payload(Json::Arr(views)))
}

/// The symbol's own source, as a one-element list (none when empty).
fn own_source(root: &Path, s: &Symbol, detail: &str) -> Result<Vec<String>, Refusal> {
    let lines = contained_read(root, &s.file_path)?;
    if lines.is_empty() {
        return Ok(Vec::new());
    }
    let n = lines.len() as i64;
    let clamp = |i: i64| -> usize { (if i < 0 { (i + n).max(0) } else { i.min(n) }) as usize };
    let (a, b) = (clamp(s.start_line - 1), clamp(s.end_line));
    let span = if a >= b { &[][..] } else { &lines[a..b] };
    let (source, _exact) = crate::expand::apply_detail(span, detail);
    Ok(if source.is_empty() {
        Vec::new()
    } else {
        vec![source]
    })
}

// --- structure ---------------------------------------------------------------

fn structure_view(conn: &Connection, root: &Path, symbol: &str) -> rusqlite::Result<Answer> {
    let Some(s) = symbols::resolve(conn, symbol)?.into_iter().next() else {
        return Ok(not_found(format!(
            "Symbol '{symbol}' not found. Try search() or reindex."
        )));
    };
    let mut view = structure_of_file(root, &s.file_path, s.start_line, s.end_line);
    view.set("symbol", Json::from(&s.qualified_name));
    Ok(Answer::Payload(view))
}

/// The control skeleton of a file (narrowed to the definition spanning
/// `start..=end`), or an explicit abstention.
fn structure_of_file(root: &Path, rel_path: &str, start: i64, end: i64) -> Json {
    let path = root.join(rel_path);
    let head = read_head(&path);
    let language = vex_extract::language::language_of(rel_path, head.as_deref());
    let abstain = |reason: String| {
        Json::obj(vec![
            ("abstained", Json::from(true)),
            ("file", Json::from(rel_path)),
            ("language", Json::from(language)),
            ("reason", Json::from(reason)),
            ("statements", Json::Arr(Vec::new())),
        ])
    };
    let no_grammar = || {
        if language.is_empty() {
            abstain(format!("unknown language for {rel_path}"))
        } else {
            abstain(format!(
                "no grammar available for {language}; cortex cannot see the structure of this file"
            ))
        }
    };
    let Some(grammar) = vex_extract::grammars::language(language) else {
        return no_grammar();
    };
    let Ok(source) = std::fs::read(&path) else {
        return no_grammar();
    };
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&grammar).is_err() {
        return no_grammar();
    }
    let Some(tree) = parser.parse(&source, None) else {
        return no_grammar();
    };
    let node = node_covering(&tree, start, end).unwrap_or_else(|| tree.root_node());
    let mut statements = Vec::new();
    walk_statements(body_of(node), 0, None, &mut statements);
    Json::obj(vec![
        ("abstained", Json::from(false)),
        ("file", Json::from(rel_path)),
        ("language", Json::from(language)),
        ("statements", Json::Arr(statements)),
    ])
}

fn read_head(path: &Path) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(vex_extract::language::HEAD_BYTES as u64)
        .read_to_end(&mut buf)
        .ok()?;
    Some(buf)
}

fn named_children(node: tree_sitter::Node<'_>) -> Vec<tree_sitter::Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

fn line(node: tree_sitter::Node<'_>) -> i64 {
    node.start_position().row as i64 + 1
}

/// Statements in source order with the construct that opens their block.
fn walk_statements(
    node: tree_sitter::Node<'_>,
    depth: i64,
    opened_by: Option<&'static str>,
    out: &mut Vec<Json>,
) {
    for child in named_children(node) {
        let kind = child.kind();
        if kind == "statement_block" || kind == "block" {
            walk_statements(child, depth, opened_by, out);
            continue;
        }
        let entry = |out: &mut Vec<Json>| {
            out.push(Json::obj(vec![
                ("line", Json::from(line(child))),
                ("depth", Json::from(depth)),
                ("opened_by", Json::from(opened_by)),
                ("type", Json::from(kind)),
            ]));
        };
        if let Some(opener) = BLOCK_OPENERS.iter().find(|o| **o == kind) {
            entry(out);
            walk_statements(child, depth + 1, Some(opener), out);
            continue;
        }
        if kind.ends_with("_statement") || kind.ends_with("_declaration") {
            entry(out);
            walk_statements(child, depth, opened_by, out);
        }
    }
}

/// The block a definition's statements live in (through export wrappers).
fn body_of(node: tree_sitter::Node<'_>) -> tree_sitter::Node<'_> {
    for field in ["body", "consequence"] {
        if let Some(b) = node.child_by_field_name(field) {
            return b;
        }
    }
    for child in named_children(node) {
        for field in ["body", "consequence"] {
            if let Some(b) = child.child_by_field_name(field) {
                return b;
            }
        }
    }
    node
}

/// The smallest node spanning a recorded symbol's lines (the first found,
/// in Cortex's stack order, among equals).
fn node_covering(tree: &tree_sitter::Tree, start: i64, end: i64) -> Option<tree_sitter::Node<'_>> {
    let mut best: Option<tree_sitter::Node<'_>> = None;
    let mut stack = vec![tree.root_node()];
    let rows =
        |n: &tree_sitter::Node<'_>| n.end_position().row as i64 - n.start_position().row as i64;
    while let Some(n) = stack.pop() {
        if line(n) <= start && n.end_position().row as i64 + 1 >= end {
            if best.as_ref().is_none_or(|b| rows(&n) < rows(b)) {
                best = Some(n);
            }
            stack.extend(named_children(n));
        }
    }
    best
}

// --- impact ------------------------------------------------------------------

/// What the `impact` MCP tool answers.
pub fn impact(conn: &Connection, req: &ImpactRequest) -> rusqlite::Result<Answer> {
    if req.direction == "both" {
        let roots: Vec<String> = req.roots.iter().take(NEIGHBORHOOD_ROOTS).cloned().collect();
        return Ok(Answer::Payload(neighborhood(
            conn,
            &roots,
            req.depth.clamp(1, NEIGHBORHOOD_MAX_DEPTH),
            req.confidence_min.clamp(0.0, 1.0),
        )?));
    }
    let mut reports = Vec::new();
    for root in &req.roots {
        match impact_report(conn, root, &req.direction, req.depth)? {
            Ok(report) => reports.push(report),
            Err(message) if !req.list => return Ok(not_found(message)),
            Err(message) => reports.push(Json::obj(vec![
                ("symbol", Json::from(root)),
                ("error", Json::from(message)),
            ])),
        }
    }
    if req.list {
        Ok(Answer::Payload(Json::Arr(reports)))
    } else {
        Ok(Answer::Payload(
            reports.into_iter().next().unwrap_or(Json::Null),
        ))
    }
}

/// `application/graph.impact`: the blast radius of the symbol and of its
/// file, grouped by depth. `Err` carries the not-found message.
fn impact_report(
    conn: &Connection,
    symbol: &str,
    direction: &str,
    max_depth: i64,
) -> rusqlite::Result<Result<Json, String>> {
    let Some(s) = symbols::resolve(conn, symbol)?.into_iter().next() else {
        return Ok(Err(format!("Symbol '{symbol}' not found.")));
    };
    let mut hits = bfs(conn, &s.qualified_name, direction, max_depth)?;
    hits.extend(bfs(
        conn,
        &format!("file:{}", s.file_path),
        direction,
        max_depth,
    )?);
    let mut by_depth: IndexMap<i64, Vec<Json>> = IndexMap::new();
    for (depth, hit) in hits {
        by_depth.entry(depth).or_default().push(hit);
    }
    let direct = by_depth.get(&1).cloned().unwrap_or_default();
    let transitive: Vec<Json> = by_depth
        .iter()
        .filter(|(d, _)| **d > 1)
        .flat_map(|(_, hs)| hs.iter().cloned())
        .collect();
    Ok(Ok(Json::obj(vec![
        ("target", Json::from(&s.qualified_name)),
        ("direction", Json::from(direction)),
        ("depth_1_will_break", Json::Arr(direct)),
        ("transitive", Json::Arr(transitive)),
        ("confidence_key", Json::from(CONFIDENCE_KEY)),
    ])))
}

/// `GraphStore.impact`: breadth-first, each node once, confidence
/// attenuated by 0.9 per hop past the first.
pub(crate) fn bfs(
    conn: &Connection,
    start: &str,
    direction: &str,
    max_depth: i64,
) -> rusqlite::Result<Vec<(i64, Json)>> {
    let (col_from, col_to) = if direction == "upstream" {
        ("dst", "src")
    } else {
        ("src", "dst")
    };
    let mut seen = std::collections::HashSet::from([start.to_string()]);
    let mut frontier = vec![start.to_string()];
    let mut out = Vec::new();
    for depth in 1..=max_depth {
        if frontier.is_empty() {
            break;
        }
        let placeholders = vec!["?"; frontier.len()].join(",");
        let sql = format!(
            "SELECT {col_to}, type, confidence FROM edges WHERE {col_from} IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(frontier.iter()), |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, f64>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut next = Vec::new();
        for (node, kind, confidence) in rows {
            if !seen.insert(node.clone()) {
                continue;
            }
            next.push(node.clone());
            let attenuated = py_round(confidence * 0.9f64.powf((depth - 1) as f64), 2);
            out.push((
                depth,
                Json::obj(vec![
                    ("symbol", Json::from(node)),
                    ("depth", Json::from(depth)),
                    ("edge", Json::from(kind)),
                    ("confidence", Json::from(attenuated)),
                ]),
            ));
        }
        frontier = next;
    }
    Ok(out)
}

/// `views._kind`.
fn node_kind(raw: Option<&str>, node_id: &str) -> String {
    if node_id.starts_with("file:") {
        return "file".into();
    }
    let k = raw.unwrap_or("").to_lowercase();
    if NODE_KINDS.contains(&k.as_str()) {
        k
    } else {
        "function".into()
    }
}

/// `views._node_for`.
fn node_for(conn: &Connection, node_id: &str, depth: i64, state: &str) -> rusqlite::Result<Json> {
    let tail = node_id.rsplit(':').next().unwrap_or("");
    let label = tail.rsplit('.').next().unwrap_or("");
    let label = if label.is_empty() { node_id } else { label };
    let mut node = Json::obj(vec![
        ("id", Json::from(node_id)),
        ("label", Json::from(label)),
        ("kind", Json::from(node_kind(None, node_id))),
        ("depth", Json::from(depth)),
        ("state", Json::from(state)),
    ]);
    let row = conn
        .prepare_cached(
            "SELECT kind, file_path, start_line, end_line FROM symbols \
             WHERE qualified_name = ? LIMIT 1",
        )?
        .query_row([node_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })
        .optional()?;
    if let Some((kind, file, start, end)) = row {
        node.set("kind", Json::from(node_kind(Some(&kind), node_id)));
        node.set("file", Json::from(file));
        node.set("start_line", Json::from(start));
        node.set("end_line", Json::from(end));
    } else if let Some(file) = node_id.strip_prefix("file:") {
        node.set("file", Json::from(file));
    }
    Ok(node)
}

/// `views.neighborhood_view` with the MCP tool's envelope.
fn neighborhood(
    conn: &Connection,
    roots: &[String],
    depth: i64,
    confidence_min: f64,
) -> rusqlite::Result<Json> {
    let (max_nodes, max_edges) = (NEIGHBORHOOD_NODES, NEIGHBORHOOD_EDGES);
    let mut resolved = Vec::new();
    for r in roots {
        let first = symbols::resolve(conn, r)?.into_iter().next();
        resolved.push(first.map_or_else(|| r.clone(), |s| s.qualified_name));
    }
    let mut nodes: IndexMap<String, Json> = IndexMap::new();
    let mut edges: IndexMap<String, Json> = IndexMap::new();
    let mut warnings = Vec::new();
    let mut truncated = false;
    let mut below = 0usize;
    let mut frontier: Vec<String> = Vec::new();
    for r in &resolved {
        if !frontier.contains(r) {
            frontier.push(r.clone());
        }
    }
    for rid in &frontier {
        nodes.insert(rid.clone(), node_for(conn, rid, 0, "match")?);
    }
    for d in 1..=depth.max(1) {
        let mut next = Vec::new();
        for node_id in &frontier {
            let (incoming, outgoing) = neighbors(conn, node_id)?;
            for (incoming_side, rows) in [(true, incoming), (false, outgoing)] {
                for e in rows {
                    if below_floor(&e.kind, e.confidence, confidence_min) {
                        below += 1;
                        continue;
                    }
                    if nodes.len() >= max_nodes || edges.len() >= max_edges {
                        truncated = true;
                        break;
                    }
                    if !nodes.contains_key(&e.other) {
                        nodes.insert(e.other.clone(), node_for(conn, &e.other, d, "normal")?);
                        next.push(e.other.clone());
                    }
                    let (src, dst) = if incoming_side {
                        (e.other.as_str(), node_id.as_str())
                    } else {
                        (node_id.as_str(), e.other.as_str())
                    };
                    let kind = if e.kind.is_empty() {
                        "refs".to_uppercase()
                    } else {
                        e.kind.to_uppercase()
                    };
                    let eid = format!("{src}->{dst}:{}", e.kind);
                    edges.insert(
                        eid.clone(),
                        Json::obj(vec![
                            ("id", Json::from(eid)),
                            ("source", Json::from(src)),
                            ("target", Json::from(dst)),
                            ("type", Json::from(kind)),
                            ("confidence", Json::from(e.confidence)),
                        ]),
                    );
                }
            }
        }
        frontier = next;
        if truncated {
            warnings.push("graph truncated at server budget");
            break;
        }
    }
    let index_version: String = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'index_version'",
            [],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or_default();
    let mut envelope = Json::obj(vec![
        ("nodes", Json::Arr(nodes.values().cloned().collect())),
        ("edges", Json::Arr(edges.values().cloned().collect())),
        ("roots", Json::from(resolved.clone())),
        ("truncated", Json::from(truncated)),
        (
            "limits",
            Json::obj(vec![
                ("nodes", Json::from(max_nodes)),
                ("edges", Json::from(max_edges)),
                ("depth", Json::from(depth)),
            ]),
        ),
        ("warnings", Json::from(warnings)),
        ("index_version", Json::from(index_version)),
    ]);
    if below > 0 {
        let root = resolved.first().map(String::as_str).unwrap_or("");
        envelope.set(
            "withheld",
            Json::obj(vec![("low_confidence", Json::from(below))]),
        );
        envelope.set(
            "confidence_recovery",
            confidence_note(below, root, confidence_min),
        );
    }
    if truncated {
        let hit_nodes = nodes.len() >= max_nodes;
        envelope.set(
            "recovery",
            graph_note(frontier.len().max(1), &resolved, depth, hit_nodes),
        );
    }
    Ok(envelope)
}

/// `recovery.confidence_note`.
fn confidence_note(withheld: usize, symbol: &str, floor: f64) -> Json {
    recovery::note(
        "graph_confidence_floor",
        withheld,
        Json::from(floor),
        vec![
            format!("context(symbol={}, confidence_min=0.0)", repr_str(symbol)),
            format!(
                "impact(symbol={}, direction='both', confidence_min=0.0)",
                repr_str(symbol)
            ),
        ],
        Some(Json::obj(vec![
            ("floor", Json::from(floor)),
            (
                "meaning",
                Json::from(
                    "below this an edge is a name match with no receiver type -- \
                     verify before trusting it",
                ),
            ),
        ])),
    )
}

/// `recovery.graph_note`: the limit quoted is the global cap, as Cortex
/// quotes it, not the envelope's.
fn graph_note(withheld: usize, roots: &[String], depth: i64, hit_nodes: bool) -> Json {
    let (limit, hit) = if hit_nodes {
        (recovery::CAP_GRAPH_NODES, "node")
    } else {
        (recovery::CAP_GRAPH_EDGES, "edge")
    };
    let root = roots.first().map(String::as_str).unwrap_or("<symbol>");
    recovery::note(
        &format!("graph_{hit}_cap"),
        withheld,
        Json::from(limit),
        vec![
            format!("traverse(roots=['{root}'], depth=1)  # narrow the frontier"),
            format!("impact(symbol='{root}', max_depth=1)  # direct edges only"),
            format!("context(symbol='{root}')  # single-symbol neighborhood"),
        ],
        Some(Json::obj(vec![
            ("depth_requested", Json::from(depth)),
            (
                "roots",
                Json::from(roots.iter().take(3).cloned().collect::<Vec<_>>()),
            ),
        ])),
    )
}
