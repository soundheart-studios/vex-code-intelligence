//! Julia: Cortex's `languages/julia.py`. Modules, structs and abstract or
//! primitive types, functions (long form and `f(x) = ...`, each method its
//! own site), macros, `using`/`import`, and calls and macro calls.

use crate::ast::{named_children, text};
use crate::lang::adapter::{chunk, drop_own, last_segment, lines_of};
use crate::text::py_strip;
use indexmap::IndexMap;
use regex::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;
use tree_sitter::Node;
use vex_core::{Chunk, Parsed, Symbol};

const MAX_CHUNK_LINES: usize = 120;

static QUOTED_OPERATOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^:\((.+)\)$").expect("valid regex"));

/// Identifiers under `node` (not `node` itself) in document order.
fn identifiers_in<'t>(node: Node<'t>, src: &[u8], first_only: bool) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack: Vec<Node<'t>> = named_children(node).into_iter().rev().collect();
    while let Some(child) = stack.pop() {
        if child.kind() == "identifier" {
            out.push(text(child, src));
            if first_only {
                break;
            }
        }
        stack.extend(named_children(child).into_iter().rev());
    }
    out
}

fn first_identifier(node: Node, src: &[u8]) -> String {
    identifiers_in(node, src, true)
        .into_iter()
        .next()
        .unwrap_or_default()
}

/// The call in a signature, through `where` clauses and `::T` returns.
fn signature_call(mut target: Option<Node>) -> Option<Node> {
    while let Some(t) = target {
        if t.kind() == "call_expression" {
            return Some(t);
        }
        target = named_children(t).into_iter().find(|c| {
            matches!(
                c.kind(),
                "call_expression" | "where_expression" | "typed_expression"
            )
        });
    }
    None
}

/// (qualifier, name): `function Base.haskey(...)` is `haskey` owned by
/// `Base`; `Base.:(==)` and `Base.:+` name the operator itself.
fn function_name(node: Node, src: &[u8]) -> (String, String) {
    let target = named_children(node)
        .into_iter()
        .find(|c| c.kind() == "signature")
        .unwrap_or(node);
    if let Some(call) = signature_call(Some(target))
        && let Some(callee) = call.named_child(0)
    {
        if callee.kind() == "identifier" {
            return (String::new(), text(callee, src));
        }
        if callee.kind() == "field_expression" {
            let t = text(callee, src);
            let (qualifier, name) = match t.rfind('.') {
                Some(i) => (t[..i].to_string(), t[i + 1..].to_string()),
                None => (String::new(), t.clone()),
            };
            let name = QUOTED_OPERATOR.replace(&name, "$1").into_owned();
            let name = name.strip_prefix(':').unwrap_or(&name).to_string();
            if !name.is_empty() {
                return (qualifier, name);
            }
        }
    }
    (String::new(), first_identifier(target, src))
}

fn import_names(node: Node, src: &[u8]) -> Vec<String> {
    let mut names = Vec::new();
    for child in named_children(node) {
        match child.kind() {
            "identifier" | "scoped_identifier" => {
                names.push(text(child, src).trim_start_matches('.').to_string());
            }
            "import_path" => {
                if let Some(last) = named_children(child)
                    .into_iter()
                    .filter(|c| c.kind() == "identifier")
                    .map(|c| text(c, src))
                    .next_back()
                {
                    names.push(last);
                }
            }
            "selected_import" => {
                let package = named_children(child).into_iter().find(|c| {
                    matches!(c.kind(), "identifier" | "scoped_identifier" | "import_path")
                });
                if let Some(p) = package {
                    if p.kind() == "import_path" {
                        names.extend(import_names(p, src));
                    } else {
                        names.push(text(p, src).trim_start_matches('.').to_string());
                    }
                }
            }
            _ => {}
        }
    }
    names.retain(|n| !n.is_empty());
    names
}

struct Julia<'a> {
    rel: &'a str,
    lines: Vec<String>,
    chunks: Vec<Chunk>,
    symbols: Vec<Symbol>,
    seen: HashSet<(String, usize)>,
}

impl Julia<'_> {
    fn add_chunk(&mut self, node: Node, kind: &str, name: &str) {
        if self.lines.is_empty() {
            return;
        }
        let start = node.start_position().row;
        let end = node
            .end_position()
            .row
            .min(start + MAX_CHUNK_LINES)
            .min(self.lines.len() - 1);
        if let Some(c) = chunk(self.rel, &self.lines, start, end, kind, "julia", Some(name)) {
            self.chunks.push(c);
        }
    }

    /// Each method is its own site (ADR 19).
    fn add_symbol(&mut self, scope: &str, name: &str, kind: &str, node: Node) -> String {
        let qn = format!("{scope}.{name}");
        if self.seen.insert((qn.clone(), node.start_position().row)) {
            self.symbols.push(Symbol {
                qualified_name: qn.clone(),
                name: name.to_string(),
                kind: kind.to_string(),
                file_path: self.rel.to_string(),
                start_line: node.start_position().row + 1,
                end_line: node.end_position().row + 1,
                declaration: false,
            });
        }
        qn
    }
}

/// Cortex `parse_julia`. None when the grammar is not compiled in.
pub fn parse_julia(rel: &str, source: &[u8]) -> Option<Parsed> {
    let tree = crate::engine::parse_tree("julia", source)?;
    let src = source;
    let mut x = Julia {
        rel,
        lines: lines_of(source),
        chunks: Vec::new(),
        symbols: Vec::new(),
        seen: HashSet::new(),
    };
    let qual_prefix = rel.replace('/', ".");
    let mut imports = Vec::new();
    let mut bindings: IndexMap<String, (String, String)> = IndexMap::new();
    let mut refs: IndexMap<String, u64> = IndexMap::new();
    // (body, caller, skip_signature)
    let mut bodies: Vec<(Node, String, bool)> = Vec::new();

    let root = tree.root_node();
    let mut stack: Vec<(Node, String)> = vec![(root, qual_prefix.clone())];
    while let Some((node, scope)) = stack.pop() {
        match node.kind() {
            "module_definition" => {
                let name = named_children(node)
                    .into_iter()
                    .find(|c| c.kind() == "identifier")
                    .map(|c| text(c, src))
                    .unwrap_or_default();
                if name.is_empty() {
                    continue;
                }
                let qn = x.add_symbol(&scope, &name, "module", node);
                x.add_chunk(node, "module", &name);
                for c in named_children(node).into_iter().rev() {
                    if c.kind() != "identifier" {
                        stack.push((c, qn.clone()));
                    }
                }
                continue;
            }
            "struct_definition" | "abstract_definition" | "primitive_definition" => {
                let type_head = named_children(node)
                    .into_iter()
                    .find(|c| c.kind() == "type_head")
                    .unwrap_or(node);
                let name = first_identifier(type_head, src);
                if !name.is_empty() {
                    let kind = if node.kind() == "struct_definition" {
                        "struct"
                    } else {
                        "type"
                    };
                    x.add_symbol(&scope, &name, kind, node);
                    x.add_chunk(node, kind, &name);
                    let ids = identifiers_in(type_head, src, false);
                    if ids.len() > 1 {
                        *refs.entry(ids[ids.len() - 1].clone()).or_insert(0) += 1;
                    }
                }
                continue;
            }
            "function_definition" => {
                let (qualifier, name) = function_name(node, src);
                if !name.is_empty() {
                    let owner = if qualifier.is_empty() {
                        scope.clone()
                    } else {
                        format!("{scope}.{qualifier}")
                    };
                    let qn = x.add_symbol(&owner, &name, "function", node);
                    x.add_chunk(node, "function", &name);
                    bodies.push((node, qn, true));
                }
                continue;
            }
            "macro_definition" | "macro_statement" => {
                let first = first_identifier(node, src);
                let name = first.trim_start_matches('@');
                if !name.is_empty() {
                    let qn = x.add_symbol(&scope, name, "macro", node);
                    x.add_chunk(node, "macro", name);
                    bodies.push((node, qn, true));
                }
                continue;
            }
            "assignment" | "short_function_definition" => {
                if let Some(lhs) = node.named_child(0)
                    && matches!(lhs.kind(), "call_expression" | "where_expression")
                    && signature_call(Some(lhs)).is_some()
                {
                    let (qualifier, name) = function_name(lhs, src);
                    if !name.is_empty() {
                        let owner = if qualifier.is_empty() {
                            scope.clone()
                        } else {
                            format!("{scope}.{qualifier}")
                        };
                        let qn = x.add_symbol(&owner, &name, "function", node);
                        x.add_chunk(node, "function", &name);
                        let count = node.named_child_count();
                        if let Some(body) = node.named_child(count.saturating_sub(1)) {
                            bodies.push((body, qn, false));
                        }
                        continue;
                    }
                }
            }
            "using_statement" | "import_statement" => {
                for module in import_names(node, src) {
                    imports.push(module.clone());
                    bindings.insert(last_segment(&module).to_string(), (module, String::new()));
                }
                continue;
            }
            _ => {}
        }
        for c in named_children(node).into_iter().rev() {
            stack.push((c, scope.clone()));
        }
    }

    let mut calls: IndexMap<(String, String), u64> = IndexMap::new();
    for (body, caller, skip_signature) in &bodies {
        let mut stack = vec![(*body, *skip_signature)];
        while let Some((node, skip)) = stack.pop() {
            if matches!(node.kind(), "function_definition" | "macro_definition") {
                if skip {
                    for c in named_children(node).into_iter().rev() {
                        if c.kind() != "signature" {
                            stack.push((c, false));
                        }
                    }
                }
                continue;
            }
            if matches!(node.kind(), "call_expression" | "macrocall_expression") {
                let written = node
                    .named_child(0)
                    .map(|callee| {
                        py_strip(&text(callee, src))
                            .trim_start_matches('@')
                            .to_string()
                    })
                    .unwrap_or_default();
                let base = last_segment(&written);
                if base.chars().count() > 1 {
                    *refs.entry(base.to_string()).or_insert(0) += 1;
                    *calls.entry((caller.clone(), written.clone())).or_insert(0) += 1;
                }
            }
            for c in named_children(node).into_iter().rev() {
                stack.push((c, false));
            }
        }
    }

    if x.chunks.is_empty() && !x.lines.is_empty() {
        x.add_chunk(root, "module", "");
    }
    let names: Vec<String> = x.symbols.iter().map(|s| s.name.clone()).collect();
    drop_own(&mut refs, &names);
    Some(Parsed {
        chunks: x.chunks,
        symbols: x.symbols,
        imports,
        refs,
        bindings,
        calls,
        members: Vec::new(),
    })
}
