//! The AST extraction engine: a port of Cortex's `astchunk.parse_file` and
//! the helpers it shares across languages.
//!
//! The structure mirrors the Python closely enough to be read side by side:
//! `declarations` is the walk over declaration containers, `visit_def` turns
//! one definition into a symbol and a chunk and recurses, and
//! `collect_refs_and_imports` gathers calls, references and imports.
//! Language-specific branches dispatch on the language name exactly where
//! Cortex's do; a language is only wired in once its golden files match.

use crate::text::{decode_replace, is_py_whitespace, py_strip, splitlines};
use indexmap::IndexMap;
use std::collections::{BTreeMap, HashMap};
use tree_sitter::{Node, Tree};
use vex_core::ids::chunk_id;
use vex_core::{Chunk, Parsed, Symbol};

/// Oversized definitions are truncated to this many lines in their chunk.
pub const MAX_CHUNK_LINES: usize = 120;

/// Per-language node configuration (Cortex `_CONFIG`).
pub struct Config {
    /// Definition node type -> symbol kind (`@wrapped` for wrappers).
    pub defs: &'static [(&'static str, &'static str)],
    pub calls: &'static [&'static str],
    pub class_nodes: &'static [&'static str],
    pub imports: &'static [&'static str],
    /// Keep receivers in call names (`obj.run`).
    pub qualified: bool,
}

impl Config {
    pub fn def_kind(&self, node_type: &str) -> Option<&'static str> {
        self.defs
            .iter()
            .find(|(t, _)| *t == node_type)
            .map(|(_, k)| *k)
    }
    pub fn is_def(&self, node_type: &str) -> bool {
        self.def_kind(node_type).is_some()
    }
}

/// Wrappers between a statement and the declaration inside it.
const WRAPPERS: &[&str] = &[
    "decorated_definition",
    "export_statement",
    "export_default_declaration",
];
/// Initialisers that make a binding a definition rather than a value.
const FUNCTION_VALUES: &[&str] = &["arrow_function", "function_expression", "function"];
const IDENT_TYPES: &[&str] = &[
    "identifier",
    "field_identifier",
    "simple_identifier",
    "package_identifier",
    "type_identifier",
    "property_identifier",
];
const HASH_COMMENT_LANGUAGES: &[&str] =
    &["python", "ruby", "elixir", "julia", "bash", "powershell"];
const ANNOTATION_LANGUAGES: &[&str] = &[
    "java",
    "kotlin",
    "swift",
    "scala",
    "groovy",
    "typescript",
    "tsx",
    "javascript",
    "csharp",
    "php",
];

/// Python `str.isidentifier()`.
pub fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c == '_' || unicode_ident::is_xid_start(c) => {
            chars.all(unicode_ident::is_xid_continue)
        }
        _ => false,
    }
}

/// `node.text.decode(errors="replace")`.
pub fn text(node: Node, src: &[u8]) -> String {
    decode_replace(&src[node.byte_range()])
}

pub fn named_children<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

pub fn children<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    node.children(&mut cursor).collect()
}

pub fn field<'t>(node: Node<'t>, name: &str) -> Option<Node<'t>> {
    node.child_by_field_name(name)
}

pub fn is_ident(node: Node) -> bool {
    IDENT_TYPES.contains(&node.kind()) || node.kind().contains("identifier")
}

/// Cortex `_name_of`.
pub fn name_of(node: Node, src: &[u8]) -> Option<String> {
    if node.kind() == "impl_item"
        && let Some(t) = field(node, "type")
    {
        return Some(text(t, src));
    }
    if matches!(node.kind(), "destructor_name" | "operator_name") {
        return Some(text(node, src));
    }
    if let Some(n) = field(node, "name") {
        return Some(text(n, src));
    }
    if let Some(d) = field(node, "declarator") {
        return if is_ident(d) {
            Some(text(d, src))
        } else {
            name_of(d, src)
        };
    }
    for c in named_children(node) {
        if node.kind() == "reference_declarator" && c.kind().ends_with("declarator") {
            return name_of(c, src);
        }
        if matches!(c.kind(), "type_spec" | "type_identifier") {
            return Some(name_of(c, src).unwrap_or_else(|| text(c, src)));
        }
        if is_ident(c) {
            return Some(text(c, src));
        }
    }
    None
}

/// Cortex `_idents_in`: identifier leaves in source order, at most `limit`.
pub fn idents_in(node: Node, src: &[u8], limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack: std::collections::VecDeque<Node> = std::collections::VecDeque::from([node]);
    while out.len() < limit {
        let Some(n) = stack.pop_front() else { break };
        if is_ident(n) && n.named_child_count() == 0 {
            out.push(text(n, src));
        } else {
            for c in named_children(n).into_iter().rev() {
                stack.push_front(c);
            }
        }
    }
    out
}

/// Cortex `_call_name`.
pub fn call_name(node: Node, src: &[u8], qualified: bool) -> Option<String> {
    let mut fn_ = field(node, "function")
        .or_else(|| field(node, "name"))
        .or_else(|| field(node, "method"));
    if fn_.is_none() && node.named_child_count() > 0 {
        fn_ = node.named_child(0);
    }
    let mut f = fn_?;
    if node.kind() == "function_call_expression" {
        if matches!(f.kind(), "name" | "qualified_name" | "relative_name") {
            return Some(
                text(f, src)
                    .replace('\\', ".")
                    .trim_start_matches('.')
                    .to_string(),
            );
        }
        return Some("(dynamic).(call)".into());
    }
    if node.kind() == "scoped_call_expression" {
        if let Some(scope) = field(node, "scope")
            .filter(|s| matches!(s.kind(), "name" | "qualified_name" | "relative_scope"))
        {
            return Some(format!(
                "{}.{}",
                text(scope, src).replace('\\', "."),
                text(f, src)
            ));
        }
        return Some(format!("(dynamic).{}", text(f, src)));
    }
    if qualified {
        if matches!(f.kind(), "subscript" | "subscript_expression") {
            return Some("(dynamic).(computed)".into());
        }
        let node_obj = field(node, "object");
        let obj = node_obj.or_else(|| field(f, "object"));
        let member = if node_obj.is_some() {
            field(node, "name")
        } else {
            field(f, "attribute").or_else(|| field(f, "property"))
        };
        if let (Some(obj), Some(member)) = (obj, member) {
            let spelling = text(member, src);
            let base = if is_ident(member) || is_identifier(&spelling) {
                spelling
            } else {
                "(computed)".to_string()
            };
            return Some(format!(
                "{}.{base}",
                receiver_name(obj, src).unwrap_or_else(|| "(dynamic)".into())
            ));
        }
        let source_node = if node_obj.is_some() { node } else { f };
        let mut idents = idents_in(source_node, src, 6);
        if let (Some(obj_n), Some(name_n)) = (node_obj, field(node, "name")) {
            let obj_ids = idents_in(obj_n, src, 6);
            idents = obj_ids
                .last()
                .cloned()
                .into_iter()
                .chain([text(name_n, src)])
                .collect();
        }
        if idents.len() >= 2 {
            return Some(idents[idents.len() - 2..].join("."));
        }
        return idents.last().cloned();
    }
    while f.named_child_count() > 0 {
        let next = field(f, "attribute")
            .or_else(|| field(f, "property"))
            .or_else(|| field(f, "field"))
            .or_else(|| field(f, "name"));
        match next {
            Some(n) => f = n,
            None => return idents_in(f, src, 6).last().cloned(),
        }
    }
    is_ident(f).then(|| text(f, src))
}

/// The receiver chain of a qualified call: identifiers, `this`/`super` and
/// `a.b` member chains; None for a computed receiver.
fn receiver_name(receiver: Node, src: &[u8]) -> Option<String> {
    if (is_ident(receiver) && receiver.named_child_count() == 0)
        || matches!(receiver.kind(), "this" | "super")
    {
        return Some(text(receiver, src));
    }
    if receiver.kind() == "variable_name" {
        let name = text(receiver, src);
        let name = name.strip_prefix('$').unwrap_or(&name).to_string();
        return is_identifier(&name).then_some(name);
    }
    let obj = field(receiver, "object");
    let member = field(receiver, "attribute").or_else(|| field(receiver, "property"));
    if let (Some(obj), Some(member)) = (obj, member)
        && is_ident(member)
        && let Some(prefix) = receiver_name(obj, src)
    {
        return Some(format!("{prefix}.{}", text(member, src)));
    }
    None
}

/// Cortex `_function_binding` (JS/TS family only).
fn function_binding<'t>(node: Node<'t>, cfg: &Config) -> Option<Node<'t>> {
    if !cfg.is_def("variable_declarator")
        || !matches!(node.kind(), "lexical_declaration" | "variable_declaration")
    {
        return None;
    }
    let declarators: Vec<Node> = named_children(node)
        .into_iter()
        .filter(|c| c.kind() == "variable_declarator")
        .collect();
    if declarators.len() != 1 {
        return None;
    }
    let value = field(declarators[0], "value")?;
    FUNCTION_VALUES
        .contains(&value.kind())
        .then_some(declarators[0])
}

/// Cortex `_unwrap`: descend through a wrapper to the declaration it wraps.
pub fn unwrap<'t>(node: Node<'t>, cfg: &Config) -> Node<'t> {
    if !WRAPPERS.contains(&node.kind()) {
        return function_binding(node, cfg).unwrap_or(node);
    }
    let d = field(node, "definition").or_else(|| field(node, "declaration"));
    if let Some(d) = d.filter(|d| cfg.is_def(d.kind())) {
        return d;
    }
    for child in d.into_iter().chain(named_children(node)) {
        if cfg.is_def(child.kind()) {
            return child;
        }
        if let Some(bound) = function_binding(child, cfg) {
            return bound;
        }
    }
    node
}

/// Cortex `_body_children`.
pub fn body_children<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    let body = field(node, "body").or_else(|| {
        named_children(node).into_iter().find(|c| {
            c.kind().ends_with("body")
                || matches!(
                    c.kind(),
                    "declaration_list" | "block" | "field_declaration_list"
                )
        })
    });
    named_children(body.unwrap_or(node))
}

/// Cortex `_is_leading_line`: a comment or attribute line documenting the
/// declaration below it.
pub fn is_leading_line(text: &str, language: &str) -> bool {
    if ["//", "/*", "*", "*/"].iter().any(|p| text.starts_with(p)) {
        return true;
    }
    if language == "rust" && (text.starts_with("#[") || text.starts_with("#![")) {
        return true;
    }
    if language == "csharp" && text.starts_with('[') {
        return true;
    }
    if ANNOTATION_LANGUAGES.contains(&language) && text.starts_with('@') {
        return true;
    }
    if language == "lua" && text.starts_with("--") {
        return true;
    }
    HASH_COMMENT_LANGUAGES.contains(&language) && text.starts_with('#')
}

/// Template-argument depth before each character; -1 from an `operator`
/// keyword on, whose `<`, `>` and `()` nest nothing (Cortex `_cpp_depths`).
pub fn cpp_depths(name: &str) -> Vec<i64> {
    let chars: Vec<char> = name.chars().collect();
    let end = find_word(&chars, "operator").unwrap_or(chars.len());
    let mut depths = Vec::with_capacity(chars.len());
    let mut depth = 0i64;
    for (i, &ch) in chars.iter().enumerate() {
        depths.push(if i < end { depth } else { -1 });
        if i < end {
            depth += i64::from(ch == '<') - i64::from(ch == '>');
        }
    }
    depths
}

/// Index of the first `\bword\b` match (Python `re` word boundaries).
fn find_word(chars: &[char], word: &str) -> Option<usize> {
    let w: Vec<char> = word.chars().collect();
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    (0..=chars.len().saturating_sub(w.len())).find(|&i| {
        chars.len() >= w.len()
            && chars[i..i + w.len()] == w[..]
            && (i == 0 || !is_word(chars[i - 1]))
            && chars.get(i + w.len()).is_none_or(|&c| !is_word(c))
    })
}

/// Cortex `_top_level_scopes`: split at `::` outside template arguments.
pub fn top_level_scopes(name: &str) -> Vec<String> {
    let chars: Vec<char> = name.chars().collect();
    let depths = cpp_depths(name);
    let mut parts = Vec::new();
    let (mut start, mut i) = (0usize, 0usize);
    while i + 1 < chars.len() {
        if depths[i] == 0 && chars[i] == ':' && chars[i + 1] == ':' {
            parts.push(chars[start..i].iter().collect());
            i += 2;
            start = i;
            continue;
        }
        i += 1;
    }
    parts.push(chars[start..].iter().collect());
    parts
}

/// `re.sub(r"\s*::\s*", "::", name)`.
fn collapse_scope_whitespace(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let chars: Vec<char> = name.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        // Find a run of whitespace, `::`, whitespace starting here.
        let mut j = i;
        while j < chars.len() && is_py_whitespace(chars[j]) {
            j += 1;
        }
        if j + 1 < chars.len() && chars[j] == ':' && chars[j + 1] == ':' {
            let mut k = j + 2;
            while k < chars.len() && is_py_whitespace(chars[k]) {
                k += 1;
            }
            out.push_str("::");
            i = k;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// All extraction state for one file (the locals of Cortex's `parse_file`).
pub struct Extractor<'a> {
    pub rel: &'a str,
    pub language: &'a str,
    pub src: &'a [u8],
    pub cfg: &'a Config,
    pub lines: Vec<String>,
    pub chunk_language: &'a str,
    pub qual_prefix: String,
    pub chunks: Vec<Chunk>,
    pub symbols: Vec<Symbol>,
    pub refs: IndexMap<String, u64>,
    pub imports: Vec<String>,
    pub bindings: IndexMap<String, (String, String)>,
    pub call_sites: Vec<(String, usize)>,
    pub caller_spans: Vec<(usize, usize, String)>,
    /// Python constructor evidence per function (keyed by start byte).
    pub constructor_cache: HashMap<usize, HashMap<String, (String, usize)>>,
    /// Go receiver evidence per method (keyed by start byte).
    pub go_receivers: HashMap<usize, Option<(String, String)>>,
    /// Declared local types per typed scope (keyed by start byte).
    pub declared_types: HashMap<usize, HashMap<String, Option<String>>>,
    /// Binding expressions per typed scope (keyed by start byte).
    pub binding_cache: HashMap<usize, IndexMap<String, serde_json::Value>>,
    /// C++ local types per function (keyed by start byte).
    pub cpp_locals: HashMap<usize, HashMap<String, std::collections::HashSet<Option<String>>>>,
    /// C++ class -> field -> declared type, computed once on demand.
    pub cpp_fields: Option<HashMap<String, HashMap<String, Option<String>>>>,
    /// C++ owner -> the namespace scope it lives in (`"" -> ""` seeded).
    pub cpp_namespace_scopes: HashMap<String, String>,
}

impl<'a> Extractor<'a> {
    fn leading_start(&self, mut row: usize) -> usize {
        while row > 0 {
            let line = if row - 1 < self.lines.len() {
                py_strip(&self.lines[row - 1])
            } else {
                ""
            };
            if line.is_empty() || !is_leading_line(line, self.language) {
                break;
            }
            row -= 1;
        }
        row
    }

    fn add_chunk(&mut self, start: usize, end: usize, kind: &str, names: &[String]) {
        let hi = (end + 1).min(self.lines.len());
        let content = if start < hi {
            self.lines[start..hi].join("\n")
        } else {
            String::new()
        };
        if py_strip(&content).is_empty() {
            return;
        }
        let mut meta = BTreeMap::new();
        meta.insert("symbols".to_string(), names.join(" ").into());
        let id = chunk_id(self.rel, start, &content);
        self.chunks.push(Chunk::with_metadata(
            id,
            self.rel,
            start + 1,
            end + 1,
            content,
            kind,
            self.chunk_language,
            meta,
        ));
    }

    fn js_family(&self) -> bool {
        matches!(self.language, "javascript" | "typescript" | "tsx")
    }

    /// Cortex `declarations`: definitions under `children`, with their owner.
    /// The JS family walks arbitrary expressions too (callbacks, object
    /// literals, class fields); Python only control-flow blocks.
    fn declarations<'t>(
        &mut self,
        nodes: Vec<Node<'t>>,
        parent_name: &str,
    ) -> Vec<(Node<'t>, String)> {
        let js = self.js_family();
        let mut out = Vec::new();
        // A file-scoped namespace owns every sibling after it.
        let mut parent_name = parent_name.to_string();
        let parent_name = &mut parent_name;
        for child in nodes {
            let node = unwrap(child, self.cfg);
            if self.language == "kotlin"
                && crate::lang::kotlin::recovered_class(node, self.src).is_some()
            {
                out.push((child, parent_name.clone()));
                continue;
            }
            if self.language == "php" && node.kind() == "namespace_definition" {
                let name = name_of(node, self.src)
                    .unwrap_or_default()
                    .replace('\\', ".");
                match field(node, "body") {
                    // `namespace Foo;` owns every sibling after it.
                    None => *parent_name = name,
                    Some(body) => out.extend(self.declarations(named_children(body), &name)),
                }
                continue;
            }
            if self.language == "ruby" && node.kind() == "singleton_class" {
                // `class << self` opens the enclosing class's singleton: its
                // methods are class methods of that class.
                out.extend(self.declarations(body_children(node), parent_name));
                continue;
            }
            if self.language == "csharp"
                && matches!(
                    node.kind(),
                    "namespace_declaration" | "file_scoped_namespace_declaration"
                )
            {
                let owner = match (parent_name.is_empty(), name_of(node, self.src)) {
                    (false, Some(n)) => format!("{parent_name}.{n}"),
                    (false, None) => format!("{parent_name}.None"),
                    (true, Some(n)) => n,
                    (true, None) => String::new(),
                };
                let owner = if owner.is_empty() {
                    parent_name.clone()
                } else {
                    owner
                };
                if node.kind() == "file_scoped_namespace_declaration" {
                    *parent_name = owner;
                } else {
                    out.extend(self.declarations(body_children(node), &owner));
                }
                continue;
            }
            if self.language == "csharp"
                && matches!(node.kind(), "preproc_if" | "preproc_elif" | "preproc_else")
            {
                // Every branch is kept, as for C: no build configuration
                // chooses one.
                out.extend(self.declarations(named_children(node), parent_name));
                continue;
            }
            if self.language == "swift"
                && node.kind() == "class_declaration"
                && children(node).iter().any(|c| c.kind() == "extension")
            {
                let owner = match (parent_name.is_empty(), name_of(node, self.src)) {
                    (false, Some(n)) => format!("{parent_name}.{n}"),
                    (false, None) => format!("{parent_name}.None"),
                    (true, Some(n)) => n,
                    (true, None) => String::new(),
                };
                let owner = if owner.is_empty() {
                    parent_name.clone()
                } else {
                    owner
                };
                out.extend(self.declarations(body_children(node), &owner));
                continue;
            }
            if self.language == "lua" && node.kind() != "function_declaration" {
                // Bindings by assignment (`M.f = function`, `local f =
                // function`, table fields) are definitions; tables are walked
                // with the bound name as owner; data values yield nothing.
                match node.kind() {
                    "variable_declaration" | "return_statement" | "expression_list" => {
                        out.extend(self.declarations(named_children(node), parent_name));
                    }
                    "assignment_statement" | "field" => {
                        if let Some((target, value)) = crate::lang::lua::binding(node) {
                            if value.kind() == "function_definition" {
                                out.push((child, parent_name.clone()));
                            } else if value.kind() == "table_constructor"
                                && target.kind() == "identifier"
                            {
                                let name = text(target, self.src);
                                let owner = if parent_name.is_empty() {
                                    name
                                } else {
                                    format!("{parent_name}.{name}")
                                };
                                out.extend(self.declarations(named_children(value), &owner));
                            }
                        }
                    }
                    "table_constructor" => {
                        out.extend(self.declarations(named_children(node), parent_name));
                    }
                    _ => {}
                }
                continue;
            }
            if js
                && matches!(node.kind(), "lexical_declaration" | "variable_declaration")
                && named_children(node)
                    .iter()
                    .filter(|c| c.kind() == "variable_declarator")
                    .count()
                    > 1
            {
                // Ambiguous multi-bindings are left alone.
                continue;
            }
            if js && node.kind() == "variable_declarator" {
                match field(node, "value") {
                    Some(v) if FUNCTION_VALUES.contains(&v.kind()) => {
                        out.push((child, parent_name.to_string()));
                    }
                    Some(v) => {
                        // Python: f"{parent_name}.{name}" if parent_name else name,
                        // then `owner or parent_name`.
                        let name = name_of(node, self.src);
                        let owner = match (parent_name.is_empty(), name) {
                            (false, Some(n)) => format!("{parent_name}.{n}"),
                            (false, None) => format!("{parent_name}.None"),
                            (true, Some(n)) => n,
                            (true, None) => String::new(),
                        };
                        let owner = if owner.is_empty() {
                            parent_name.to_string()
                        } else {
                            owner
                        };
                        out.extend(self.declarations(vec![v], &owner));
                    }
                    None => {}
                }
            } else if self.cfg.is_def(node.kind())
                || (js
                    && matches!(
                        node.kind(),
                        "class" | "function_expression" | "function" | "arrow_function"
                    ))
            {
                out.push((child, parent_name.to_string()));
            } else if js && node.kind() != "ERROR" {
                if matches!(
                    node.kind(),
                    "internal_module"
                        | "module"
                        | "pair"
                        | "public_field_definition"
                        | "field_definition"
                ) {
                    let key = field(node, "name")
                        .or_else(|| field(node, "key"))
                        .or_else(|| field(node, "property"));
                    let value = field(node, "value").or_else(|| field(node, "body"));
                    match (key, value) {
                        (Some(k), Some(v)) => {
                            let name = text(k, self.src);
                            let name = name.trim_matches(|c| c == '"' || c == '\'');
                            let owner = if parent_name.is_empty() {
                                name.to_string()
                            } else {
                                format!("{parent_name}.{name}")
                            };
                            out.extend(self.declarations(vec![v], &owner));
                        }
                        _ => out.extend(self.declarations(named_children(node), parent_name)),
                    }
                } else {
                    out.extend(self.declarations(named_children(node), parent_name));
                }
            } else if self.language == "java" && node.kind() != "ERROR" {
                // Anonymous classes have no name: each body gets a
                // source-position owner, same-line siblings included.
                if node.kind() == "object_creation_expression" {
                    for member in named_children(node) {
                        let owner = if member.kind() == "class_body" {
                            let p = node.start_position();
                            format!("{parent_name}.(anonymous@{}:{})", p.row + 1, p.column + 1)
                        } else {
                            parent_name.to_string()
                        };
                        out.extend(self.declarations(vec![member], &owner));
                    }
                } else {
                    out.extend(self.declarations(named_children(node), parent_name));
                }
            } else if self.language == "c"
                && (matches!(
                    node.kind(),
                    "type_definition"
                        | "declaration"
                        | "preproc_if"
                        | "preproc_ifdef"
                        | "preproc_else"
                        | "linkage_specification"
                        | "declaration_list"
                ) || (node.kind() == "ERROR" && opens_extern_c(node)))
            {
                out.extend(self.declarations(named_children(node), parent_name));
            } else if self.language == "cpp" {
                out.extend(self.cpp_declarations(node, parent_name));
            } else if self.language == "rust" && node.kind() == "foreign_mod_item" {
                out.extend(self.declarations(body_children(node), parent_name));
            } else if (self.language == "go"
                && matches!(node.kind(), "type_declaration" | "interface_type"))
                || self.language == "python"
                    && matches!(
                        node.kind(),
                        "if_statement"
                            | "elif_clause"
                            | "else_clause"
                            | "for_statement"
                            | "while_statement"
                            | "try_statement"
                            | "except_clause"
                            | "finally_clause"
                            | "with_statement"
                            | "match_statement"
                            | "case_clause"
                            | "block"
                    )
            {
                out.extend(self.declarations(named_children(node), parent_name));
            }
        }
        out
    }

    /// The C++ arm of Cortex `declarations` for a node that is not a
    /// definition: namespaces add ownership (unnamed ones `(anonymous)`), a
    /// friend defined in a class is a namespace function, and preprocessor
    /// alternatives, templates, linkage blocks and statements are walked.
    fn cpp_declarations<'t>(
        &mut self,
        node: Node<'t>,
        parent_name: &str,
    ) -> Vec<(Node<'t>, String)> {
        match node.kind() {
            "namespace_definition" => {
                let name = field(node, "name")
                    .map(|n| text(n, self.src).replace("::", "."))
                    .unwrap_or_else(|| "(anonymous)".to_string());
                let owner = if parent_name.is_empty() {
                    name
                } else {
                    format!("{parent_name}.{name}")
                };
                self.cpp_namespace_scopes
                    .insert(owner.clone(), owner.clone());
                self.declarations(body_children(node), &owner)
            }
            "friend_declaration" => {
                let scope = self
                    .cpp_namespace_scopes
                    .get(parent_name)
                    .cloned()
                    .unwrap_or_default();
                self.declarations(named_children(node), &scope)
            }
            "preproc_if"
            | "preproc_ifdef"
            | "preproc_else"
            | "preproc_elif"
            | "preproc_elifdef"
            | "template_declaration"
            | "linkage_specification"
            | "declaration_list"
            | "declaration"
            | "field_declaration"
            | "compound_statement"
            | "if_statement"
            | "else_clause"
            | "for_statement"
            | "for_range_loop"
            | "while_statement"
            | "do_statement"
            | "switch_statement"
            | "case_statement"
            | "try_statement"
            | "catch_clause" => self.declarations(named_children(node), parent_name),
            _ => Vec::new(),
        }
    }

    /// Cortex `_emit_enum_members`: each member's own identifier is a symbol
    /// under the enum.
    fn emit_enum_members(&mut self, node: Node, owner: &str) {
        let body = field(node, "body").or_else(|| {
            named_children(node).into_iter().find(|c| {
                (c.kind().ends_with("_list") || c.kind().ends_with("_body"))
                    && c.kind() != "attribute_list"
            })
        });
        let Some(body) = body else { return };
        for member in named_children(body) {
            if matches!(member.kind(), "comment" | "ERROR") {
                continue;
            }
            let member_name = if is_ident(member) {
                text(member, self.src)
            } else {
                name_of(member, self.src).unwrap_or_else(|| text(member, self.src))
            };
            if member_name.is_empty() || !is_identifier(&member_name) {
                continue;
            }
            self.symbols.push(Symbol {
                qualified_name: format!("{}.{owner}.{member_name}", self.qual_prefix),
                name: member_name,
                kind: "enum_member".to_string(),
                file_path: self.rel.to_string(),
                start_line: member.start_position().row + 1,
                end_line: member.end_position().row + 1,
                declaration: false,
            });
        }
    }

    fn visit_def(&mut self, node: Node, parent_name: &str) {
        let wrapper_start = node.start_position().row;
        let node = unwrap(node, self.cfg);
        let js = self.js_family();
        let mut kind = self.cfg.def_kind(node.kind());
        let recovered = if self.language == "kotlin" {
            crate::lang::kotlin::recovered_class(node, self.src)
        } else {
            None
        };
        if recovered.is_some() {
            kind = Some("class");
        }
        if js {
            kind = kind.or(match node.kind() {
                "class" => Some("class"),
                "function_expression" | "function" | "arrow_function" => Some("function"),
                _ => None,
            });
        }
        let mut lua_target = None;
        let mut lua_value = None;
        if self.language == "lua" && matches!(node.kind(), "assignment_statement" | "field") {
            let bound = crate::lang::lua::binding(node);
            lua_target = bound.map(|(t, _)| t);
            lua_value = bound.map(|(_, v)| v);
            kind = lua_value
                .filter(|v| v.kind() == "function_definition")
                .map(|_| "function");
        }
        let kind = match kind {
            Some(k) if k != "@wrapped" => k,
            _ => return,
        };
        let mut name = match lua_target {
            Some(t) => Some(text(t, self.src)),
            None => name_of(node, self.src),
        };
        if js
            && matches!(
                node.kind(),
                "class" | "function_expression" | "function" | "arrow_function"
            )
        {
            let (row, col) = (node.start_position().row, node.start_position().column);
            name = Some(match field(node, "name") {
                Some(n) => text(n, self.src),
                None => format!("(anonymous@{}:{})", row + 1, col + 1),
            });
        }
        if self.language == "cpp" && node.kind() == "function_definition" {
            name = crate::lang::cpp::recovered_name(node, self.src).or(name);
        }
        if self.language == "kotlin" {
            if node.kind() == "companion_object" {
                name = name.or_else(|| Some("Companion".to_string()));
            } else if node.kind() == "property_declaration" {
                name = named_children(node)
                    .into_iter()
                    .find(|c| c.kind() == "variable_declaration")
                    .and_then(|v| name_of(v, self.src));
            }
            if recovered.is_some() {
                name = recovered.clone();
            }
        }
        let Some(mut name) = name.filter(|n| !n.is_empty()) else {
            return;
        };
        if self.language == "lua" {
            // Python replaces `:` then `.`; one pass is the same.
            name = name.replace([':', '.'], "::");
        }
        let mut name = collapse_scope_whitespace(&name);
        if self.language == "cpp" {
            // An unknown macro before a templated return type leaves the type
            // in the declarator: keep what follows its last top-level space.
            name = crate::lang::cpp::without_return_type(&name);
        }
        let mut scope = String::new();
        if name.contains("::") {
            let mut parts = top_level_scopes(&name);
            name = parts.pop().unwrap_or_default();
            scope = parts.join("::");
        }
        // Python: `(parent_name or ".".join(scopes)) if scope else parent_name`.
        let owner = if !scope.is_empty() && parent_name.is_empty() {
            top_level_scopes(&scope).join(".")
        } else {
            parent_name.to_string()
        };
        let mut owner = owner;
        if self.language == "kotlin" && node.kind() == "function_declaration" {
            // An extension receiver precedes the dot before the name; the
            // return type is another user_type and must not be taken for it.
            let kids = children(node);
            if let Some(index) = kids.iter().position(|c| c.kind() == ".").filter(|&i| i > 0) {
                let receiver = text(kids[index - 1], self.src);
                owner = if parent_name.is_empty() {
                    receiver
                } else {
                    format!("{parent_name}.{receiver}")
                };
            }
        }
        if self.language == "go" && node.kind() == "method_declaration" {
            // The receiver's type, pointers and type arguments dropped.
            let parameter = field(node, "receiver").and_then(|r| r.named_child(0));
            let mut target = parameter.and_then(|p| field(p, "type"));
            while let Some(t) = target.filter(|t| t.kind() == "pointer_type") {
                target = t.named_child(0);
            }
            if let Some(t) = target {
                owner = text(t, self.src)
                    .split('[')
                    .next()
                    .unwrap_or("")
                    .to_string();
            }
        }
        if self.language == "cpp" && !scope.is_empty() {
            let explicit = top_level_scopes(&scope).join(".");
            let explicit = explicit.trim_start_matches('.').to_string();
            owner = if parent_name.is_empty()
                || scope.starts_with("::")
                || explicit == parent_name
                || explicit.starts_with(&format!("{parent_name}."))
            {
                explicit
            } else {
                format!("{parent_name}.{explicit}")
            };
        }
        let full = if owner.is_empty() {
            name.clone()
        } else {
            format!("{owner}.{name}")
        };
        if self.language == "cpp" {
            let namespace = self
                .cpp_namespace_scopes
                .get(&owner)
                .cloned()
                .unwrap_or_else(|| owner.clone());
            self.cpp_namespace_scopes.insert(full.clone(), namespace);
        }
        let start = node.start_position().row;
        let mut end = node.end_position().row;
        if recovered.is_some() && node.end_position().column == 0 && end > start {
            end -= 1; // an ERROR can swallow the newline after its last token
        }
        let qn = format!("{}.{full}", self.qual_prefix);
        self.symbols.push(Symbol {
            qualified_name: qn.clone(),
            name: name.clone(),
            kind: kind.to_string(),
            file_path: self.rel.to_string(),
            start_line: start + 1,
            end_line: end + 1,
            // An interface method has no body (ADR 19).
            declaration: (self.language == "go" && node.kind() == "method_elem")
                || (self.language == "java"
                    && node.kind() == "method_declaration"
                    && field(node, "body").is_none()),
        });
        self.caller_spans
            .push((node.start_byte(), node.end_byte(), qn));
        let mut chunk_start = self.leading_start(wrapper_start.min(start));
        if self.language == "cpp" {
            let mut wrapper = node.parent();
            while let Some(w) = wrapper.filter(|w| w.kind() == "template_declaration") {
                chunk_start = w.start_position().row;
                wrapper = w.parent();
            }
        }
        if kind == "enum" {
            self.emit_enum_members(node, &full);
        }
        // A recovered annotation class has no body; its children are its
        // own stacked annotations, not nested classes.
        if self.cfg.class_nodes.contains(&node.kind())
            || (recovered.is_some() && node.kind() == "ERROR")
            || (js && node.kind() == "class")
        {
            let methods = self.declarations(body_children(node), &full);
            let first_method = methods
                .iter()
                .map(|(m, _)| self.leading_start(m.start_position().row))
                .min()
                .unwrap_or(end + 1);
            self.add_chunk(
                chunk_start,
                chunk_start.max(first_method.saturating_sub(1)),
                "class",
                &[name],
            );
            for (m, owner) in methods {
                self.visit_def(m, &owner);
            }
        } else {
            if end - start > MAX_CHUNK_LINES {
                end = start + MAX_CHUNK_LINES;
            }
            self.add_chunk(chunk_start, end, kind, std::slice::from_ref(&full));
            // Rust items may be defined inside a function, shadowing a
            // same-named outer fn; they are legal in any block.
            // Local C++ types live in function blocks; require a real
            // function declarator (an export macro can make a class look
            // like a broken function).
            let mut cpp_function = false;
            if self.language == "cpp" && node.kind() == "function_definition" {
                let mut declarator = field(node, "declarator");
                while let Some(d) = declarator.filter(|d| !d.has_error()) {
                    if d.kind() == "function_declarator" {
                        cpp_function = true;
                        break;
                    }
                    declarator = field(d, "declarator");
                }
            }
            if cpp_function {
                for (nested, owner) in self.declarations(body_children(node), &full) {
                    self.visit_def(nested, &owner);
                }
            } else if self.language == "rust" && node.kind() == "function_item" {
                let items = crate::lang::rust::block_items(field(node, "body"));
                for (nested, owner) in self.declarations(items, &full) {
                    self.visit_def(nested, &owner);
                }
            } else if matches!(self.language, "python" | "java" | "lua") || js {
                let body_node = if js && node.kind() == "variable_declarator" {
                    field(node, "value")
                } else if lua_value.is_some() {
                    lua_value
                } else {
                    Some(node)
                };
                if let Some(body_node) = body_node {
                    let children = match field(body_node, "body") {
                        Some(body) if js => vec![body],
                        _ => body_children(body_node),
                    };
                    for (nested, owner) in self.declarations(children, &full) {
                        self.visit_def(nested, &owner);
                    }
                }
            }
        }
    }

    fn collect_refs_and_imports(&mut self, root: Node) {
        let mut stack = vec![root];
        while let Some(n) = stack.pop() {
            if self.cfg.calls.contains(&n.kind()) {
                if self.language == "lua" {
                    // A require() is module evidence, not a call. The
                    // enclosing declaration keeps its local alias.
                    let mut target = n;
                    let mut ancestor = n.parent();
                    for _ in 0..3 {
                        let Some(a) = ancestor else { break };
                        if a.kind() == "variable_declaration" {
                            target = a;
                            break;
                        }
                        ancestor = a.parent();
                    }
                    let before = self.imports.len();
                    self.extract_bindings(target);
                    if self.imports.len() > before {
                        continue;
                    }
                }
                // Python's `if name:`: an empty name is no call.
                if let Some(name) =
                    call_name(n, self.src, self.cfg.qualified).filter(|n| !n.is_empty())
                {
                    let name = self.instance_call(name, n);
                    let base = name.rsplit('.').next().unwrap_or(&name).to_string();
                    if base.chars().count() > 2 {
                        *self.refs.entry(base).or_insert(0) += 1;
                    }
                    self.call_sites.push((name, n.start_byte()));
                }
            } else if n.kind().contains("import")
                || n.kind() == "use_declaration"
                || self.cfg.imports.contains(&n.kind())
            {
                self.extract_bindings(n);
                continue;
            }
            stack.extend(children(n));
        }
    }

    pub(crate) fn instance_call(&mut self, name: String, call: Node) -> String {
        if crate::typed::typed_scopes(self.language).is_some() {
            // The written chain (`mx.tree.find`) rather than the two-segment
            // name; without evidence the name stays as it was.
            let chain = crate::typed::member_chain(call, self.language, self.src)
                .unwrap_or_else(|| name.clone());
            if self.language == "go" {
                let receiver = crate::lang::go::receiver_call(self, &chain, call);
                if receiver != chain {
                    return receiver;
                }
            }
            let typed = crate::typed::typed_receiver_call(self, &chain, call);
            return if typed != chain { typed } else { name };
        }
        match self.language {
            "cpp" => crate::lang::cpp::member_call(self, name, call),
            "python" => crate::lang::python::instance_call(self, name, call),
            "javascript" | "typescript" | "tsx" if name.contains('.') => {
                crate::lang::javascript::instance_call(self, name, call)
            }
            _ => name,
        }
    }

    fn extract_bindings(&mut self, node: Node) {
        match self.language {
            "python" => crate::lang::python::extract_bindings(self, node),
            // Cortex tests `language in ("javascript", "typescript")`; TSX
            // files arrive as "typescript" with only the grammar switched.
            "javascript" | "typescript" => crate::lang::javascript::extract_bindings(self, node),
            "go" => crate::lang::go::extract_bindings(self, node),
            // Cortex: `language in ("java", "kotlin", "groovy")`.
            "java" | "kotlin" | "groovy" => crate::lang::java::extract_bindings(self, node),
            "php" => crate::lang::php::extract_bindings(self, node),
            "rust" => crate::lang::rust::extract_bindings(self, node),
            "scala" => crate::lang::scala::extract_bindings(self, node),
            "lua" => crate::lang::lua::extract_bindings(self, node),
            // Generic: module strings only (the resolver suffix-matches).
            _ => {
                for c in named_children(node) {
                    if matches!(
                        c.kind(),
                        "dotted_name"
                            | "string"
                            | "identifier"
                            | "import_prefix"
                            | "interpreted_string_literal"
                            | "scoped_identifier"
                            | "import_spec"
                    ) {
                        let t = text(c, self.src);
                        self.imports
                            .push(t.trim_matches(|ch| ch == '\'' || ch == '"').to_string());
                    }
                }
            }
        }
    }

    fn member_types(&self, root: Node) -> Vec<(String, String, String)> {
        match self.language {
            "python" => crate::lang::python::member_types(root, self.src),
            "go" => crate::lang::go::member_types(root, self.src),
            "java" => crate::lang::java::member_types(root, self.src),
            "rust" => crate::lang::rust::member_types(root, self.src),
            "cpp" => crate::lang::cpp::member_types(root, self.src),
            _ => Vec::new(),
        }
    }
}

/// Cortex `_opens_extern_c`: an ERROR whose own tokens open an
/// `extern "..." {` block.
fn opens_extern_c(node: Node) -> bool {
    let kinds: Vec<&str> = children(node).iter().map(|c| c.kind()).collect();
    kinds
        .windows(3)
        .any(|w| w == ["extern", "string_literal", "{"])
}

/// Cortex `parse_file` for a language whose extractor is wired in.
pub fn parse_file(rel: &str, language: &str, src: &[u8], tree: &Tree, cfg: &Config) -> Parsed {
    parse_file_with_grammar_source(rel, language, src, src, tree, cfg)
}

/// `parse_file` where the grammar read a byte-preserving view of the source
/// (C++ annotation masking): chunks keep the original text; node text comes
/// from the view the tree was parsed from.
pub fn parse_file_with_grammar_source(
    rel: &str,
    language: &str,
    original: &[u8],
    src: &[u8],
    tree: &Tree,
    cfg: &Config,
) -> Parsed {
    parse_file_with_extra_calls(rel, language, original, src, tree, cfg, &[])
}

/// `parse_file` with Cortex's `_extra_calls`: call sites (name, byte offset
/// in the original source) found by another grammar, owned like any other
/// (Objective-C++ message sends the C++ pass cannot see).
pub fn parse_file_with_extra_calls(
    rel: &str,
    language: &str,
    original: &[u8],
    src: &[u8],
    tree: &Tree,
    cfg: &Config,
    extra_calls: &[(String, usize)],
) -> Parsed {
    let lines: Vec<String> = splitlines(&decode_replace(original))
        .into_iter()
        .map(String::from)
        .collect();
    let mut x = Extractor {
        rel,
        language,
        src,
        cfg,
        lines,
        chunk_language: language,
        qual_prefix: rel.replace('/', "."),
        chunks: Vec::new(),
        symbols: Vec::new(),
        refs: IndexMap::new(),
        imports: Vec::new(),
        bindings: IndexMap::new(),
        call_sites: Vec::new(),
        caller_spans: Vec::new(),
        constructor_cache: HashMap::new(),
        go_receivers: HashMap::new(),
        declared_types: HashMap::new(),
        binding_cache: HashMap::new(),
        cpp_locals: HashMap::new(),
        cpp_fields: None,
        cpp_namespace_scopes: HashMap::from([(String::new(), String::new())]),
    };
    let root = tree.root_node();
    let top_defs = x.declarations(children(root), "");
    let first_def_line = top_defs
        .iter()
        .map(|(d, _)| x.leading_start(d.start_position().row))
        .min()
        .unwrap_or(x.lines.len());
    if first_def_line > 0 {
        x.add_chunk(0, (first_def_line - 1).min(MAX_CHUNK_LINES), "module", &[]);
    }
    for (d, owner) in top_defs {
        x.visit_def(d, &owner);
    }
    x.collect_refs_and_imports(root);
    for (name, offset) in extra_calls {
        x.call_sites.push((name.clone(), *offset));
        let base = name.rsplit('.').next().unwrap_or(name);
        if base.chars().count() > 2 {
            *x.refs.entry(base.to_string()).or_insert(0) += 1;
        }
    }

    // Byte spans distinguish adjacent definitions; the narrowest wins. The
    // sort is stable, as Python's is, so ties keep definition order.
    let mut spans = x.caller_spans.clone();
    spans.sort_by_key(|(s, e, _)| e - s);
    let mut calls: IndexMap<(String, String), u64> = IndexMap::new();
    for (name, offset) in &x.call_sites {
        let caller = spans
            .iter()
            .find(|(s, e, _)| *s <= *offset && *offset < *e)
            .map(|(_, _, qn)| qn.clone())
            .unwrap_or_else(|| format!("file:{rel}"));
        *calls.entry((caller, name.clone())).or_insert(0) += 1;
    }
    // A reference to one of the file's own definitions is not a dependency.
    for s in &x.symbols {
        x.refs.shift_remove(&s.name);
    }
    let members = x.member_types(root);
    Parsed {
        chunks: x.chunks,
        symbols: x.symbols,
        imports: x.imports,
        refs: x.refs,
        bindings: x.bindings,
        calls,
        members,
    }
}
