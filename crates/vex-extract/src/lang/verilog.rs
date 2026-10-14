//! Verilog/SystemVerilog: Cortex's `languages/verilog.py`. Modules,
//! interfaces, programs, packages and classes own their functions and
//! tasks; package imports bind the package; instantiations are refs that
//! survive the own-definition filter; subroutine calls are calls.

use crate::ast::{field, named_children, text};
use crate::lang::adapter::{chunk, drop_own, lines_of};
use indexmap::IndexMap;
use std::collections::HashSet;
use tree_sitter::Node;
use vex_core::{Chunk, Parsed, Symbol};

const MAX_CHUNK_LINES: usize = 120;

/// (kind, header node type) of a container.
fn container(kind: &str) -> Option<(&'static str, &'static str)> {
    Some(match kind {
        "module_declaration" => ("module", "module_header"),
        "interface_declaration" => ("interface", "interface_header"),
        "program_declaration" => ("program", "program_header"),
        "package_declaration" => ("package", "package_header"),
        "class_declaration" => ("class", "class_header"),
        "interface_class_declaration" => ("interface", "interface_class_header"),
        _ => return None,
    })
}

fn child_of<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
    named_children(node).into_iter().find(|c| c.kind() == kind)
}

/// The first simple or escaped identifier under `node`, document order.
fn first_identifier(node: Option<Node>, src: &[u8]) -> String {
    let Some(node) = node else {
        return String::new();
    };
    let mut stack: Vec<Node> = named_children(node).into_iter().rev().collect();
    while let Some(child) = stack.pop() {
        if matches!(child.kind(), "simple_identifier" | "escaped_identifier") {
            return text(child, src);
        }
        stack.extend(named_children(child).into_iter().rev());
    }
    String::new()
}

struct Verilog<'a> {
    rel: &'a str,
    lines: Vec<String>,
    chunks: Vec<Chunk>,
    symbols: Vec<Symbol>,
    seen: HashSet<(String, usize)>,
}

impl Verilog<'_> {
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
        if let Some(c) = chunk(
            self.rel,
            &self.lines,
            start,
            end,
            kind,
            "verilog",
            Some(name),
        ) {
            self.chunks.push(c);
        }
    }

    /// Every definition site, not just the first (ADR 19).
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

/// Cortex `parse_verilog`. None when the grammar is not compiled in.
pub fn parse_verilog(rel: &str, source: &[u8]) -> Option<Parsed> {
    let tree = crate::engine::parse_tree("verilog", source)?;
    let src = source;
    let mut x = Verilog {
        rel,
        lines: lines_of(source),
        chunks: Vec::new(),
        symbols: Vec::new(),
        seen: HashSet::new(),
    };
    let mut imports = Vec::new();
    let mut bindings: IndexMap<String, (String, String)> = IndexMap::new();
    let mut refs: IndexMap<String, u64> = IndexMap::new();
    let mut instance_refs: IndexMap<String, u64> = IndexMap::new();
    let mut bodies: Vec<(Node, String)> = Vec::new();
    let root = tree.root_node();

    let mut stack: Vec<(Node, String)> = vec![(root, rel.replace('/', "."))];
    while let Some((node, scope)) = stack.pop() {
        if let Some((kind, header_type)) = container(node.kind()) {
            let header = child_of(node, header_type);
            let name = first_identifier(Some(header.unwrap_or(node)), src);
            if name.is_empty() {
                continue;
            }
            let qn = x.add_symbol(&scope, &name, kind, node);
            x.add_chunk(node, kind, &name);
            for c in named_children(node).into_iter().rev() {
                if header.is_none_or(|h| h.id() != c.id()) {
                    stack.push((c, qn.clone()));
                }
            }
            continue;
        }
        let routine = match node.kind() {
            "function_declaration" => Some((
                "function",
                "function_body_declaration",
                "function_identifier",
            )),
            "task_declaration" => Some(("task", "task_body_declaration", "task_identifier")),
            _ => None,
        };
        if let Some((kind, body_type, id_type)) = routine {
            let name = first_identifier(
                child_of(node, body_type).and_then(|b| child_of(b, id_type)),
                src,
            );
            if !name.is_empty() {
                let qn = x.add_symbol(&scope, &name, kind, node);
                x.add_chunk(node, kind, &name);
                bodies.push((node, qn));
            }
            continue;
        }
        if node.kind() == "package_import_declaration" {
            for item in named_children(node) {
                if item.kind() != "package_import_item" {
                    continue;
                }
                let t = text(item, src);
                let module = crate::text::py_strip(t.split("::").next().unwrap_or(""));
                if !module.is_empty() {
                    imports.push(module.to_string());
                    bindings.insert(module.to_string(), (module.to_string(), String::new()));
                }
            }
            continue;
        }
        if matches!(
            node.kind(),
            "module_instantiation"
                | "checker_instantiation"
                | "interface_instantiation"
                | "program_instantiation"
        ) {
            let instance_type = match field(node, "module_type") {
                Some(t) => crate::text::py_strip(&text(t, src)).to_string(),
                None => first_identifier(Some(node), src),
            };
            if !instance_type.is_empty() {
                *refs.entry(instance_type.clone()).or_insert(0) += 1;
                *instance_refs.entry(instance_type).or_insert(0) += 1;
            }
            continue;
        }
        for c in named_children(node).into_iter().rev() {
            stack.push((c, scope.clone()));
        }
    }

    let mut calls: IndexMap<(String, String), u64> = IndexMap::new();
    let mut seen_calls: HashSet<(String, usize, usize, String)> = HashSet::new();
    for (body, caller) in &bodies {
        let mut stack = vec![*body];
        while let Some(node) = stack.pop() {
            let is_let_call = node.kind() == "let_expression"
                && crate::ast::children(node).iter().any(|c| c.kind() == "(");
            if matches!(
                node.kind(),
                "subroutine_call" | "tf_call" | "system_tf_call"
            ) || is_let_call
            {
                let name = first_identifier(Some(node), src);
                let key = (
                    caller.clone(),
                    node.start_byte(),
                    node.end_byte(),
                    name.clone(),
                );
                if !name.is_empty() && seen_calls.insert(key) {
                    *refs.entry(name.clone()).or_insert(0) += 1;
                    *calls.entry((caller.clone(), name)).or_insert(0) += 1;
                }
            }
            for c in named_children(node).into_iter().rev() {
                if !matches!(c.kind(), "function_declaration" | "task_declaration") {
                    stack.push(c);
                }
            }
        }
    }

    if x.chunks.is_empty() && !x.lines.is_empty() {
        x.add_chunk(root, "module", "");
    }
    let names: Vec<String> = x.symbols.iter().map(|s| s.name.clone()).collect();
    drop_own(&mut refs, &names);
    // Instantiations of the file's own modules stay as refs.
    for (k, v) in instance_refs {
        *refs.entry(k).or_insert(0) += v;
    }
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
