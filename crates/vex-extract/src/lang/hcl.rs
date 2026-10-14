//! Terraform/HCL: Cortex's `languages/hcl.py`. Top-level blocks are
//! addressed as Terraform does (`aws_s3_bucket.logs`, `var.region`,
//! `module.vpc`, `local.name`) and scoped by directory, as Terraform
//! modules are; traversals are refs and a module's `source` an import.

use crate::ast::{children, field, named_children, text};
use crate::lang::adapter::{chunk, lines_of};
use crate::text::py_strip;
use indexmap::IndexMap;
use std::collections::HashSet;
use tree_sitter::Node;
use vex_core::{Chunk, Parsed, Symbol};

const MAX_CHUNK_LINES: usize = 120;

fn unquote(s: &str) -> &str {
    s.trim_matches(|c| c == '\'' || c == '"')
}

struct Hcl<'a> {
    rel: &'a str,
    src: &'a [u8],
    directory: String,
    lines: Vec<String>,
    chunks: Vec<Chunk>,
    symbols: Vec<Symbol>,
    imports: Vec<String>,
    refs: IndexMap<String, u64>,
    seen: HashSet<(String, usize)>,
}

impl Hcl<'_> {
    fn t(&self, node: Node) -> String {
        text(node, self.src)
    }

    fn block_parts(&self, block: Node) -> (String, Vec<String>) {
        let mut block_type = String::new();
        let mut labels = Vec::new();
        for c in children(block) {
            if matches!(c.kind(), "block_start" | "body" | "block_end") {
                break;
            }
            if c.kind() == "identifier" && block_type.is_empty() {
                block_type = self.t(c);
            } else if matches!(c.kind(), "string_lit" | "identifier") {
                labels.push(unquote(py_strip(&self.t(c))).to_string());
            }
        }
        (block_type, labels)
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
        if let Some(c) = chunk(self.rel, &self.lines, start, end, kind, "hcl", Some(name)) {
            self.chunks.push(c);
        }
    }

    /// Every definition site, not just the first (ADR 19).
    fn add_symbol(&mut self, name: &str, kind: &str, node: Node) {
        let qn = format!("{}.{name}", self.directory);
        if !self.seen.insert((qn.clone(), node.start_position().row)) {
            return;
        }
        self.symbols.push(Symbol {
            qualified_name: qn,
            name: name.to_string(),
            kind: kind.to_string(),
            file_path: self.rel.to_string(),
            start_line: node.start_position().row + 1,
            end_line: node.end_position().row + 1,
            declaration: false,
        });
    }

    /// `var.x`, `local.x`, `module.x`, `data.t.n` or `type.name` from a
    /// variable and the attribute accesses that follow it.
    fn ref_address(&self, variable: Node) -> String {
        let head = self.t(variable);
        let mut attrs: Vec<String> = Vec::new();
        if let Some(parent) = variable.parent() {
            let mut after = false;
            for c in children(parent) {
                if c.id() == variable.id() {
                    after = true;
                    continue;
                }
                if after && c.kind() == "get_attr" {
                    match named_children(c)
                        .into_iter()
                        .find(|k| k.kind() == "identifier")
                    {
                        Some(ident) => attrs.push(self.t(ident)),
                        None => break,
                    }
                } else if after {
                    break;
                }
            }
        }
        if head.is_empty()
            || matches!(
                head.as_str(),
                "count" | "each" | "self" | "path" | "terraform"
            )
        {
            return String::new();
        }
        match (head.as_str(), attrs.len()) {
            ("var" | "local" | "module", n) if n >= 1 => format!("{head}.{}", attrs[0]),
            ("data", n) if n >= 2 => format!("data.{}.{}", attrs[0], attrs[1]),
            (_, n) if n >= 1 => format!("{head}.{}", attrs[0]),
            _ => String::new(),
        }
    }

    fn collect_refs(&mut self, node: Node) {
        let mut stack = vec![node];
        while let Some(n) = stack.pop() {
            if n.kind() == "variable_expr" {
                let r = self.ref_address(n);
                if !r.is_empty() {
                    *self.refs.entry(r).or_insert(0) += 1;
                }
            }
            stack.extend(named_children(n).into_iter().rev());
        }
    }

    fn source_import(&mut self, body: Node) {
        for attribute in named_children(body) {
            if attribute.kind() != "attribute" {
                continue;
            }
            let key = field(attribute, "key").or_else(|| attribute.named_child(0));
            if key.is_none_or(|k| self.t(k) != "source") {
                continue;
            }
            let raw = named_children(attribute)
                .into_iter()
                .find(|c| matches!(c.kind(), "expression" | "literal_value" | "string_lit"))
                .map(|c| unquote(&self.t(c)).to_string())
                .unwrap_or_default();
            if !raw.is_empty() {
                self.imports.push(raw);
            }
        }
    }
}

/// Terraform's address for a block, or "" when it has none.
fn address(block_type: &str, labels: &[String]) -> String {
    match (block_type, labels.len()) {
        ("resource", n) if n >= 2 => format!("{}.{}", labels[0], labels[1]),
        ("data", n) if n >= 2 => format!("data.{}.{}", labels[0], labels[1]),
        ("module", n) if n >= 1 => format!("module.{}", labels[0]),
        ("variable", n) if n >= 1 => format!("var.{}", labels[0]),
        ("output", n) if n >= 1 => format!("output.{}", labels[0]),
        ("provider", n) if n >= 1 => format!("provider.{}", labels[0]),
        _ => String::new(),
    }
}

/// Cortex `parse_hcl`. None when the grammar is not compiled in.
pub fn parse_hcl(rel: &str, source: &[u8]) -> Option<Parsed> {
    let tree = crate::engine::parse_tree("hcl", source)?;
    let directory = match rel.rfind('/') {
        Some(i) => rel[..i].replace('/', "."),
        None => String::new(),
    };
    let mut x = Hcl {
        rel,
        src: source,
        directory: if directory.is_empty() || directory == "." {
            "tf".to_string()
        } else {
            directory
        },
        lines: lines_of(source),
        chunks: Vec::new(),
        symbols: Vec::new(),
        imports: Vec::new(),
        refs: IndexMap::new(),
        seen: HashSet::new(),
    };
    let root = tree.root_node();
    let body = named_children(root)
        .into_iter()
        .find(|c| c.kind() == "body")
        .unwrap_or(root);
    for block in named_children(body) {
        if block.kind() != "block" {
            continue;
        }
        let (block_type, labels) = x.block_parts(block);
        let block_body = named_children(block)
            .into_iter()
            .find(|c| c.kind() == "body");
        if block_type == "locals"
            && let Some(bb) = block_body
        {
            for attribute in named_children(bb) {
                let Some(first) = attribute
                    .named_child(0)
                    .filter(|_| attribute.kind() == "attribute")
                else {
                    continue;
                };
                let name = format!("local.{}", x.t(first));
                x.add_symbol(&name, "local", attribute);
                x.add_chunk(attribute, "local", &name);
                x.collect_refs(attribute);
            }
            continue;
        }
        let name = address(&block_type, &labels);
        if name.is_empty() {
            continue;
        }
        x.add_symbol(&name, &block_type, block);
        x.add_chunk(block, &block_type, &name);
        if let Some(bb) = block_body {
            x.collect_refs(bb);
            if block_type == "module" {
                x.source_import(bb);
            }
        }
    }
    Some(Parsed {
        chunks: x.chunks,
        symbols: x.symbols,
        imports: x.imports,
        refs: x.refs,
        bindings: IndexMap::new(),
        calls: IndexMap::new(),
        members: Vec::new(),
    })
}
