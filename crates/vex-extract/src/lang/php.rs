//! PHP: Cortex's `_CONFIG["php"]` and the PHP arm of `_extract_bindings`
//! (`use A\B\C as D` binds D to `A\B\C`). Namespaces own their members
//! (the shared engine's declarations walk).

use crate::ast::{Config, Extractor, text};
use crate::text::py_strip;
use tree_sitter::Node;

pub const CONFIG: Config = Config {
    defs: &[
        ("function_definition", "function"),
        ("method_declaration", "method"),
        ("class_declaration", "class"),
        ("interface_declaration", "interface"),
        ("trait_declaration", "trait"),
        ("enum_declaration", "enum"),
    ],
    calls: &[
        "function_call_expression",
        "member_call_expression",
        "scoped_call_expression",
    ],
    class_nodes: &[
        "class_declaration",
        "interface_declaration",
        "trait_declaration",
        "enum_declaration",
    ],
    imports: &["namespace_use_clause"],
    qualified: true,
};

/// `use Foo\Bar;` binds `Bar`; `use Foo\Bar as Baz;` binds `Baz`.
pub fn extract_bindings(x: &mut Extractor, node: Node) {
    let whole = text(node, x.src);
    let t = py_strip(&whole).trim_end_matches(';');
    let t = py_strip(t.strip_prefix("use").unwrap_or(t));
    if t.is_empty() {
        return;
    }
    let (module, local) = match t.rsplit_once(" as ") {
        Some((m, l)) => {
            let module = py_strip(m).trim_start_matches('\\').to_string();
            (module, py_strip(l).to_string())
        }
        None => {
            let module = py_strip(t).trim_start_matches('\\').to_string();
            let local = module.rsplit('\\').next().unwrap_or(&module).to_string();
            (module, local)
        }
    };
    let orig = module.rsplit('\\').next().unwrap_or(&module).to_string();
    x.imports.push(module.clone());
    x.bindings.insert(local, (module, orig));
}
