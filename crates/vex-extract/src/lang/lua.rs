//! Lua: Cortex's `_CONFIG["lua"]`, `_lua_binding`, and the Lua arm of
//! `_extract_bindings` (`local x = require("m")` binds x to module m).
//! Most Lua modules define functions by assignment; the binding is the
//! definition (ADR 7).

use crate::ast::{Config, Extractor, field, named_children, text};
use regex::Regex;
use std::sync::LazyLock;
use tree_sitter::Node;

pub const CONFIG: Config = Config {
    defs: &[("function_declaration", "function")],
    calls: &["function_call"],
    class_nodes: &[],
    imports: &[],
    qualified: true,
};

/// (target, value) of a single-name binding, or None. `a, b = f, g` binds
/// several names and is left alone; a field without a name is a list
/// element.
pub fn binding<'t>(node: Node<'t>) -> Option<(Node<'t>, Node<'t>)> {
    if node.kind() == "field" {
        let (target, value) = (field(node, "name")?, field(node, "value")?);
        return (target.kind() == "identifier").then_some((target, value));
    }
    let kids = named_children(node);
    let variables = kids.iter().find(|c| c.kind() == "variable_list")?;
    let values = kids.iter().find(|c| c.kind() == "expression_list")?;
    let (vars, vals) = (named_children(*variables), named_children(*values));
    if vars.len() != 1 || vals.len() != 1 {
        return None;
    }
    matches!(vars[0].kind(), "identifier" | "dot_index_expression").then_some((vars[0], vals[0]))
}

static REQUIRE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:(?P<local>[A-Za-z_]\w*)\s*=\s*)?require\s*\(?\s*['"](?P<module>[^'"]+)"#)
        .expect("valid regex")
});

/// `require "m"` imports m; `local x = require("m")` also binds x.
pub fn extract_bindings(x: &mut Extractor, node: Node) {
    let whole = text(node, x.src);
    let Some(m) = REQUIRE.captures(&whole) else {
        return;
    };
    let module = m["module"].to_string();
    x.imports.push(module.clone());
    if let Some(local) = m.name("local") {
        x.bindings
            .insert(local.as_str().to_string(), (module, String::new()));
    }
}
