//! C#: Cortex's `_CONFIG["csharp"]` and the C# arms of `declarations`
//! (namespaces own their members; every preprocessor branch is kept).

use crate::ast::Config;

pub const CONFIG: Config = Config {
    defs: &[
        ("method_declaration", "method"),
        ("class_declaration", "class"),
        ("interface_declaration", "interface"),
        ("struct_declaration", "struct"),
        ("enum_declaration", "enum"),
        ("property_declaration", "property"),
    ],
    calls: &["invocation_expression", "object_creation_expression"],
    class_nodes: &["class_declaration", "interface_declaration"],
    imports: &[],
    qualified: true,
};
