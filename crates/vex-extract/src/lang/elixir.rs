//! Elixir: Cortex's `languages/elixir.py`. `defmodule` and the def forms
//! are definitions (every clause its own site), alias/import/require/use
//! are imports binding the last segment, and calls in function bodies are
//! calls unless they are a special form or a Kernel builtin.

use crate::ast::{field, named_children, text};
use crate::lang::adapter::{chunk, drop_own, kids, last_segment, lines_of};
use crate::text::py_strip;
use indexmap::IndexMap;
use std::collections::HashSet;
use tree_sitter::Node;
use vex_core::{Chunk, Parsed, Symbol};

const MAX_CHUNK_LINES: usize = 120;

fn is_definition(keyword: &str) -> bool {
    matches!(keyword, "def" | "defp" | "defmacro" | "defmacrop")
}

fn is_import(keyword: &str) -> bool {
    matches!(keyword, "alias" | "import" | "require" | "use")
}

fn is_non_call(keyword: &str) -> bool {
    is_definition(keyword)
        || is_import(keyword)
        || matches!(
            keyword,
            "defmodule"
                | "defstruct"
                | "defprotocol"
                | "defimpl"
                | "defguard"
                | "if"
                | "unless"
                | "case"
                | "cond"
                | "with"
                | "for"
                | "quote"
                | "receive"
        )
}

const BUILTINS: &[&str] = &[
    "and",
    "apply",
    "div",
    "elem",
    "exit",
    "hd",
    "inspect",
    "is_atom",
    "is_binary",
    "is_boolean",
    "is_integer",
    "is_list",
    "is_map",
    "is_nil",
    "length",
    "map_size",
    "not",
    "or",
    "raise",
    "rem",
    "self",
    "send",
    "spawn",
    "tl",
    "to_string",
    "tuple_size",
];

/// (identifier, arguments, do_block) of a call; the last of each kind wins.
fn call_parts(node: Node) -> (Option<Node>, Option<Node>, Option<Node>) {
    let (mut identifier, mut arguments, mut do_block) = (None, None, None);
    for c in kids(node) {
        match c.kind() {
            "identifier" => identifier = Some(c),
            "arguments" => arguments = Some(c),
            "do_block" => do_block = Some(c),
            _ => {}
        }
    }
    (identifier, arguments, do_block)
}

struct Elixir<'a> {
    rel: &'a str,
    src: &'a [u8],
    qual_prefix: String,
    lines: Vec<String>,
    chunks: Vec<Chunk>,
    symbols: Vec<Symbol>,
    imports: Vec<String>,
    bindings: IndexMap<String, (String, String)>,
    seen: HashSet<(String, usize)>,
}

impl Elixir<'_> {
    fn t(&self, node: Node) -> String {
        text(node, self.src)
    }

    fn add_chunk(&mut self, start: usize, end: usize, kind: &str, name: &str) {
        if self.lines.is_empty() {
            return;
        }
        let end = end.min(start + MAX_CHUNK_LINES).min(self.lines.len() - 1);
        if let Some(c) = chunk(
            self.rel,
            &self.lines,
            start,
            end,
            kind,
            "elixir",
            Some(name),
        ) {
            self.chunks.push(c);
        }
    }

    /// Each clause is its own site (ADR 19).
    fn add_symbol(&mut self, qn: &str, name: &str, kind: &str, node: Node) {
        if !self
            .seen
            .insert((qn.to_string(), node.start_position().row))
        {
            return;
        }
        self.symbols.push(Symbol {
            qualified_name: qn.to_string(),
            name: name.to_string(),
            kind: kind.to_string(),
            file_path: self.rel.to_string(),
            start_line: node.start_position().row + 1,
            end_line: node.end_position().row + 1,
            declaration: false,
        });
    }

    fn alias_modules(&self, arguments: Option<Node>) -> Vec<String> {
        let Some(arguments) = arguments else {
            return Vec::new();
        };
        for child in kids(arguments) {
            if child.kind() == "alias" {
                return vec![self.t(child)];
            }
            if child.kind() == "dot" {
                let ck = kids(child);
                let base = ck
                    .iter()
                    .find(|c| c.kind() == "alias")
                    .map(|c| self.t(*c))
                    .unwrap_or_default();
                let tuple = ck.iter().find(|c| c.kind() == "tuple");
                if let (false, Some(tuple)) = (base.is_empty(), tuple) {
                    let members: Vec<String> = kids(*tuple)
                        .into_iter()
                        .filter(|c| c.kind() == "alias")
                        .map(|c| self.t(c))
                        .collect();
                    if !members.is_empty() {
                        return members.iter().map(|m| format!("{base}.{m}")).collect();
                    }
                }
                let written = self.t(child);
                let written = py_strip(&written);
                return if written.is_empty() {
                    Vec::new()
                } else {
                    vec![written.to_string()]
                };
            }
        }
        Vec::new()
    }

    fn function_name(&self, arguments: Option<Node>) -> String {
        let Some(arguments) = arguments else {
            return String::new();
        };
        for child in kids(arguments) {
            let mut child = Some(child);
            if let Some(c) = child.filter(|c| c.kind() == "binary_operator") {
                if let Some(op) = field(c, "operator") {
                    if self.t(op) == "when" {
                        child = field(c, "left");
                    }
                } else if kids(c).iter().any(|k| k.kind() == "when") {
                    child = field(c, "left");
                }
            }
            let Some(child) = child else { continue };
            if child.kind() == "call" {
                if let (Some(ident), _, _) = call_parts(child) {
                    return self.t(ident);
                }
            } else if child.kind() == "identifier" {
                return self.t(child);
            }
        }
        String::new()
    }

    /// Cortex `walk`, in document order without recursion; returns the
    /// function bodies with their callers.
    fn walk<'t>(&mut self, root: Node<'t>) -> Vec<(Node<'t>, String)> {
        let mut bodies = Vec::new();
        let mut stack: Vec<(Node<'t>, String, String)> = vec![(root, String::new(), String::new())];
        let push_children =
            |stack: &mut Vec<(Node<'t>, String, String)>, node: Node<'t>, m: &str, q: &str| {
                for c in kids(node).into_iter().rev() {
                    stack.push((c, m.to_string(), q.to_string()));
                }
            };
        while let Some((node, module_name, module_qn)) = stack.pop() {
            if node.kind() != "call" {
                push_children(&mut stack, node, &module_name, &module_qn);
                continue;
            }
            let (identifier, arguments, do_block) = call_parts(node);
            let Some(identifier) = identifier else {
                push_children(&mut stack, node, &module_name, &module_qn);
                continue;
            };
            let keyword = self.t(identifier);
            if keyword == "defmodule" {
                let names = self.alias_modules(arguments);
                let Some(name) = names.into_iter().next() else {
                    continue;
                };
                let qn = format!("{}.{name}", self.qual_prefix);
                self.add_symbol(&qn, &name, "module", node);
                let start = node.start_position().row as i64;
                let header_end = do_block
                    .map(kids)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|c| {
                        c.kind() == "call"
                            && call_parts(*c).0.is_some_and(|i| is_definition(&self.t(i)))
                    })
                    .map(|c| c.start_position().row as i64 - 1)
                    .min()
                    .unwrap_or(node.end_position().row as i64);
                self.add_chunk(
                    start as usize,
                    start.max(header_end) as usize,
                    "module",
                    &name,
                );
                if let Some(d) = do_block {
                    push_children(&mut stack, d, &name, &qn);
                }
                continue;
            }
            if is_definition(&keyword) {
                let name = self.function_name(arguments);
                if name.is_empty() {
                    continue;
                }
                let parent = if module_qn.is_empty() {
                    self.qual_prefix.clone()
                } else {
                    module_qn.clone()
                };
                let qn = format!("{parent}.{name}");
                let kind = if module_qn.is_empty() {
                    "function"
                } else {
                    "method"
                };
                self.add_symbol(&qn, &name, kind, node);
                let label = if module_name.is_empty() {
                    name.clone()
                } else {
                    format!("{module_name}.{name}")
                };
                self.add_chunk(
                    node.start_position().row,
                    node.end_position().row,
                    kind,
                    &label,
                );
                if let Some(d) = do_block {
                    bodies.push((d, qn));
                } else {
                    // `do: expression` is a keyword pair, not a do_block;
                    // the definition head is never walked as a call.
                    for container in named_children(node) {
                        let candidates = if container.kind() == "keywords" {
                            vec![container]
                        } else {
                            named_children(container)
                        };
                        for keywords in candidates {
                            if keywords.kind() != "keywords" {
                                continue;
                            }
                            for pair in named_children(keywords) {
                                if let (Some(k), Some(v)) =
                                    (field(pair, "key"), field(pair, "value"))
                                {
                                    let key = self.t(k);
                                    if py_strip(&key).trim_end_matches(':') == "do" {
                                        bodies.push((v, qn.clone()));
                                    }
                                }
                            }
                        }
                    }
                }
                continue;
            }
            if is_import(&keyword) {
                for module in self.alias_modules(arguments) {
                    self.imports.push(module.clone());
                    let local = last_segment(&module).to_string();
                    self.bindings.insert(local, (module, String::new()));
                }
                continue;
            }
            push_children(&mut stack, node, &module_name, &module_qn);
        }
        bodies
    }
}

/// Cortex `parse_elixir`. None when the grammar is not compiled in.
pub fn parse_elixir(rel: &str, source: &[u8]) -> Option<Parsed> {
    let tree = crate::engine::parse_tree("elixir", source)?;
    let mut x = Elixir {
        rel,
        src: source,
        qual_prefix: rel.replace('/', "."),
        lines: lines_of(source),
        chunks: Vec::new(),
        symbols: Vec::new(),
        imports: Vec::new(),
        bindings: IndexMap::new(),
        seen: HashSet::new(),
    };
    let bodies = x.walk(tree.root_node());

    let mut refs: IndexMap<String, u64> = IndexMap::new();
    let mut calls: IndexMap<(String, String), u64> = IndexMap::new();
    for (body, caller) in &bodies {
        let mut stack = vec![*body];
        while let Some(node) = stack.pop() {
            let ck = kids(node);
            if node.kind() == "call" && !call_parts(node).0.is_some_and(|i| is_non_call(&x.t(i))) {
                let mut written = String::new();
                for c in &ck {
                    if c.kind() == "dot" {
                        written = x.t(*c).trim_end_matches('.').to_string();
                        break;
                    }
                    if c.kind() == "identifier" {
                        written = x.t(*c);
                        break;
                    }
                }
                let callee = last_segment(&written);
                if !callee.is_empty() && !BUILTINS.contains(&callee) {
                    *refs.entry(callee.to_string()).or_insert(0) += 1;
                    *calls.entry((caller.clone(), written.clone())).or_insert(0) += 1;
                }
            }
            stack.extend(ck.into_iter().rev());
        }
    }

    if x.chunks.is_empty() && !x.lines.is_empty() {
        let last = x.lines.len() - 1;
        x.add_chunk(0, last, "module", "");
    }
    let names: Vec<String> = x.symbols.iter().map(|s| s.name.clone()).collect();
    drop_own(&mut refs, &names);
    Some(Parsed {
        chunks: x.chunks,
        symbols: x.symbols,
        imports: x.imports,
        refs,
        bindings: x.bindings,
        calls,
        members: Vec::new(),
    })
}
