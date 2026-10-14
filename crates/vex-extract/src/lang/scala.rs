//! Scala: Cortex's `_CONFIG["scala"]` and the Scala arm of
//! `_extract_bindings` (the module before any `{` selector list).

use crate::ast::{Config, Extractor, text};
use crate::text::py_strip;
use tree_sitter::Node;

pub const CONFIG: Config = Config {
    // A package object's members are package-level API; a bodiless
    // `def` in a trait is a declaration.
    defs: &[
        ("class_definition", "class"),
        ("object_definition", "class"),
        ("package_object", "class"),
        ("trait_definition", "trait"),
        ("function_definition", "function"),
        ("function_declaration", "function"),
    ],
    calls: &["call_expression"],
    class_nodes: &[
        "class_definition",
        "object_definition",
        "package_object",
        "trait_definition",
    ],
    imports: &[],
    qualified: true,
};

/// `import a.b.{C, D}` imports `a.b`; `import a.b._` imports `a.b`.
pub fn extract_bindings(x: &mut Extractor, node: Node) {
    let whole = text(node, x.src);
    let rest = py_strip(whole.strip_prefix("import").unwrap_or(&whole));
    if rest.is_empty() {
        return;
    }
    let module = rest
        .split('{')
        .next()
        .unwrap_or("")
        .trim_end_matches(['.', '_', ' ']);
    x.imports.push(if module.is_empty() {
        rest.to_string()
    } else {
        module.to_string()
    });
}
