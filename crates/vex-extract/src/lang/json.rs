//! JSON: Cortex's `languages/json.py`. Only config and manifest files
//! (known names, or a top-level key such as `dependencies`) get structure:
//! one chunk per top-level key, a symbol per key path (bounded in depth and
//! count), and dependencies, `extends`, `$ref` and `$schema` as imports.
//! Anything else is data and returns None (line chunking takes over).

use crate::ast::{field, named_children, text};
use crate::lang::adapter::{chunk, lines_of};
use crate::text::path_name;
use indexmap::IndexMap;
use std::collections::HashSet;
use tree_sitter::Node;
use vex_core::ids::chunk_id;
use vex_core::{Chunk, Parsed, Symbol};

const CONFIG_NAMES: &[&str] = &[
    "package.json",
    "tsconfig.json",
    "jsconfig.json",
    "composer.json",
    "deno.json",
    "bower.json",
    "manifest.json",
    "app.json",
    "vercel.json",
    "angular.json",
    "nest-cli.json",
    "biome.json",
    "renovate.json",
    ".babelrc",
    ".babelrc.json",
    ".eslintrc.json",
    ".prettierrc.json",
    ".prettierrc",
    "babel.config.json",
];
const CONFIG_SUFFIXES: &[&str] = &[
    ".eslintrc.json",
    ".prettierrc.json",
    ".babelrc.json",
    "tsconfig.json",
    "jsconfig.json",
];
const DEPENDENCY_KEYS: &[&str] = &[
    "dependencies",
    "devDependencies",
    "peerDependencies",
    "optionalDependencies",
    "bundleDependencies",
    "bundledDependencies",
];
const MAX_DEPTH: usize = 6;
const MAX_KEYS: usize = 500;
const MAX_CHUNK_LINES: usize = 120;

fn is_config_key(key: &str) -> bool {
    DEPENDENCY_KEYS.contains(&key)
        || matches!(key, "extends" | "$ref" | "$schema" | "compilerOptions")
}

/// Python's `str.casefold()` for a file name: lower-casing, with the
/// multi-letter folds that could matter for ASCII comparisons.
fn casefold(s: &str) -> String {
    s.to_lowercase().replace('ß', "ss")
}

struct Json<'a> {
    rel: &'a str,
    src: &'a [u8],
    qual_prefix: String,
    symbols: Vec<Symbol>,
    imports: Vec<String>,
    seen: HashSet<(String, usize)>,
    key_count: usize,
}

impl Json<'_> {
    fn string_value(&self, node: Node) -> String {
        match field(node, "string_content") {
            Some(c) => text(c, self.src),
            None => text(node, self.src)
                .trim_matches(|c| c == '\'' || c == '"')
                .to_string(),
        }
    }

    fn key_of(&self, pair: Node) -> String {
        field(pair, "key")
            .map(|k| self.string_value(k))
            .unwrap_or_default()
    }

    fn walk_object(&mut self, obj: Node, parents: &[String], depth: usize) {
        if depth > MAX_DEPTH || self.key_count >= MAX_KEYS {
            return;
        }
        for pair in named_children(obj) {
            if pair.kind() != "pair" || self.key_count >= MAX_KEYS {
                continue;
            }
            let key = self.key_of(pair);
            if key.is_empty() {
                continue;
            }
            self.key_count += 1;
            let mut path_parts = parents.to_vec();
            path_parts.push(key.clone());
            let qn = format!("{}.{}", self.qual_prefix, path_parts.join("/"));
            let row = pair.start_position().row;
            // Every site is its own symbol (ADR 19).
            if self.seen.insert((qn.clone(), row)) {
                self.symbols.push(Symbol {
                    qualified_name: qn,
                    name: key.clone(),
                    kind: "config_key".to_string(),
                    file_path: self.rel.to_string(),
                    start_line: row + 1,
                    end_line: pair.end_position().row + 1,
                    declaration: false,
                });
            }
            let Some(value) = field(pair, "value") else {
                continue;
            };
            let parent_key = parents.last().map(String::as_str).unwrap_or("");
            match value.kind() {
                "object" => self.walk_object(value, &path_parts, depth + 1),
                "string" => {
                    let value_text = self.string_value(value);
                    if DEPENDENCY_KEYS.contains(&parent_key) && !value_text.is_empty() {
                        self.imports.push(key);
                    } else if matches!(key.as_str(), "extends" | "$ref" | "$schema")
                        && !value_text.is_empty()
                    {
                        self.imports.push(value_text);
                    }
                }
                "array" if key == "extends" => {
                    for item in named_children(value) {
                        if item.kind() == "string" {
                            let v = self.string_value(item);
                            if !v.is_empty() {
                                self.imports.push(v);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

/// Cortex `parse_json`: structural chunks for a config file, or None for
/// data-shaped JSON (and when the grammar is not compiled in).
pub fn parse_json(rel: &str, source: &[u8]) -> Option<Parsed> {
    let tree = crate::engine::parse_tree("json", source)?;
    let mut document = tree.root_node();
    if document.kind() == "document"
        && let Some(first) = document.named_child(0)
    {
        document = first;
    }
    if document.kind() != "object" {
        return None;
    }
    let mut x = Json {
        rel,
        src: source,
        qual_prefix: rel.replace('/', "."),
        symbols: Vec::new(),
        imports: Vec::new(),
        seen: HashSet::new(),
        key_count: 0,
    };
    let top_pairs: Vec<Node> = named_children(document)
        .into_iter()
        .filter(|c| c.kind() == "pair")
        .collect();
    let name = casefold(path_name(rel));
    let recognized = CONFIG_NAMES.contains(&name.as_str())
        || CONFIG_SUFFIXES.iter().any(|s| name.ends_with(s))
        || top_pairs.iter().any(|p| is_config_key(&x.key_of(*p)));
    if !recognized {
        return None;
    }
    let lines = lines_of(source);
    let mut chunks: Vec<Chunk> = Vec::new();
    for pair in &top_pairs {
        let key = x.key_of(*pair);
        if key.is_empty() {
            continue;
        }
        let start = pair.start_position().row;
        let end = pair
            .end_position()
            .row
            .min(start + MAX_CHUNK_LINES)
            .min(lines.len().saturating_sub(1));
        if let Some(c) = chunk(rel, &lines, start, end, "config", "json", Some(&key)) {
            chunks.push(c);
        }
    }
    x.walk_object(document, &[], 0);
    if chunks.is_empty() && !lines.is_empty() {
        let n = lines.len().min(MAX_CHUNK_LINES + 1);
        let content = lines[..n].join("\n");
        chunks.push(Chunk::with_metadata(
            chunk_id(rel, 0, &content),
            rel,
            1,
            n,
            content,
            "config",
            "json",
            Default::default(),
        ));
    }
    Some(Parsed {
        chunks,
        symbols: x.symbols,
        imports: x.imports,
        refs: IndexMap::new(),
        bindings: IndexMap::new(),
        calls: IndexMap::new(),
        members: Vec::new(),
    })
}
