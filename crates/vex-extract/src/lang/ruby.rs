//! Ruby: Cortex's `_CONFIG["ruby"]`. `class << self` is handled in the
//! shared engine's declarations walk; imports use the generic arm.

use crate::ast::Config;

pub const CONFIG: Config = Config {
    defs: &[
        ("method", "function"),
        ("singleton_method", "method"),
        ("class", "class"),
        ("module", "module"),
    ],
    calls: &["call"],
    class_nodes: &["class", "module"],
    imports: &[],
    qualified: false,
};
