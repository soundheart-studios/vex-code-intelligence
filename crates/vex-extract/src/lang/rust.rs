//! Rust: Cortex's `_CONFIG["rust"]` (qualified), the Rust arm of
//! `_extract_bindings`, `_rust_block_items`, Rust's `_result_desc` and the
//! Rust rows of `_member_types`. Typed member calls are typed.rs's.

use crate::ast::{Config, Extractor, field, named_children, text};
use crate::typed::{fields, plain_type, type_desc};
use tree_sitter::Node;

pub const CONFIG: Config = Config {
    defs: &[
        ("function_item", "function"),
        ("struct_item", "struct"),
        ("trait_item", "trait"),
        ("enum_item", "enum"),
        ("impl_item", "impl"),
        ("mod_item", "module"),
        ("function_signature_item", "function"),
    ],
    calls: &["call_expression", "macro_invocation"],
    class_nodes: &["impl_item", "trait_item", "mod_item"],
    imports: &[],
    qualified: true,
};

/// Python `s.rpartition("::")`.
fn rpartition_scope(s: &str) -> (&str, &str) {
    s.rsplit_once("::").unwrap_or(("", s))
}

/// `use crate::auth::check`, `use x::y as z`, `use a::{self, B, c::D as E}`,
/// `use a::*`: module = the path minus its item, bound to the item (or alias).
pub fn extract_bindings(x: &mut Extractor, node: Node) {
    let src = x.src;
    fn add_use(x: &mut Extractor, scoped: Node, alias: Option<String>) {
        let full = text(scoped, x.src);
        let (module, item) = match full.rsplit_once("::") {
            Some((m, i)) => (m.to_string(), i.to_string()),
            None => (String::new(), full.clone()),
        };
        x.imports.push(if module.is_empty() {
            full.clone()
        } else {
            module.clone()
        });
        x.bindings
            .insert(alias.unwrap_or_else(|| item.clone()), (module, item));
    }
    fn add_list(x: &mut Extractor, prefix: &str, listed: Node) {
        let src = x.src;
        let join = |item: &str| {
            if prefix.is_empty() {
                item.to_string()
            } else {
                format!("{prefix}::{item}")
            }
        };
        for item in named_children(listed) {
            match item.kind() {
                "self" => {
                    let (head, last) = rpartition_scope(prefix);
                    if !last.is_empty() {
                        x.imports
                            .push(if head.is_empty() { prefix } else { head }.to_string());
                        x.bindings
                            .insert(last.to_string(), (head.to_string(), last.to_string()));
                    }
                }
                "identifier" | "scoped_identifier" => {
                    let full = join(&text(item, src));
                    let (module, name) = rpartition_scope(&full);
                    x.imports
                        .push(if module.is_empty() { &full } else { module }.to_string());
                    x.bindings
                        .insert(name.to_string(), (module.to_string(), name.to_string()));
                }
                "use_as_clause" => {
                    if let Some(path) = field(item, "path") {
                        let full = join(&text(path, src));
                        let (module, name) = rpartition_scope(&full);
                        x.imports
                            .push(if module.is_empty() { &full } else { module }.to_string());
                        let local = field(item, "alias")
                            .map(|a| text(a, src))
                            .unwrap_or_else(|| name.to_string());
                        x.bindings
                            .insert(local, (module.to_string(), name.to_string()));
                    }
                }
                "scoped_use_list" => {
                    if let Some(nested) = field(item, "list") {
                        let inner = field(item, "path")
                            .map(|p| text(p, src))
                            .unwrap_or_default();
                        let joined = [prefix, inner.as_str()]
                            .iter()
                            .filter(|s| !s.is_empty())
                            .copied()
                            .collect::<Vec<_>>()
                            .join("::");
                        add_list(x, &joined, nested);
                    }
                }
                "use_wildcard" => x.imports.push(prefix.to_string()),
                _ => {}
            }
        }
    }
    let mut stack = named_children(node);
    while let Some(c) = stack.pop() {
        match c.kind() {
            "scoped_identifier" => add_use(x, c, None),
            "use_as_clause" => {
                if let Some(path) = c.named_child(0) {
                    let alias = field(c, "alias").map(|a| text(a, src));
                    add_use(x, path, alias);
                }
            }
            "scoped_use_list" => {
                if let Some(listed) = field(c, "list") {
                    let prefix = field(c, "path").map(|p| text(p, src)).unwrap_or_default();
                    add_list(x, &prefix, listed);
                }
            }
            "use_list" | "use_wildcard" => stack.extend(named_children(c)),
            "identifier" => x.imports.push(text(c, src)),
            _ => {}
        }
    }
}

/// Cortex `_rust_block_items`: items anywhere in a function body, not
/// inside a nested item or closure (which own their own), in source order.
pub fn block_items(body: Option<Node>) -> Vec<Node> {
    let mut items = Vec::new();
    let mut stack = body.map(named_children).unwrap_or_default();
    while let Some(node) = stack.pop() {
        if node.kind().ends_with("_item") {
            items.push(node);
        } else if node.kind() != "closure_expression" {
            stack.extend(named_children(node));
        }
    }
    items.sort_by_key(|n| n.start_byte());
    items
}

/// Cortex `_result_desc` for Rust: declared results, `?`-prefixed when
/// wrapped in Result or Option.
pub fn result_desc(node: Option<Node>, src: &[u8]) -> Option<String> {
    let mut node = node?;
    let mut wrapped = "";
    if node.kind() == "generic_type"
        && let (Some(head), Some(arguments)) = (field(node, "type"), field(node, "type_arguments"))
        && matches!(&src[head.byte_range()], b"Result" | b"Option")
        && let Some(first) = arguments.named_child(0)
    {
        wrapped = "?";
        node = first;
    }
    let parts: Vec<String> = if node.kind() == "tuple_type" {
        named_children(node)
            .into_iter()
            .map(|c| plain_type(Some(c), src).unwrap_or_default())
            .collect()
    } else {
        vec![plain_type(Some(node), src).unwrap_or_default()]
    };
    parts
        .iter()
        .any(|p| !p.is_empty())
        .then(|| format!("{wrapped}{}", parts.join(",")))
}

/// Enclosing structs and modules, outermost first; None under an unnamed one.
fn owner_path(node: Node, src: &[u8]) -> Option<String> {
    let mut parts = Vec::new();
    let mut scope = node.parent();
    while let Some(s) = scope {
        if matches!(s.kind(), "struct_item" | "mod_item") {
            parts.push(text(field(s, "name")?, src).replace("::", "."));
        }
        scope = s.parent();
    }
    parts.reverse();
    let joined = parts.join(".");
    (!joined.is_empty()).then_some(joined)
}

/// The Rust rows of Cortex `_member_types`: struct fields with a plain
/// type, and function results under `name()` owned by impl and mod.
pub fn member_types(root: Node, src: &[u8]) -> Vec<(String, String, String)> {
    let mut rows = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        stack.extend(named_children(node));
        match node.kind() {
            "field_declaration" => {
                let owner = owner_path(node, src);
                let kind = type_desc(field(node, "type"), "rust", src);
                for name in fields(node, "name") {
                    if let (Some(o), Some(k)) = (&owner, &kind) {
                        rows.push((o.clone(), text(name, src), k.clone()));
                    }
                }
            }
            "function_item" => {
                let results = result_desc(field(node, "return_type"), src);
                let mut owner_parts = Vec::new();
                let mut scope = node.parent();
                while let Some(s) = scope {
                    match s.kind() {
                        "impl_item" => {
                            if let Some(target) = plain_type(field(s, "type"), src) {
                                owner_parts.push(target);
                            }
                        }
                        "mod_item" => {
                            if let Some(n) = field(s, "name") {
                                owner_parts.push(text(n, src));
                            }
                        }
                        // A nested fn's results are not recorded here.
                        "function_item" => break,
                        _ => {}
                    }
                    scope = s.parent();
                }
                if let (Some(name), Some(r)) = (field(node, "name"), results) {
                    owner_parts.reverse();
                    rows.push((owner_parts.join("."), format!("{}()", text(name, src)), r));
                }
            }
            _ => {}
        }
    }
    rows
}
