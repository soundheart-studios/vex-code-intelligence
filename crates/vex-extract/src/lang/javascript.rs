//! JavaScript: Cortex's `_CONFIG["javascript"]`, and the JS-family
//! branches TypeScript shares: `_js_instance_call`, `_js_binding` and the
//! JavaScript arm of `_extract_bindings`.

use crate::ast::{Config, Extractor, field, idents_in, is_identifier, named_children, text};
use tree_sitter::Node;

pub const CONFIG: Config = Config {
    defs: &[
        ("function_declaration", "function"),
        ("class_declaration", "class"),
        ("method_definition", "method"),
        // a function bound to a name -- see ast::function_binding
        ("variable_declarator", "function"),
    ],
    calls: &["call_expression", "new_expression"],
    class_nodes: &["class_declaration"],
    imports: &[],
    qualified: true,
};

/// Function nodes: each binds its own parameters and declarations.
const JS_FUNCTIONS: &[&str] = &[
    "arrow_function",
    "function_expression",
    "function_declaration",
    "method_definition",
    "generator_function",
    "generator_function_declaration",
];

/// `ky.#raceBodyRead()` where `const ky = new Ky(...)`: carried as
/// `(instance:Ky).#raceBodyRead`. The nearest function, from the call
/// outward, that binds the name decides; a parameter of that name shadows
/// any outer binding. Only a `const`, or a `let` never assigned again,
/// initialised with `new C(...)` counts.
pub fn instance_call(x: &Extractor, name: String, call: Node) -> String {
    let src = x.src;
    let (receiver, method) = match name.rsplit_once('.') {
        Some(parts) => parts,
        None => return name,
    };
    if !is_identifier(receiver) || receiver == "this" || receiver == "super" {
        return name;
    }
    let mut scope = call.parent();
    while let Some(s) = scope {
        if JS_FUNCTIONS.contains(&s.kind()) {
            let parameters = field(s, "parameters").or_else(|| field(s, "parameter"));
            if parameters.is_some_and(|p| idents_in(p, src, 100).iter().any(|i| i == receiver)) {
                return name;
            }
            if let Some(found) = binding(s, receiver, src) {
                return if found.is_empty() {
                    name
                } else {
                    format!("(instance:{found}).{method}")
                };
            }
        }
        scope = s.parent();
    }
    name
}

/// The class `name` is constructed from in this function's own body: the
/// class name, "" when bound otherwise or reassigned, None when not bound.
fn binding(function: Node, name: &str, src: &[u8]) -> Option<String> {
    let body = field(function, "body");
    let mut declared: Option<String> = None;
    let mut writes = 0usize;
    let mut stack: Vec<(Node, bool)> = body.map(|b| vec![(b, false)]).unwrap_or_default();
    while let Some((node, nested)) = stack.pop() {
        // A nested function's declarations are its own, but a write there
        // to the captured name still reassigns it.
        let nested = nested || JS_FUNCTIONS.contains(&node.kind());
        if matches!(node.kind(), "lexical_declaration" | "variable_declaration") && !nested {
            let keyword = node.child(0).map(|k| text(k, src)).unwrap_or_default();
            for declarator in named_children(node) {
                let target = field(declarator, "name");
                if declarator.kind() != "variable_declarator"
                    || target.is_none_or(|t| text(t, src) != name)
                {
                    continue;
                }
                let constructor = field(declarator, "value")
                    .filter(|v| v.kind() == "new_expression")
                    .and_then(|v| field(v, "constructor"));
                declared = Some(match constructor {
                    Some(c)
                        if c.kind() == "identifier"
                            && matches!(keyword.as_str(), "const" | "let") =>
                    {
                        text(c, src)
                    }
                    _ => String::new(),
                });
            }
        } else if matches!(
            node.kind(),
            "assignment_expression" | "augmented_assignment_expression" | "update_expression"
        ) {
            let left = field(node, "left").or_else(|| field(node, "argument"));
            if left.is_some_and(|l| l.kind() == "identifier" && text(l, src) == name) {
                writes += 1;
            }
        }
        stack.extend(named_children(node).into_iter().map(|c| (c, nested)));
    }
    let declared = declared?;
    Some(if writes > 0 { String::new() } else { declared })
}

/// `import d, { a as b, c } from "m"` and `import * as ns from "m"`: the
/// module string and its named bindings.
pub fn extract_bindings(x: &mut Extractor, node: Node) {
    if node.kind() != "import_statement" {
        return;
    }
    let src = x.src;
    let module = field(node, "source")
        .map(|s| strip_quotes(&text(s, src)))
        .unwrap_or_default();
    x.imports.push(module.clone());
    let mut stack = named_children(node);
    while let Some(c) = stack.pop() {
        match c.kind() {
            "import_specifier" => {
                let orig = field(c, "name").map(|n| text(n, src)).unwrap_or_default();
                let local = field(c, "alias")
                    .map(|a| text(a, src))
                    .unwrap_or_else(|| orig.clone());
                if !local.is_empty() {
                    x.bindings.insert(local, (module.clone(), orig));
                }
            }
            "identifier" => {
                x.bindings
                    .insert(text(c, src), (module.clone(), "default".to_string()));
            }
            "namespace_import" => {
                for cc in named_children(c) {
                    if cc.kind() == "identifier" {
                        x.bindings
                            .insert(text(cc, src), (module.clone(), "*".to_string()));
                    }
                }
            }
            _ => stack.extend(named_children(c)),
        }
    }
}

/// Python `s.strip("'\"")`.
fn strip_quotes(s: &str) -> String {
    s.trim_matches(|c| c == '\'' || c == '"').to_string()
}
