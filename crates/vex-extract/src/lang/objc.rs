//! Objective-C and Objective-C++: Cortex's `languages/objc.py`.
//!
//! Containers (`@interface`, `@implementation`, `@protocol`), methods and
//! C functions are definitions; message sends and C calls inside their
//! bodies are calls. A `.mm` file is also read by the C++ extractor over a
//! byte-preserving view with every Objective-C region blanked, its message
//! sends handed over as extra call sites.

use crate::ast::{call_name, children, field, name_of, named_children, text};
use crate::text::{decode_replace, path_suffix, py_strip, splitlines};
use indexmap::IndexMap;
use std::collections::{BTreeMap, HashSet};
use tree_sitter::Node;
use vex_core::ids::chunk_id;
use vex_core::{Chunk, Parsed, Symbol};

const BLANK_MACROS: [&[u8]; 2] = [b"NS_ASSUME_NONNULL_BEGIN", b"NS_ASSUME_NONNULL_END"];
const MAX_CHUNK_LINES: usize = 120;

fn container_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "class_interface" | "class_implementation" => Some("class"),
        "protocol_declaration" => Some("protocol"),
        _ => None,
    }
}

fn is_method(kind: &str) -> bool {
    matches!(kind, "method_declaration" | "method_definition")
}

/// Spaces for every byte but `\n` and `\r`.
fn blanked(bytes: &[u8]) -> Vec<u8> {
    bytes
        .iter()
        .map(|&b| if b == b'\n' || b == b'\r' { b } else { b' ' })
        .collect()
}

/// Python `\s` in a bytes pattern.
fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// The end of a raw string `R"delim( ... )delim"` whose `R` is at `r`, or
/// None (Cortex `_RAW_STRING` from the `R` on: a delimiter of at most 16
/// bytes, none of `()\` or whitespace, then the shortest body).
fn raw_string_end(src: &[u8], r: usize) -> Option<usize> {
    if src.get(r) != Some(&b'R') || src.get(r + 1) != Some(&b'"') {
        return None;
    }
    let delim_start = r + 2;
    let mut open = delim_start;
    while open < src.len() && open - delim_start <= 16 {
        let b = src[open];
        if b == b'(' {
            break;
        }
        if b == b')' || b == b'\\' || is_space(b) {
            return None;
        }
        open += 1;
    }
    if src.get(open) != Some(&b'(') || open - delim_start > 16 {
        return None;
    }
    let delim = &src[delim_start..open];
    let mut close = Vec::with_capacity(delim.len() + 2);
    close.push(b')');
    close.extend_from_slice(delim);
    close.push(b'"');
    let body = open + 1;
    src[body..]
        .windows(close.len())
        .position(|w| w == close.as_slice())
        .map(|i| body + i + close.len())
}

/// Cortex `_blank_raw_strings`: each C++ raw string (with an optional
/// `u8`/`u`/`U`/`L` prefix) becomes `""` and spaces of the same length.
pub fn blank_raw_strings(src: &[u8]) -> Vec<u8> {
    let mut out = src.to_vec();
    let mut i = 0;
    while i < src.len() {
        // The alternatives in the pattern's order: `u8`, then one prefix
        // byte, then none.
        let mut candidates = Vec::new();
        if src[i..].starts_with(b"u8") {
            candidates.push(i + 2);
        }
        if matches!(src[i], b'u' | b'U' | b'L') {
            candidates.push(i + 1);
        }
        candidates.push(i);
        match candidates.into_iter().find_map(|r| raw_string_end(src, r)) {
            Some(end) => {
                let mut kept = blanked(&src[i..end]);
                kept[0] = b'"';
                kept[1] = b'"';
                out[i..end].copy_from_slice(&kept);
                i = end;
            }
            None => i += 1,
        }
    }
    out
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Cortex `_blank_range_for_headers`: `for (T x : xs)` headers become
/// `for (;;   )` of equal length; three-part and `for ... in` loops stay.
pub fn blank_range_for_headers(src: &[u8]) -> Vec<u8> {
    let mut out = src.to_vec();
    let mut i = 0;
    while i + 3 <= src.len() {
        // `\bfor\s*\(`, matches found on the unmodified source.
        let boundary = i == 0 || !is_word(src[i - 1]);
        if !(boundary && &src[i..i + 3] == b"for" && src.get(i + 3).is_none_or(|&b| !is_word(b))) {
            i += 1;
            continue;
        }
        let mut open = i + 3;
        while open < src.len() && is_space(src[open]) {
            open += 1;
        }
        if src.get(open) != Some(&b'(') {
            i += 1;
            continue;
        }
        let mut depth = 0i64;
        let mut close = None;
        for (pos, &b) in src
            .iter()
            .enumerate()
            .take(src.len().min(open + 2000))
            .skip(open)
        {
            if b == b'(' {
                depth += 1;
            } else if b == b')' {
                depth -= 1;
                if depth == 0 {
                    close = Some(pos);
                    break;
                }
            }
        }
        i = open + 1;
        let Some(close) = close else { continue };
        let inner = &src[open + 1..close];
        let lone_colon = (0..inner.len()).any(|k| {
            inner[k] == b':' && (k == 0 || inner[k - 1] != b':') && inner.get(k + 1) != Some(&b':')
        });
        if inner.contains(&b';') || !lone_colon {
            continue;
        }
        let mut blank = blanked(inner);
        if blank.len() >= 2 {
            blank[0] = b';';
            blank[1] = b';';
        }
        out[open + 1..close].copy_from_slice(&blank);
    }
    out
}

/// Replace every occurrence of `needle` (left to right, non-overlapping).
fn blank_all(src: &[u8], needle: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        if src[i..].starts_with(needle) {
            out.extend(std::iter::repeat_n(b' ', needle.len()));
            i += needle.len();
        } else {
            out.push(src[i]);
            i += 1;
        }
    }
    out
}

struct Objc<'a> {
    rel: &'a str,
    src: &'a [u8],
    qual_prefix: String,
    lines: Vec<String>,
    mixed: bool,
    chunks: Vec<Chunk>,
    symbols: Vec<Symbol>,
    imports: Vec<String>,
    refs: IndexMap<String, u64>,
    seen: HashSet<(String, usize)>,
    masked_regions: Vec<(usize, usize)>,
}

impl<'a> Objc<'a> {
    fn add_chunk(&mut self, start: usize, end: usize, kind: &str, name: &str) {
        if self.lines.is_empty() {
            return;
        }
        let end = end.min(start + MAX_CHUNK_LINES).min(self.lines.len() - 1);
        let content = if start <= end {
            self.lines[start..=end].join("\n")
        } else {
            String::new()
        };
        if py_strip(&content).is_empty() {
            return;
        }
        let mut meta = BTreeMap::new();
        meta.insert("symbols".to_string(), name.into());
        self.chunks.push(Chunk::with_metadata(
            chunk_id(self.rel, start, &content),
            self.rel,
            start + 1,
            end + 1,
            content,
            kind,
            "objc",
            meta,
        ));
    }

    /// Declaration (`@interface`) and definition (`@implementation`) are
    /// separate sites (ADR 19); a site is (name, start row).
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
            declaration: node.kind() == "method_declaration",
        });
    }

    fn container_name(&self, node: Node) -> String {
        if let Some(n) = field(node, "name") {
            return text(n, self.src);
        }
        children(node)
            .into_iter()
            .find(|c| c.kind() == "identifier")
            .map(|c| text(c, self.src))
            .unwrap_or_default()
    }

    fn method_name(&self, node: Node) -> String {
        let kids = children(node);
        let named: Vec<String> = kids
            .iter()
            .enumerate()
            .filter(|(i, c)| {
                c.kind() == "identifier"
                    && matches!(
                        node.field_name_for_child(*i as u32),
                        Some("method") | Some("name")
                    )
            })
            .map(|(_, c)| text(*c, self.src))
            .collect();
        if !named.is_empty() {
            return named.concat();
        }
        kids.iter()
            .filter(|c| c.kind() == "identifier")
            .map(|c| text(*c, self.src))
            .collect::<Vec<_>>()
            .concat()
    }

    fn import_from(&mut self, node: Node) {
        if node.kind() == "module_import" {
            if let Some(target) = field(node, "path") {
                let t = text(target, self.src);
                let module = py_strip(t.split('.').next().unwrap_or(""));
                if !module.is_empty() {
                    self.imports.push(module.to_string());
                }
            }
            return;
        }
        for c in children(node) {
            let raw = match c.kind() {
                "system_lib_string" => text(c, self.src)
                    .trim_matches(|ch| ch == '<' || ch == '>')
                    .to_string(),
                "string_literal" => text(c, self.src)
                    .trim_matches(|ch| ch == '\'' || ch == '"')
                    .to_string(),
                _ => continue,
            };
            if !raw.is_empty() {
                self.imports.push(raw);
            }
        }
    }

    /// Cortex `walk`, in document order without recursion. Returns the
    /// bodies whose calls belong to a definition: (node, caller).
    fn walk<'t>(&mut self, root: Node<'t>) -> Vec<(Node<'t>, String)> {
        let mut bodies = Vec::new();
        let mut stack: Vec<(Node<'t>, String, String)> = vec![(root, String::new(), String::new())];
        while let Some((node, owner, owner_qn)) = stack.pop() {
            let kind = node.kind();
            if matches!(kind, "preproc_include" | "module_import") {
                self.import_from(node);
                if kind == "module_import" {
                    self.masked_regions
                        .push((node.start_byte(), node.end_byte()));
                }
                continue;
            }
            if let Some(ckind) = container_kind(kind) {
                self.masked_regions
                    .push((node.start_byte(), node.end_byte()));
                let name = self.container_name(node);
                let kids = children(node);
                if name.is_empty() {
                    for c in kids.into_iter().rev() {
                        stack.push((c, owner.clone(), owner_qn.clone()));
                    }
                    continue;
                }
                let qn = format!("{}.{name}", self.qual_prefix);
                self.add_symbol(&qn, &name, ckind, node);
                let start = node.start_position().row;
                let header_end = kids
                    .iter()
                    .filter(|m| is_method(m.kind()))
                    .map(|m| m.start_position().row as i64 - 1)
                    .min()
                    .unwrap_or(node.end_position().row as i64);
                self.add_chunk(start, (start as i64).max(header_end) as usize, ckind, &name);
                let identifiers: Vec<String> = kids
                    .iter()
                    .filter(|c| c.kind() == "identifier")
                    .map(|c| text(*c, self.src))
                    .collect();
                if kind == "class_interface" && identifiers.len() > 1 {
                    *self.refs.entry(identifiers[1].clone()).or_insert(0) += 1;
                }
                for c in kids.into_iter().rev() {
                    stack.push((c, name.clone(), qn.clone()));
                }
                continue;
            }
            if is_method(kind) {
                let name = self.method_name(node);
                if name.is_empty() {
                    continue;
                }
                let parent = if owner_qn.is_empty() {
                    self.qual_prefix.clone()
                } else {
                    owner_qn.clone()
                };
                let qn = format!("{parent}.{name}");
                self.add_symbol(&qn, &name, "method", node);
                let label = if owner.is_empty() {
                    name.clone()
                } else {
                    format!("{owner}.{name}")
                };
                self.add_chunk(
                    node.start_position().row,
                    node.end_position().row,
                    "method",
                    &label,
                );
                if kind == "method_definition" {
                    bodies.push((node, qn));
                }
                continue;
            }
            if kind == "function_definition" && !self.mixed {
                let declarator = field(node, "declarator");
                if let (Some(name), Some(d)) = (name_of(node, self.src), declarator)
                    && !name.is_empty()
                    && !d.has_error()
                {
                    let qn = format!("{}.{name}", self.qual_prefix);
                    self.add_symbol(&qn, &name, "function", node);
                    self.add_chunk(
                        node.start_position().row,
                        node.end_position().row,
                        "function",
                        &name,
                    );
                    bodies.push((node, qn));
                }
                continue;
            }
            for c in children(node).into_iter().rev() {
                stack.push((c, owner.clone(), owner_qn.clone()));
            }
        }
        bodies
    }

    /// Cortex `call_name`; an empty name counts as none (`if name:`).
    fn call_name(&self, node: Node) -> Option<String> {
        let name = match node.kind() {
            "message_expression" => {
                let parts: String = children(node)
                    .iter()
                    .enumerate()
                    .filter(|(i, c)| {
                        c.kind() == "identifier"
                            && node.field_name_for_child(*i as u32) == Some("method")
                    })
                    .map(|(_, c)| text(*c, self.src))
                    .collect();
                (!parts.is_empty()).then_some(parts)
            }
            "call_expression" => call_name(node, self.src, false),
            _ => None,
        };
        name.filter(|n| !n.is_empty())
    }
}

/// Cortex `parse_objc`. None when the grammar is not compiled in.
pub fn parse_objc(rel: &str, original: &[u8]) -> Option<Parsed> {
    let mut source = original.to_vec();
    for macro_name in BLANK_MACROS {
        source = blank_all(&source, macro_name);
    }
    // The C++ pass reads this view: its grammar handles raw strings and
    // range-for itself, and their calls must stay visible to it.
    let cpp_view = source.clone();
    let mixed = path_suffix(rel).to_lowercase() == ".mm";
    if mixed {
        source = blank_range_for_headers(&blank_raw_strings(&source));
    }
    let tree = crate::engine::parse_tree("objc", &source)?;
    let lines: Vec<String> = splitlines(&decode_replace(original))
        .into_iter()
        .map(String::from)
        .collect();
    let mut x = Objc {
        rel,
        src: &source,
        qual_prefix: rel.replace('/', "."),
        lines,
        mixed,
        chunks: Vec::new(),
        symbols: Vec::new(),
        imports: Vec::new(),
        refs: IndexMap::new(),
        seen: HashSet::new(),
        masked_regions: Vec::new(),
    };
    let root = tree.root_node();
    let bodies = x.walk(root);

    let mut calls: IndexMap<(String, String), u64> = IndexMap::new();
    for (method, caller) in &bodies {
        let start = field(*method, "body").unwrap_or(*method);
        let mut stack = vec![start];
        while let Some(node) = stack.pop() {
            if let Some(name) = x.call_name(node) {
                *x.refs.entry(name.clone()).or_insert(0) += 1;
                *calls.entry((caller.clone(), name)).or_insert(0) += 1;
            }
            for c in children(node).into_iter().rev() {
                if !is_method(c.kind()) {
                    stack.push(c);
                }
            }
        }
    }

    if mixed {
        mixed_cpp_pass(&mut x, original, &cpp_view, root, &mut calls);
    }

    if x.chunks.is_empty() && !x.lines.is_empty() {
        let last = x.lines.len() - 1;
        x.add_chunk(0, last, "module", "");
    }
    for s in &x.symbols {
        x.refs.shift_remove(&s.name);
    }
    Some(Parsed {
        chunks: x.chunks,
        symbols: x.symbols,
        imports: x.imports,
        refs: x.refs,
        bindings: IndexMap::new(),
        calls,
        members: Vec::new(),
    })
}

/// Objective-C++: the C++ extractor over `cpp_view` with every region the
/// Objective-C grammar identified blanked (byte offsets and newlines kept),
/// each outermost message send replaced by `0`, and every send's name
/// handed over as an extra call site.
fn mixed_cpp_pass(
    x: &mut Objc,
    original: &[u8],
    cpp_view: &[u8],
    root: Node,
    calls: &mut IndexMap<(String, String), u64>,
) {
    let mut masked = cpp_view.to_vec();
    let blank = |masked: &mut Vec<u8>, start: usize, end: usize| {
        for b in &mut masked[start..end] {
            if *b != b'\n' && *b != b'\r' {
                *b = b' ';
            }
        }
    };
    for &(start, end) in &x.masked_regions {
        blank(&mut masked, start, end);
    }
    let mut extra: Vec<(String, usize)> = Vec::new();
    let mut stack = vec![(root, false)];
    while let Some((node, inside)) = stack.pop() {
        if container_kind(node.kind()).is_some() || node.kind() == "module_import" {
            continue;
        }
        let is_message = node.kind() == "message_expression";
        if (inside || is_message)
            && let Some(name) = x.call_name(node)
        {
            extra.push((name, node.start_byte()));
        }
        if is_message && !inside {
            blank(&mut masked, node.start_byte(), node.end_byte());
            masked[node.start_byte()] = b'0';
        }
        for c in named_children(node).into_iter().rev() {
            stack.push((c, inside || is_message));
        }
    }
    let grammar = crate::lang::cpp::mask_annotations(&masked);
    let Some(tree) = crate::engine::parse_tree("cpp", &grammar) else {
        return;
    };
    let cpp = crate::ast::parse_file_with_extra_calls(
        x.rel,
        "cpp",
        original,
        &grammar,
        &tree,
        &crate::lang::cpp::CONFIG,
        &extra,
    );
    x.symbols.extend(cpp.symbols);
    let mut known: HashSet<String> = x.chunks.iter().map(|c| c.id.clone()).collect();
    for mut chunk in cpp.chunks {
        chunk.language = "objc".to_string();
        if known.insert(chunk.id.clone()) {
            x.chunks.push(chunk);
        }
    }
    for i in cpp.imports {
        if !x.imports.contains(&i) {
            x.imports.push(i);
        }
    }
    for (k, v) in cpp.refs {
        *x.refs.entry(k).or_insert(0) += v;
    }
    for (k, v) in cpp.calls {
        *calls.entry(k).or_insert(0) += v;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Expected values are Cortex's own `_blank_raw_strings` and
    // `_blank_range_for_headers` on the same input.
    #[test]
    fn raw_strings_are_blanked_keeping_length_and_newlines() {
        let src = b"a = R\"x(\n{ \"k\": v }\n)x\"; b = u8R\"(q)\";";
        let out = blank_raw_strings(src);
        assert_eq!(out.len(), src.len());
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "a = \"\"  \n          \n   ; b = \"\"      ;"
        );
    }

    #[test]
    fn a_raw_string_without_its_close_is_left_alone() {
        let src = b"R\"x(never closed";
        assert_eq!(blank_raw_strings(src), src.to_vec());
        // A delimiter longer than 16 bytes is not a raw string.
        let long = b"R\"abcdefghijklmnopq(x)abcdefghijklmnopq\"";
        assert_eq!(blank_raw_strings(long), long.to_vec());
    }

    #[test]
    fn only_range_for_headers_are_rewritten() {
        let src = b"for (auto& x : xs) {}\nfor (int i = 0; i < n; i++) {}\nfor (id o in a) {}\nfor (A::B c : d) {}\nfor (std::x::y) {}";
        let out = String::from_utf8(blank_range_for_headers(src)).unwrap();
        assert_eq!(
            out,
            "for (;;          ) {}\nfor (int i = 0; i < n; i++) {}\nfor (id o in a) {}\nfor (;;        ) {}\nfor (std::x::y) {}"
        );
        // `before` is not the keyword `for`.
        assert_eq!(
            blank_range_for_headers(b"xfor (a : b)"),
            b"xfor (a : b)".to_vec()
        );
    }
}
