//! Python: Cortex's `_CONFIG["python"]` and the Python branches of
//! `instance_call`, `_extract_bindings` and `_member_types`.

use crate::ast::{
    Config, Extractor, call_name, field, idents_in, is_identifier, named_children, text,
};
use std::collections::HashMap;
use tree_sitter::Node;

pub const CONFIG: Config = Config {
    defs: &[
        ("function_definition", "function"),
        ("class_definition", "class"),
        ("decorated_definition", "@wrapped"),
    ],
    calls: &["call"],
    class_nodes: &["class_definition"],
    imports: &[],
    qualified: true,
};

/// Statements that write a name, for the single-assignment rule.
const WRITES: &[&str] = &[
    "assignment",
    "augmented_assignment",
    "named_expression",
    "for_statement",
    "global_statement",
    "nonlocal_statement",
    "import_statement",
    "import_from_statement",
    "delete_statement",
    "as_pattern",
    "except_clause",
];

/// Conservative constructor evidence carried in a call name:
/// `super().m()` -> `(super).m`; `x.m()` where `x = C(...)` is the only
/// write to `x` in the function, straight-line and before the call ->
/// `(instance:C).m`.
pub fn instance_call(x: &mut Extractor, name: String, call: Node) -> String {
    if !name.contains('.') {
        return name;
    }
    let src = x.src;
    let function = field(call, "function");
    let receiver_node = function.and_then(|f| field(f, "object"));
    if let Some(r) = receiver_node
        && r.kind() == "call"
        && field(r, "function").is_some_and(|f| &src[f.byte_range()] == b"super")
    {
        return format!("(super).{}", name.rsplit('.').next().unwrap_or(""));
    }
    let (receiver, method) = name.rsplit_once('.').unwrap();
    if !is_identifier(receiver) || receiver == "self" || receiver == "cls" {
        return name;
    }
    let mut scope = call.parent();
    while let Some(s) = scope {
        if s.kind() == "function_definition" {
            break;
        }
        scope = s.parent();
    }
    let Some(scope) = scope else { return name };
    let key = scope.start_byte();
    let evidence = x
        .constructor_cache
        .entry(key)
        .or_insert_with(|| constructor_evidence(scope, src));
    match evidence.get(receiver) {
        Some((constructor, end_byte)) if *end_byte < call.start_byte() => {
            format!("(instance:{constructor}).{method}")
        }
        _ => name,
    }
}

/// name -> (constructor, assignment end byte) for names written exactly once
/// in the function, by a top-level `name = Call(...)` statement.
fn constructor_evidence(scope: Node, src: &[u8]) -> HashMap<String, (String, usize)> {
    let body = field(scope, "body");
    let mut writes: HashMap<String, usize> = HashMap::new();
    let mut candidates: HashMap<String, (String, usize)> = HashMap::new();
    let mut stack: Vec<Node> = body.map(named_children).unwrap_or_default();
    while let Some(current) = stack.pop() {
        if matches!(
            current.kind(),
            "function_definition" | "class_definition" | "lambda"
        ) {
            continue;
        }
        if WRITES.contains(&current.kind()) {
            let target = field(current, "left").or_else(|| field(current, "name"));
            let affected = match target {
                Some(t) => idents_in(t, src, 100),
                None => idents_in(current, src, 100),
            };
            for a in affected {
                *writes.entry(a).or_insert(0) += 1;
            }
            let value = field(current, "right");
            let statement = current.parent();
            if current.kind() == "assignment"
                && let (Some(t), Some(v), Some(st)) = (target, value, statement)
                && t.kind() == "identifier"
                && v.kind() == "call"
                && st.kind() == "expression_statement"
                && st.parent() == body
                && let Some(constructor) = call_name(v, src, true)
            {
                candidates.insert(text(t, src), (constructor, current.end_byte()));
            }
        }
        stack.extend(named_children(current));
    }
    candidates.retain(|k, _| writes.get(k) == Some(&1));
    candidates
}

/// Named import bindings and imported module strings.
pub fn extract_bindings(x: &mut Extractor, node: Node) {
    let src = x.src;
    match node.kind() {
        "import_from_statement" => {
            let kids = named_children(node);
            let module = kids.first().map(|m| text(*m, src)).unwrap_or_default();
            x.imports.push(module.clone());
            for c in kids.iter().skip(1) {
                match c.kind() {
                    "dotted_name" => {
                        let name = text(*c, src).split('.').next().unwrap_or("").to_string();
                        x.bindings.insert(name.clone(), (module.clone(), name));
                    }
                    "aliased_import" => {
                        let orig = c.named_child(0).map(|n| text(n, src)).unwrap_or_default();
                        if let Some(alias) = field(*c, "alias") {
                            x.bindings.insert(text(alias, src), (module.clone(), orig));
                        }
                    }
                    _ => {}
                }
            }
        }
        "import_statement" => {
            for c in named_children(node) {
                let target = if c.kind() == "aliased_import" {
                    c.named_child(0).unwrap_or(c)
                } else {
                    c
                };
                let module = text(target, src);
                x.imports.push(module.clone());
                let alias = if c.kind() == "aliased_import" {
                    field(c, "alias")
                } else {
                    None
                };
                let binding = match alias {
                    Some(a) => text(a, src),
                    None => module.split('.').next().unwrap_or("").to_string(),
                };
                x.bindings.insert(binding, (module, String::new()));
            }
        }
        _ => {}
    }
}

/// `(class, "(base)N", written base)` rows for class bases in declaration
/// order; only identifier and attribute bases count (not `metaclass=`).
pub fn member_types(root: Node, src: &[u8]) -> Vec<(String, String, String)> {
    let mut rows = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        stack.extend(named_children(node));
        if node.kind() != "class_definition" {
            continue;
        }
        let (Some(name), Some(bases)) = (field(node, "name"), field(node, "superclasses")) else {
            continue;
        };
        let outer = owner_path(node, src);
        let owner = [outer, Some(text(name, src))]
            .into_iter()
            .flatten()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(".");
        let mut position = 0;
        for base in named_children(bases) {
            if matches!(base.kind(), "identifier" | "attribute") {
                rows.push((owner.clone(), format!("(base){position}"), text(base, src)));
                position += 1;
            }
        }
    }
    rows
}

/// Enclosing classes and functions, outermost first; None for none (or an
/// unnamed owner, which Python's grammar does not produce).
fn owner_path(node: Node, src: &[u8]) -> Option<String> {
    let mut parts = Vec::new();
    let mut scope = node.parent();
    while let Some(s) = scope {
        if matches!(s.kind(), "class_definition" | "function_definition") {
            parts.push(text(field(s, "name")?, src).replace("::", "."));
        }
        scope = s.parent();
    }
    parts.reverse();
    let joined = parts.join(".");
    (!joined.is_empty()).then_some(joined)
}
