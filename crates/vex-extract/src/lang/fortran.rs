//! Fortran: Cortex's `languages/fortran.py`. Programs, modules,
//! subroutines and functions (names lower-cased, as Fortran is
//! case-insensitive), derived types, `use` imports, `call`s, and function
//! references to procedures the file defines.
//!
//! Cortex runs capital-F sources (`.F`, `.F90`, ...) through the host's C
//! preprocessor when one is installed, and parses the raw bytes otherwise.
//! Extraction here does no I/O and spawns nothing, so vex always parses
//! the raw bytes: Cortex's behaviour on a host without `cpp`. No golden
//! file is capital-F.

use crate::ast::{named_children, text};
use crate::lang::adapter::{chunk, drop_own, lines_of};
use indexmap::IndexMap;
use std::collections::HashSet;
use tree_sitter::Node;
use vex_core::{Chunk, Parsed, Symbol};

const MAX_CHUNK_LINES: usize = 120;

/// (header statement type, kind) of a scope.
fn scope_type(kind: &str) -> Option<(&'static str, &'static str)> {
    Some(match kind {
        "program" => ("program_statement", "program"),
        "module" => ("module_statement", "module"),
        "subroutine" => ("subroutine_statement", "subroutine"),
        "function" => ("function_statement", "function"),
        _ => return None,
    })
}

fn named_target(node: Node, kinds: &[&str], src: &[u8]) -> Option<String> {
    named_children(node)
        .into_iter()
        .find(|c| kinds.contains(&c.kind()))
        .map(|c| text(c, src).to_lowercase())
}

struct Fortran<'a> {
    rel: &'a str,
    lines: Vec<String>,
    chunks: Vec<Chunk>,
    symbols: Vec<Symbol>,
    seen: HashSet<(String, usize)>,
}

impl Fortran<'_> {
    /// A node ending at column 0 of a line ends on the line before.
    fn add_chunk(&mut self, node: Node, kind: &str, name: &str) {
        if self.lines.is_empty() {
            return;
        }
        let start = node.start_position().row;
        let end_pos = node.end_position();
        let end = start.max(end_pos.row.saturating_sub((end_pos.column == 0) as usize));
        let end = end.min(start + MAX_CHUNK_LINES).min(self.lines.len() - 1);
        if let Some(c) = chunk(
            self.rel,
            &self.lines,
            start,
            end,
            kind,
            "fortran",
            Some(name),
        ) {
            self.chunks.push(c);
        }
    }

    /// Every definition site, not just the first (ADR 19).
    fn add_symbol(&mut self, scope: &str, name: &str, kind: &str, node: Node) -> String {
        let qn = format!("{scope}.{name}");
        if self.seen.insert((qn.clone(), node.start_position().row)) {
            let start = node.start_position().row;
            let end_pos = node.end_position();
            let end = self
                .lines
                .len()
                .min((start + 1).max(end_pos.row + (end_pos.column != 0) as usize));
            self.symbols.push(Symbol {
                qualified_name: qn.clone(),
                name: name.to_string(),
                kind: kind.to_string(),
                file_path: self.rel.to_string(),
                start_line: start + 1,
                end_line: end,
                declaration: false,
            });
        }
        qn
    }
}

/// Cortex `parse_fortran` (without the preprocessor; see the module docs).
pub fn parse_fortran(rel: &str, source: &[u8]) -> Option<Parsed> {
    let tree = crate::engine::parse_tree("fortran", source)?;
    let src = source;
    let mut x = Fortran {
        rel,
        lines: lines_of(source),
        chunks: Vec::new(),
        symbols: Vec::new(),
        seen: HashSet::new(),
    };
    let mut imports = Vec::new();
    let mut bindings: IndexMap<String, (String, String)> = IndexMap::new();
    let mut bodies: Vec<(Node, String)> = Vec::new();
    let mut known: HashSet<String> = HashSet::new();
    let root = tree.root_node();

    let mut stack: Vec<(Node, String)> = vec![(root, rel.replace('/', "."))];
    while let Some((node, scope)) = stack.pop() {
        if let Some((statement_type, kind)) = scope_type(node.kind()) {
            let name = named_children(node)
                .into_iter()
                .find(|c| c.kind() == statement_type)
                .and_then(|s| named_target(s, &["name", "identifier"], src))
                .unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            let qn = x.add_symbol(&scope, &name, kind, node);
            x.add_chunk(node, kind, &name);
            if matches!(kind, "subroutine" | "function") {
                known.insert(name.clone());
                bodies.push((node, qn.clone()));
            }
            for c in named_children(node).into_iter().rev() {
                if c.kind() != statement_type
                    && !matches!(
                        c.kind(),
                        "end_module_statement"
                            | "end_program_statement"
                            | "end_subroutine_statement"
                            | "end_function_statement"
                    )
                {
                    stack.push((c, qn.clone()));
                }
            }
            continue;
        }
        if node.kind() == "derived_type_definition" {
            let name = named_children(node)
                .into_iter()
                .find(|c| c.kind() == "derived_type_statement")
                .and_then(|s| named_target(s, &["type_name", "identifier"], src));
            if let Some(name) = name {
                x.add_symbol(&scope, &name, "type", node);
                x.add_chunk(node, "type", &name);
            }
            continue;
        }
        if node.kind() == "use_statement" {
            if let Some(module) = named_target(node, &["module_name", "name", "identifier"], src) {
                imports.push(module.clone());
                bindings.insert(module.clone(), (module, String::new()));
            }
            continue;
        }
        for c in named_children(node).into_iter().rev() {
            stack.push((c, scope.clone()));
        }
    }

    let mut refs: IndexMap<String, u64> = IndexMap::new();
    let mut calls: IndexMap<(String, String), u64> = IndexMap::new();
    for (body, caller) in &bodies {
        let mut stack: Vec<Node> = named_children(*body)
            .into_iter()
            .filter(|c| {
                !matches!(
                    c.kind(),
                    "subroutine_statement"
                        | "function_statement"
                        | "program_statement"
                        | "module_statement"
                )
            })
            .rev()
            .collect();
        while let Some(node) = stack.pop() {
            if scope_type(node.kind()).is_some() {
                continue;
            }
            let callee = || named_target(node, &["name", "identifier"], src).unwrap_or_default();
            let name = match node.kind() {
                "subroutine_call" => Some(callee()).filter(|n| !n.is_empty()),
                "call_expression" => Some(callee()).filter(|n| known.contains(n)),
                _ => None,
            };
            if let Some(name) = name {
                *refs.entry(name.clone()).or_insert(0) += 1;
                *calls.entry((caller.clone(), name)).or_insert(0) += 1;
            }
            stack.extend(named_children(node).into_iter().rev());
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
