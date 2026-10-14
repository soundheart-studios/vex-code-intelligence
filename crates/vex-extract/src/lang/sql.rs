//! SQL: Cortex's `languages/sql.py`. CREATE TABLE/VIEW/FUNCTION/
//! PROCEDURE/TRIGGER are definitions (recovered from ERROR nodes when the
//! grammar could not parse the body), and FROM/JOIN/REFERENCES targets are
//! relation refs that survive the own-definition filter.

use crate::ast::{children, named_children, text};
use crate::lang::adapter::{chunk, drop_own, lines_of};
use crate::text::{decode_replace, py_strip};
use indexmap::IndexMap;
use regex::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;
use tree_sitter::Node;
use vex_core::{Chunk, Parsed, Symbol};

const MAX_CHUNK_LINES: usize = 120;

static BARE_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_][\w$]*(?:\.[A-Za-z_][\w$]*)*$").expect("valid regex"));

fn definition(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "create_table" => "table",
        "create_view" => "view",
        "create_function" => "function",
        "create_procedure" => "procedure",
        "create_trigger" => "trigger",
        _ => return None,
    })
}

/// The object a standard CREATE header names (a dialect word such as
/// `CREATE PERFETTO TABLE` matches none, so it is not guessed at).
fn object_keyword(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "keyword_table" => "table",
        "keyword_view" => "view",
        "keyword_function" => "function",
        "keyword_procedure" => "procedure",
        "keyword_trigger" => "trigger",
        _ => return None,
    })
}

fn is_header_modifier(kind: &str) -> bool {
    matches!(
        kind,
        "keyword_or"
            | "keyword_replace"
            | "keyword_temporary"
            | "keyword_temp"
            | "keyword_materialized"
            | "keyword_unique"
            | "keyword_recursive"
    )
}

struct Sql<'a> {
    rel: &'a str,
    src: &'a [u8],
    qual_prefix: String,
    lines: Vec<String>,
    chunks: Vec<Chunk>,
    symbols: Vec<Symbol>,
    refs: IndexMap<String, u64>,
    relation_refs: IndexMap<String, u64>,
    seen: HashSet<(String, usize)>,
}

impl Sql<'_> {
    fn t(&self, node: Node) -> String {
        text(node, self.src)
    }

    /// The first object_reference under `node`, document order.
    fn object_name(&self, node: Node) -> String {
        let mut stack: Vec<Node> = named_children(node).into_iter().rev().collect();
        while let Some(child) = stack.pop() {
            if child.kind() == "object_reference" {
                return self.t(child);
            }
            stack.extend(named_children(child).into_iter().rev());
        }
        String::new()
    }

    /// Every site is its own symbol (ADR 19).
    fn add_symbol(&mut self, name: &str, kind: &str, start: usize, end: usize) {
        let qn = format!("{}.{name}", self.qual_prefix);
        if self.seen.insert((qn.clone(), start)) {
            self.symbols.push(Symbol {
                qualified_name: qn,
                name: name.to_string(),
                kind: kind.to_string(),
                file_path: self.rel.to_string(),
                start_line: start + 1,
                end_line: end + 1,
                declaration: false,
            });
        }
    }

    fn add_chunk(&mut self, rows: (usize, usize), kind: &str, name: &str) {
        if self.lines.is_empty() {
            return;
        }
        let (start, end) = rows;
        let end = end.min(start + MAX_CHUNK_LINES).min(self.lines.len() - 1);
        if let Some(c) = chunk(self.rel, &self.lines, start, end, kind, "sql", Some(name)) {
            self.chunks.push(c);
        }
    }

    fn relation(&mut self, name: String) {
        *self.refs.entry(name.clone()).or_insert(0) += 1;
        *self.relation_refs.entry(name).or_insert(0) += 1;
    }

    fn collect_relation_refs(&mut self, node: Node) {
        let mut stack = vec![node];
        while let Some(node) = stack.pop() {
            if matches!(node.kind(), "from" | "join" | "relation") {
                let name = self.object_name(node);
                if !name.is_empty() {
                    self.relation(name);
                }
            }
            let kids = named_children(node);
            if matches!(
                node.kind(),
                "column_definition" | "constraint" | "add_constraint"
            ) && let Some(i) = kids.iter().position(|c| c.kind() == "keyword_references")
                && let Some(target) = kids[i + 1..]
                    .iter()
                    .find(|c| c.kind() == "object_reference")
            {
                let name = self.t(*target);
                self.relation(name);
            }
            stack.extend(kids.into_iter().rev());
        }
    }

    /// Standard CREATE headers among an ERROR's own tokens; each recovered
    /// statement spans to the token before the next CREATE.
    fn recover_creates(&mut self, error: Node) {
        let kids = children(error);
        let creates: Vec<usize> = kids
            .iter()
            .enumerate()
            .filter(|(_, k)| k.kind() == "keyword_create")
            .map(|(i, _)| i)
            .collect();
        for (n, &i) in creates.iter().enumerate() {
            let mut j = i + 1;
            while j < kids.len() && is_header_modifier(kids[j].kind()) {
                j += 1;
            }
            let kind = kids.get(j).and_then(|k| object_keyword(k.kind()));
            let mut k = j + 1;
            while k < kids.len()
                && matches!(
                    kids[k].kind(),
                    "keyword_if" | "keyword_not" | "keyword_exists"
                )
            {
                k += 1;
            }
            let Some(kind) = kind else { continue };
            if k >= kids.len() {
                continue;
            }
            let name = if kids[k].kind() == "object_reference" {
                self.t(kids[k])
            } else {
                // Recovery can drop the name node and keep only its bytes;
                // they count only as a single name.
                let gap = decode_replace(&self.src[kids[k - 1].end_byte()..kids[k].start_byte()]);
                let gap = py_strip(&gap);
                if BARE_NAME.is_match(gap) {
                    gap.to_string()
                } else {
                    String::new()
                }
            };
            if name.is_empty() {
                continue;
            }
            let stop = creates.get(n + 1).copied().unwrap_or(kids.len());
            let rows = (
                kids[i].start_position().row,
                kids[stop - 1].end_position().row,
            );
            self.add_symbol(&name, kind, rows.0, rows.1);
            self.add_chunk(rows, kind, &name);
        }
    }
}

/// Cortex `parse_sql`. None when the grammar is not compiled in.
pub fn parse_sql(rel: &str, source: &[u8]) -> Option<Parsed> {
    let tree = crate::engine::parse_tree("sql", source)?;
    let mut x = Sql {
        rel,
        src: source,
        qual_prefix: rel.replace('/', "."),
        lines: lines_of(source),
        chunks: Vec::new(),
        symbols: Vec::new(),
        refs: IndexMap::new(),
        relation_refs: IndexMap::new(),
        seen: HashSet::new(),
    };
    let root = tree.root_node();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if let Some(kind) = definition(node.kind()) {
            let name = x.object_name(node);
            if !name.is_empty() {
                let rows = (node.start_position().row, node.end_position().row);
                x.add_symbol(&name, kind, rows.0, rows.1);
                x.add_chunk(rows, kind, &name);
                x.collect_relation_refs(node);
                continue;
            }
        }
        if node.kind() == "ERROR" {
            x.recover_creates(node);
            x.collect_relation_refs(node);
        }
        stack.extend(named_children(node).into_iter().rev());
    }
    if x.chunks.is_empty() && !x.lines.is_empty() {
        for statement in named_children(root) {
            if matches!(
                statement.kind(),
                "statement" | "select" | "insert" | "update" | "delete"
            ) {
                let rows = (statement.start_position().row, statement.end_position().row);
                x.add_chunk(rows, "query", "");
                x.collect_relation_refs(statement);
            }
        }
    }
    let names: Vec<String> = x.symbols.iter().map(|s| s.name.clone()).collect();
    drop_own(&mut x.refs, &names);
    for (k, v) in std::mem::take(&mut x.relation_refs) {
        *x.refs.entry(k).or_insert(0) += v;
    }
    Some(Parsed {
        chunks: x.chunks,
        symbols: x.symbols,
        imports: Vec::new(),
        refs: x.refs,
        bindings: IndexMap::new(),
        calls: IndexMap::new(),
        members: Vec::new(),
    })
}
