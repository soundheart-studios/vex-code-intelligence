//! Zig: Cortex's `languages/zig.py`. Top-level functions and the
//! functions inside `const T = struct/enum/union { ... }` declarations,
//! `@import`/`@cImport` as imports, and calls in function bodies by their
//! written name.

use crate::ast::{field, named_children, text};
use crate::lang::adapter::{chunk, drop_own, last_segment, lines_of};
use crate::text::py_strip;
use indexmap::IndexMap;
use regex::Regex;
use std::sync::LazyLock;
use tree_sitter::Node;
use vex_core::{Parsed, Symbol};

const MAX_CHUNK_LINES: usize = 120;

static IMPORT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"@(?:cImport|import)\s*\(\s*"([^"]+)""#).expect("valid regex"));

fn type_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "struct_declaration" => Some("struct"),
        "enum_declaration" => Some("enum"),
        "union_declaration" => Some("union"),
        _ => None,
    }
}

struct Zig<'a> {
    rel: &'a str,
    src: &'a [u8],
    qual_prefix: String,
    lines: Vec<String>,
    chunks: Vec<vex_core::Chunk>,
    symbols: Vec<Symbol>,
}

impl Zig<'_> {
    fn add_chunk(&mut self, start: usize, end: usize, kind: &str, name: &str) {
        let end = end
            .min(start + MAX_CHUNK_LINES)
            .min(self.lines.len().saturating_sub(1));
        if let Some(c) = chunk(self.rel, &self.lines, start, end, kind, "zig", Some(name)) {
            self.chunks.push(c);
        }
    }

    fn symbol(&mut self, qn: String, name: String, kind: &str, node: Node) {
        self.symbols.push(Symbol {
            qualified_name: qn,
            name,
            kind: kind.to_string(),
            file_path: self.rel.to_string(),
            start_line: node.start_position().row + 1,
            end_line: node.end_position().row + 1,
            declaration: false,
        });
    }

    fn function_name(&self, node: Node) -> String {
        if let Some(n) = field(node, "name") {
            return text(n, self.src);
        }
        named_children(node)
            .into_iter()
            .find(|c| c.kind() == "identifier")
            .map(|c| text(c, self.src))
            .unwrap_or_default()
    }

    fn add_function<'t>(
        &mut self,
        node: Node<'t>,
        parent: &str,
        functions: &mut Vec<(Node<'t>, String)>,
    ) {
        let name = self.function_name(node);
        if name.is_empty() {
            return;
        }
        let qualified = if parent.is_empty() {
            name.clone()
        } else {
            format!("{parent}.{name}")
        };
        let kind = if parent.is_empty() {
            "function"
        } else {
            "method"
        };
        let qn = format!("{}.{qualified}", self.qual_prefix);
        self.symbol(qn.clone(), name, kind, node);
        self.add_chunk(
            node.start_position().row,
            node.end_position().row,
            kind,
            &qualified,
        );
        functions.push((node, qn));
    }
}

/// Cortex `descendants(node, wanted)`: a stack popped from the end; a match
/// is not descended into.
fn descendants<'t>(node: Node<'t>, wanted: &str) -> Vec<Node<'t>> {
    let mut out = Vec::new();
    let mut stack = named_children(node);
    while let Some(current) = stack.pop() {
        if current.kind() == wanted {
            out.push(current);
            continue;
        }
        stack.extend(named_children(current));
    }
    out
}

/// Cortex `parse_zig`. None when the grammar is not compiled in.
pub fn parse_zig(rel: &str, source: &[u8]) -> Option<Parsed> {
    let tree = crate::engine::parse_tree("zig", source)?;
    let mut x = Zig {
        rel,
        src: source,
        qual_prefix: rel.replace('/', "."),
        lines: lines_of(source),
        chunks: Vec::new(),
        symbols: Vec::new(),
    };
    let mut imports = Vec::new();
    let mut bindings: IndexMap<String, (String, String)> = IndexMap::new();
    let mut functions: Vec<(Node, String)> = Vec::new();

    for node in named_children(tree.root_node()) {
        if node.kind() == "function_declaration" {
            x.add_function(node, "", &mut functions);
            continue;
        }
        if node.kind() != "variable_declaration" {
            continue;
        }
        let kids = named_children(node);
        let name_node = kids.iter().copied().find(|c| c.kind() == "identifier");
        let declaration = kids.iter().copied().find(|c| type_kind(c.kind()).is_some());
        if let (Some(n), Some(d)) = (name_node, declaration) {
            let name = text(n, source);
            let kind = type_kind(d.kind()).unwrap_or_default();
            x.symbol(
                format!("{}.{name}", x.qual_prefix),
                name.clone(),
                kind,
                node,
            );
            x.add_chunk(
                node.start_position().row,
                node.end_position().row,
                kind,
                &name,
            );
            for function in descendants(d, "function_declaration") {
                x.add_function(function, &name, &mut functions);
            }
        }
        if let Some(m) = IMPORT.captures(&text(node, source)) {
            let module = m[1].to_string();
            imports.push(module.clone());
            if let Some(n) = name_node {
                bindings.insert(text(n, source), (module, String::new()));
            }
        }
    }

    match x.symbols.iter().map(|s| s.start_line).min() {
        Some(first) if first > 1 => x.add_chunk(0, first - 2, "module", ""),
        Some(_) => {}
        None if !x.lines.is_empty() => {
            let last = x.lines.len() - 1;
            x.add_chunk(0, last, "module", "");
        }
        None => {}
    }

    let mut refs: IndexMap<String, u64> = IndexMap::new();
    let mut calls: IndexMap<(String, String), u64> = IndexMap::new();
    for (function, caller) in &functions {
        let Some(body) = field(*function, "body") else {
            continue;
        };
        let mut stack: Vec<Node> = named_children(body).into_iter().rev().collect();
        while let Some(child) = stack.pop() {
            if child.kind() == "function_declaration" {
                continue;
            }
            if child.kind() == "call_expression"
                && let Some(f) = field(child, "function")
            {
                let written = py_strip(&text(f, source)).to_string();
                let callee = last_segment(&written);
                if callee.chars().count() > 2 {
                    *refs.entry(callee.to_string()).or_insert(0) += 1;
                    *calls.entry((caller.clone(), written.clone())).or_insert(0) += 1;
                }
            }
            stack.extend(named_children(child).into_iter().rev());
        }
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
