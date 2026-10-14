//! Java: Cortex's `_CONFIG["java"]`, the Java arm of `_extract_bindings`
//! and the Java rows of `_member_types`. Typed member calls are typed.rs's.

use crate::ast::{Config, Extractor, field, named_children, text};
use crate::typed::{fields, plain_type};
use tree_sitter::Node;

pub const CONFIG: Config = Config {
    defs: &[
        ("method_declaration", "method"),
        ("class_declaration", "class"),
        ("constructor_declaration", "method"),
        ("interface_declaration", "interface"),
        ("enum_declaration", "enum"),
    ],
    calls: &["method_invocation", "object_creation_expression"],
    class_nodes: &[
        "class_declaration",
        "interface_declaration",
        "enum_declaration",
    ],
    imports: &[],
    qualified: true,
};

/// `import com.foo.Auth;`: the full dotted path, bound to its last segment.
pub fn extract_bindings(x: &mut Extractor, node: Node) {
    let src = x.src;
    for c in named_children(node) {
        if matches!(
            c.kind(),
            "scoped_identifier" | "qualified_identifier" | "dotted_name"
        ) {
            let full = text(c, src);
            let last = full.rsplit('.').next().unwrap_or(&full).to_string();
            x.imports.push(full.clone());
            x.bindings.insert(last.clone(), (full, last));
        }
    }
}

/// Enclosing classes, enums and records, outermost first; None under an
/// anonymous one.
fn owner_path(node: Node, src: &[u8]) -> Option<String> {
    let mut parts = Vec::new();
    let mut scope = node.parent();
    while let Some(s) = scope {
        if matches!(
            s.kind(),
            "class_declaration" | "enum_declaration" | "record_declaration"
        ) {
            parts.push(text(field(s, "name")?, src).replace("::", "."));
        }
        scope = s.parent();
    }
    parts.reverse();
    let joined = parts.join(".");
    (!joined.is_empty()).then_some(joined)
}

/// The Java rows of Cortex `_member_types`: fields with a plain type.
pub fn member_types(root: Node, src: &[u8]) -> Vec<(String, String, String)> {
    let mut rows = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        stack.extend(named_children(node));
        if node.kind() != "field_declaration" {
            continue;
        }
        let owner = owner_path(node, src);
        let kind = plain_type(field(node, "type"), src);
        for declarator in fields(node, "declarator") {
            if let (Some(o), Some(k), Some(name)) = (&owner, &kind, field(declarator, "name")) {
                rows.push((o.clone(), text(name, src), k.clone()));
            }
        }
    }
    rows
}
