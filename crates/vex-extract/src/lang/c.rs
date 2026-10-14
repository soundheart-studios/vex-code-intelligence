//! C: Cortex's `_CONFIG["c"]`. C has no class nodes and no member-call
//! rewriting; its declaration walk and extern "C" recovery live in ast.rs.

use crate::ast::Config;

pub const CONFIG: Config = Config {
    defs: &[
        ("function_definition", "function"),
        ("struct_specifier", "struct"),
        ("enum_specifier", "enum"),
    ],
    calls: &["call_expression"],
    class_nodes: &[],
    imports: &[],
    qualified: false,
};
