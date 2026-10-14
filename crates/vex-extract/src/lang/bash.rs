//! Bash: Cortex's `languages/bash.py`. Functions, `source`/`.` imports,
//! and calls to functions the file itself defines (anything else a script
//! runs is an external command). Top-level commands belong to a synthetic
//! `__entry__` symbol spanning the file.

use crate::ast::{field, text};
use crate::engine::SourceResolver;
use crate::lang::adapter::{chunk, drop_own, kids, lines_of};
use crate::text::py_strip;
use indexmap::IndexMap;
use std::collections::HashSet;
use tree_sitter::Node;
use vex_core::{Chunk, Parsed, Symbol};

const MAX_CHUNK_LINES: usize = 120;

fn is_source_command(name: &str) -> bool {
    matches!(name, "source" | ".")
}

struct Bash<'a> {
    rel: &'a str,
    src: &'a [u8],
    lines: Vec<String>,
    chunks: Vec<Chunk>,
}

impl Bash<'_> {
    /// Cortex `literal`: the word unquoted, or "" when it is not a plain
    /// literal (expansions, substitutions, pipes, separators).
    fn literal(&self, node: Node) -> String {
        let t = text(node, self.src);
        let mut raw = py_strip(&t).to_string();
        let chars: Vec<char> = raw.chars().collect();
        if chars.len() >= 2 && matches!(chars[0], '\'' | '"') && chars[chars.len() - 1] == chars[0]
        {
            raw = chars[1..chars.len() - 1].iter().collect();
        }
        if ["$", "`", "$(", "<(", ">(", "|", ";", "&"]
            .iter()
            .any(|tok| raw.contains(tok))
        {
            return String::new();
        }
        raw
    }

    fn function_name(&self, node: Node) -> String {
        if let Some(n) = field(node, "name") {
            return self.literal(n);
        }
        kids(node)
            .into_iter()
            .find(|c| c.kind() == "word")
            .map(|c| self.literal(c))
            .unwrap_or_default()
    }

    fn command_name<'t>(&self, command: Node<'t>) -> Option<Node<'t>> {
        field(command, "name").or_else(|| command.child(0))
    }

    fn add_chunk(&mut self, start: usize, end: usize, kind: &str, name: &str) {
        if self.lines.is_empty() {
            return;
        }
        let end = end.min(start + MAX_CHUNK_LINES).min(self.lines.len() - 1);
        if let Some(c) = chunk(self.rel, &self.lines, start, end, kind, "bash", Some(name)) {
            self.chunks.push(c);
        }
    }
}

fn inside_expansion(node: Node) -> bool {
    let mut parent = node.parent();
    while let Some(p) = parent {
        if matches!(p.kind(), "command_substitution" | "process_substitution") {
            return true;
        }
        parent = p.parent();
    }
    false
}

/// The directory part of a relative path (`Path(rel).parent`, "" at top).
fn rel_dir(rel: &str) -> &str {
    rel.rfind('/').map_or("", |i| &rel[..i])
}

/// Cortex `parse_bash`. `resolve` answers whether a sourced relative or
/// absolute path names a file under the project root, and as what.
pub fn parse_bash(rel: &str, source: &[u8], resolve: SourceResolver) -> Option<Parsed> {
    let tree = crate::engine::parse_tree("bash", source)?;
    let mut x = Bash {
        rel,
        src: source,
        lines: lines_of(source),
        chunks: Vec::new(),
    };
    let qual_prefix = rel.replace('/', ".");
    let root = tree.root_node();

    let mut defined: HashSet<String> = HashSet::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "function_definition" {
            let name = x.function_name(node);
            if !name.is_empty() {
                defined.insert(name);
            }
        }
        stack.extend(kids(node));
    }

    let mut symbols: Vec<Symbol> = Vec::new();
    let mut imports: Vec<String> = Vec::new();
    let mut bodies: Vec<(Node, String)> = Vec::new();
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    // Cortex `collect`, in document order.
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "function_definition" {
            let name = x.function_name(node);
            if name.is_empty() {
                continue;
            }
            let (start, end) = (node.start_position().row, node.end_position().row);
            let qn = format!("{qual_prefix}.{name}");
            symbols.push(Symbol {
                qualified_name: qn.clone(),
                name: name.clone(),
                kind: "function".to_string(),
                file_path: rel.to_string(),
                start_line: start + 1,
                end_line: end + 1,
                declaration: false,
            });
            x.add_chunk(start, end, "function", &name);
            ranges.push((start, end));
            if let Some(body) = kids(node)
                .into_iter()
                .find(|c| c.kind() == "compound_statement")
            {
                bodies.push((body, qn));
                stack.extend(kids(body).into_iter().rev());
            }
            continue;
        }
        if node.kind() == "command"
            && !inside_expansion(node)
            && let Some(name_node) = x.command_name(node)
            && is_source_command(&x.literal(name_node))
        {
            // Cortex filters with `child is not name_node`. A name from
            // `child_by_field_name` is a fresh wrapper, never identical to a
            // child; the fallback `children[0]` comes from the same cached
            // list, so only then is the name itself left out.
            let by_field = field(node, "name").is_some();
            let args: Vec<Node> = kids(node)
                .into_iter()
                .filter(|c| {
                    (by_field || c.id() != name_node.id())
                        && matches!(c.kind(), "word" | "string" | "concatenation" | "raw_string")
                })
                .collect();
            if let Some(first) = args.first() {
                let raw = x.literal(*first);
                if !raw.is_empty() {
                    if raw.starts_with('.') || raw.starts_with('/') {
                        if let Some(module) = resolve(rel_dir(rel), &raw) {
                            imports.push(module);
                        }
                    } else {
                        imports.push(raw);
                    }
                }
            }
            continue;
        }
        stack.extend(kids(node).into_iter().rev());
    }

    let mut refs: IndexMap<String, u64> = IndexMap::new();
    let mut calls: IndexMap<(String, String), u64> = IndexMap::new();
    let entry_qn = format!("{qual_prefix}.__entry__");
    symbols.push(Symbol {
        qualified_name: entry_qn.clone(),
        name: "__entry__".to_string(),
        kind: "entrypoint".to_string(),
        file_path: rel.to_string(),
        start_line: 1,
        end_line: x.lines.len().max(1),
        declaration: false,
    });
    let mut walks = vec![(root, entry_qn)];
    walks.extend(bodies);
    for (start, caller) in walks {
        // Cortex `walk_calls(node, caller)`: function definitions below
        // `start` belong to their own walk.
        let mut stack: Vec<Node> = kids(start).into_iter().rev().collect();
        while let Some(child) = stack.pop() {
            if child.kind() == "function_definition" {
                continue;
            }
            if child.kind() == "command" && !inside_expansion(child) {
                let name = x
                    .command_name(child)
                    .map(|n| x.literal(n))
                    .unwrap_or_default();
                if defined.contains(&name) {
                    *refs.entry(name.clone()).or_insert(0) += 1;
                    *calls.entry((caller.clone(), name)).or_insert(0) += 1;
                }
            }
            stack.extend(kids(child).into_iter().rev());
        }
    }

    let preamble_end = ranges
        .iter()
        .map(|(s, _)| *s as i64 - 1)
        .min()
        .unwrap_or(x.lines.len() as i64 - 1);
    if preamble_end >= 0 {
        x.add_chunk(0, preamble_end as usize, "module", "__entry__");
    }
    if x.chunks.is_empty() && !x.lines.is_empty() {
        let last = x.lines.len() - 1;
        x.add_chunk(0, last, "module", "__entry__");
    }
    let names: Vec<String> = symbols.iter().map(|s| s.name.clone()).collect();
    drop_own(&mut refs, &names);
    Some(Parsed {
        chunks: x.chunks,
        symbols,
        imports,
        refs,
        bindings: IndexMap::new(),
        calls,
        members: Vec::new(),
    })
}
