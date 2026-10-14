//! Swift: Cortex's `_CONFIG["swift"]` and the Swift arm of `declarations`
//! (an `extension` owns its members under the extended type's name).

use crate::ast::Config;

pub const CONFIG: Config = Config {
    defs: &[
        ("function_declaration", "function"),
        ("class_declaration", "class"),
        ("protocol_declaration", "interface"),
        ("protocol_function_declaration", "function"),
        ("property_declaration", "property"),
    ],
    calls: &["call_expression"],
    class_nodes: &["class_declaration", "protocol_declaration"],
    imports: &[],
    qualified: false,
};
