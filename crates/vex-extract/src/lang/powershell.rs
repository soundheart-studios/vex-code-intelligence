//! PowerShell: Cortex's `languages/powershell.py`. Functions, classes and
//! their methods, `using`/`Import-Module`/dot-sourcing as imports, command
//! calls (normalised to a local function's spelling), and for `.psd1`
//! manifests only the module imports and one manifest chunk.

use crate::ast::{named_children, text};
use crate::lang::adapter::{chunk, drop_own, lines_of};
use crate::text::{is_py_whitespace, path_name, path_suffix, py_strip};
use indexmap::IndexMap;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
use tree_sitter::Node;
use vex_core::{Chunk, Parsed, Symbol};

const MAX_CHUNK_LINES: usize = 120;

const SKIP_COMMANDS: &[&str] = &[
    "using",
    "return",
    "if",
    "else",
    "elseif",
    "foreach",
    "for",
    "while",
    "do",
    "switch",
    "try",
    "catch",
    "finally",
    "throw",
    "break",
    "continue",
    "exit",
    "param",
    "begin",
    "process",
    "end",
    "import-module",
];

static EXTENSION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.[^.]+$").expect("valid regex"));
static DOT_SOURCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*\.\s+").expect("valid regex"));
static DOT_SOURCE_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*\.\s+([^\s;]+)").expect("valid regex"));
static IMPORT_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:-Name\s+)?([^\s;]+)").expect("valid regex"));

fn unquote(s: &str) -> &str {
    s.trim_matches(|c| c == '\'' || c == '"')
}

/// `Foo\Bar.psm1` -> `Bar`.
fn module_name(raw: &str) -> String {
    let name = unquote(raw).replace('\\', "/");
    let last = name.rsplit('/').next().unwrap_or("");
    py_strip(&EXTENSION.replace(last, "")).to_string()
}

/// Python's `str.split()`.
fn py_split(s: &str) -> Vec<&str> {
    s.split(is_py_whitespace)
        .filter(|p| !p.is_empty())
        .collect()
}

struct Ps<'a> {
    rel: &'a str,
    src: &'a [u8],
    lines: Vec<String>,
    chunks: Vec<Chunk>,
    symbols: Vec<Symbol>,
    imports: Vec<String>,
    bindings: IndexMap<String, (String, String)>,
    seen: HashSet<(String, usize)>,
}

impl Ps<'_> {
    fn t(&self, node: Node) -> String {
        text(node, self.src)
    }

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
            "powershell",
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

    fn add_import(&mut self, raw: &str) {
        let name = module_name(raw);
        if !name.is_empty() {
            self.imports.push(name.clone());
            self.bindings.insert(name.clone(), (name, String::new()));
        }
    }

    fn command_name(&self, node: Node) -> String {
        named_children(node)
            .into_iter()
            .find(|c| c.kind() == "command_name")
            .map(|c| self.t(c))
            .unwrap_or_default()
    }

    /// Dot-sourcing, `using module ...` and `Import-Module` record an
    /// import; true when the command was one of them.
    fn command_import(&mut self, node: Node) -> bool {
        let raw = self.t(node);
        if DOT_SOURCE.is_match(&raw) {
            if let Some(m) = DOT_SOURCE_PATH.captures(&raw) {
                self.add_import(&m[1]);
            }
            return true;
        }
        let name = self.command_name(node);
        let lowered = name.to_lowercase();
        if lowered == "using" {
            let parts = py_split(&raw);
            if parts.len() >= 3
                && matches!(
                    parts[1].to_lowercase().as_str(),
                    "module" | "namespace" | "assembly"
                )
            {
                self.add_import(parts[2]);
            }
            return true;
        }
        if lowered == "import-module" {
            let rest: String = raw.chars().skip(name.chars().count()).collect();
            if let Some(m) = IMPORT_NAME.captures(py_strip(&rest)) {
                self.add_import(&m[1]);
            }
            return true;
        }
        false
    }
}

/// Cortex `script_body`: the first script block body under `node`, or
/// `node` itself (a recovered bare command stays visible to the calls).
fn script_body(node: Node) -> Node {
    let mut stack: Vec<Node> = named_children(node).into_iter().rev().collect();
    while let Some(child) = stack.pop() {
        if child.kind() == "script_block_body" {
            return child;
        }
        stack.extend(named_children(child).into_iter().rev());
    }
    node
}

fn string_literals(x: &Ps, node: Node) -> Vec<String> {
    let mut values = Vec::new();
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if current.kind() == "string_literal" {
            values.push(unquote(&x.t(current)).to_string());
        } else {
            stack.extend(named_children(current));
        }
    }
    values
}

fn named_child_of<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
    named_children(node).into_iter().find(|c| c.kind() == kind)
}

/// A `.psd1` manifest: RootModule, NestedModules and RequiredModules.
fn walk_manifest(x: &mut Ps, root: Node) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() != "hash_entry" {
            stack.extend(named_children(node).into_iter().rev());
            continue;
        }
        let key = named_child_of(node, "key_expression")
            .map(|k| py_strip(&x.t(k)).to_string())
            .unwrap_or_default();
        if !matches!(
            key.as_str(),
            "RootModule" | "NestedModules" | "RequiredModules"
        ) {
            continue;
        }
        let Some(value) = named_child_of(node, "pipeline") else {
            continue;
        };
        if key != "RequiredModules" {
            for raw in string_literals(x, value) {
                x.add_import(&raw);
            }
            continue;
        }
        // ModuleVersion strings live in nested hash entries: keep direct
        // strings plus explicit ModuleName values.
        let mut nested_offsets: HashSet<usize> = HashSet::new();
        let mut explicit: Vec<String> = Vec::new();
        let mut inner = named_children(value);
        while let Some(current) = inner.pop() {
            if current.kind() == "hash_entry" {
                let sub_key = named_child_of(current, "key_expression");
                if let Some(sub_value) = named_child_of(current, "pipeline") {
                    let mut nested = Vec::new();
                    let mut ns = vec![sub_value];
                    while let Some(item) = ns.pop() {
                        if item.kind() == "string_literal" {
                            nested.push(item);
                        } else {
                            ns.extend(named_children(item));
                        }
                    }
                    nested_offsets.extend(nested.iter().map(|n| n.start_byte()));
                    if sub_key.is_some_and(|k| py_strip(&x.t(k)) == "ModuleName") {
                        explicit.extend(nested.iter().map(|n| unquote(&x.t(*n)).to_string()));
                    }
                }
                continue;
            }
            inner.extend(named_children(current));
        }
        let mut direct = Vec::new();
        let mut scan = vec![value];
        while let Some(item) = scan.pop() {
            if item.kind() == "string_literal" {
                if !nested_offsets.contains(&item.start_byte()) {
                    direct.push(unquote(&x.t(item)).to_string());
                }
            } else {
                scan.extend(named_children(item));
            }
        }
        for raw in direct.iter().chain(explicit.iter()) {
            x.add_import(raw);
        }
    }
}

/// Python's `Path.stem`.
fn stem(rel: &str) -> &str {
    let name = path_name(rel);
    let suffix = path_suffix(rel);
    &name[..name.len() - suffix.len()]
}

/// Cortex `parse_powershell`. None when the grammar is not compiled in.
pub fn parse_powershell(rel: &str, source: &[u8]) -> Option<Parsed> {
    let tree = crate::engine::parse_tree("powershell", source)?;
    let mut x = Ps {
        rel,
        src: source,
        lines: lines_of(source),
        chunks: Vec::new(),
        symbols: Vec::new(),
        imports: Vec::new(),
        bindings: IndexMap::new(),
        seen: HashSet::new(),
    };
    let root = tree.root_node();
    if path_suffix(rel).to_lowercase() == ".psd1" {
        walk_manifest(&mut x, root);
        x.add_chunk(root, "manifest", stem(rel));
        return Some(Parsed {
            chunks: x.chunks,
            symbols: x.symbols,
            imports: x.imports,
            refs: IndexMap::new(),
            bindings: x.bindings,
            calls: IndexMap::new(),
            members: Vec::new(),
        });
    }

    let mut bodies: Vec<(Node, String)> = Vec::new();
    let mut stack: Vec<(Node, String)> = vec![(root, rel.replace('/', "."))];
    while let Some((node, scope)) = stack.pop() {
        match node.kind() {
            "function_statement" => {
                let Some(target) = named_child_of(node, "function_name") else {
                    continue;
                };
                let name = x.t(target);
                let qn = x.add_symbol(&scope, &name, "function", node);
                x.add_chunk(node, "function", &name);
                let body = script_body(node);
                bodies.push((body, qn));
                for c in named_children(body).into_iter().rev() {
                    stack.push((c, scope.clone()));
                }
                continue;
            }
            "class_statement" => {
                let Some(target) = named_child_of(node, "simple_name") else {
                    continue;
                };
                let name = x.t(target);
                let qn = x.add_symbol(&scope, &name, "class", node);
                x.add_chunk(node, "class", &name);
                for c in named_children(node).into_iter().rev() {
                    stack.push((c, qn.clone()));
                }
                continue;
            }
            "class_method_definition" => {
                let Some(target) = named_child_of(node, "simple_name") else {
                    continue;
                };
                let name = x.t(target);
                let qn = x.add_symbol(&scope, &name, "method", node);
                x.add_chunk(node, "method", &name);
                bodies.push((script_body(node), qn));
                continue;
            }
            "using_statement" => {
                let ids: Vec<String> = named_children(node)
                    .into_iter()
                    .filter(|c| {
                        matches!(c.kind(), "module_name" | "namespace_name" | "simple_name")
                    })
                    .map(|c| x.t(c))
                    .collect();
                if let Some(last) = ids.last() {
                    x.add_import(last);
                }
                continue;
            }
            "command" if x.command_import(node) => continue,
            _ => {}
        }
        for c in named_children(node).into_iter().rev() {
            stack.push((c, scope.clone()));
        }
    }

    // Later definitions win, as in Cortex's dict comprehension.
    let local: HashMap<String, String> = x
        .symbols
        .iter()
        .map(|s| (s.name.to_lowercase(), s.name.clone()))
        .collect();
    let mut refs: IndexMap<String, u64> = IndexMap::new();
    let mut calls: IndexMap<(String, String), u64> = IndexMap::new();
    for (body, caller) in &bodies {
        let mut stack = vec![*body];
        while let Some(node) = stack.pop() {
            let written = match node.kind() {
                "command" => {
                    let name = x.command_name(node);
                    let lowered = name.to_lowercase();
                    (!name.is_empty() && !SKIP_COMMANDS.contains(&lowered.as_str())).then_some(name)
                }
                "ERROR" => {
                    let parts: Vec<String> = named_children(node)
                        .into_iter()
                        .filter(|c| matches!(c.kind(), "block_name" | "simple_name"))
                        .map(|c| x.t(c))
                        .collect();
                    (parts.len() >= 2).then(|| parts.join("-"))
                }
                _ => None,
            };
            if let Some(w) = written {
                let normalized = local.get(&w.to_lowercase()).cloned().unwrap_or(w);
                *refs.entry(normalized.clone()).or_insert(0) += 1;
                *calls.entry((caller.clone(), normalized)).or_insert(0) += 1;
            }
            for c in named_children(node).into_iter().rev() {
                if !matches!(c.kind(), "function_statement" | "class_statement") {
                    stack.push(c);
                }
            }
        }
    }
    if x.chunks.is_empty() && !x.lines.is_empty() {
        x.add_chunk(root, "script", "");
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
