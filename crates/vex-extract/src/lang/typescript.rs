//! TypeScript and TSX: Cortex's `_CONFIG["typescript"]` and `_CONFIG["tsx"]`
//! (identical node tables; `.tsx` files only switch the grammar). Instance
//! calls and import bindings are the JavaScript family's.

use crate::ast::Config;

pub const CONFIG: Config = Config {
    defs: &[
        ("function_declaration", "function"),
        ("class_declaration", "class"),
        ("method_definition", "method"),
        ("interface_declaration", "interface"),
        ("enum_declaration", "enum"),
        ("type_alias_declaration", "type"),
        // a function bound to a name -- see ast::function_binding
        ("variable_declarator", "function"),
    ],
    calls: &["call_expression", "new_expression"],
    class_nodes: &["class_declaration"],
    imports: &[],
    qualified: true,
};
