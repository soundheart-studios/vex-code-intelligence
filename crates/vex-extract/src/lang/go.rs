//! Go: Cortex's `_CONFIG["go"]` (qualified calls), the Go arm of
//! `_extract_bindings`, `_go_receiver_type`, `_go_receiver`,
//! `go_receiver_call` and the Go rows of `_member_types`.

use crate::ast::{Config, Extractor, field, is_identifier, named_children, text};
use crate::typed::{fields, partition, rpartition, type_desc};
use tree_sitter::Node;

pub const CONFIG: Config = Config {
    defs: &[
        ("function_declaration", "function"),
        ("method_declaration", "method"),
        ("type_spec", "type"),
        ("method_elem", "method"),
    ],
    calls: &["call_expression"],
    class_nodes: &["type_spec"],
    imports: &[],
    qualified: true,
};

/// `import "a/b"`, `import x "a/b"`, and grouped specs: the path, bound to
/// its alias or its last segment.
pub fn extract_bindings(x: &mut Extractor, node: Node) {
    let src = x.src;
    let mut specs: Vec<Node> = if node.kind() == "import_spec" {
        vec![node]
    } else {
        named_children(node)
            .into_iter()
            .filter(|c| c.kind() == "import_spec")
            .collect()
    };
    if specs.is_empty()
        && let Some(first) = node
            .named_child(0)
            .filter(|c| c.kind() == "import_spec_list")
    {
        specs = named_children(first)
            .into_iter()
            .filter(|c| c.kind() == "import_spec")
            .collect();
    }
    for spec in specs {
        let (mut alias, mut path) = (None, String::new());
        for c in named_children(spec) {
            if c.kind() == "package_identifier" {
                alias = Some(text(c, src));
            } else if c.kind().contains("string") {
                path = text(c, src).trim_matches('"').to_string();
            }
        }
        if !path.is_empty() {
            x.imports.push(path.clone());
            let name = match alias.filter(|a| !a.is_empty()) {
                Some(a) => a,
                None => path.rsplit('/').next().unwrap_or(&path).to_string(),
            };
            x.bindings.insert(name, (path, String::new()));
        }
    }
}

/// The receiver's parameter declaration and its type with pointers peeled.
fn receiver_parts(method: Node) -> Option<(Node, Option<Node>)> {
    let receiver = field(method, "receiver")?;
    let parameter = named_children(receiver)
        .into_iter()
        .find(|c| c.kind() == "parameter_declaration")?;
    let mut target = field(parameter, "type");
    while let Some(t) = target.filter(|t| t.kind() == "pointer_type") {
        target = t.named_child(0);
    }
    Some((parameter, target))
}

/// Cortex `_go_receiver_type`: the receiver's type name, generics dropped.
pub fn receiver_type(method: Node, src: &[u8]) -> Option<String> {
    let target = receiver_parts(method).and_then(|(_, t)| t);
    let name = target
        .map(|t| text(t, src).split('[').next().unwrap_or("").to_string())
        .unwrap_or_default();
    is_identifier(&name).then_some(name)
}

/// Cortex `_go_receiver`: (receiver name, type), or None when unnamed, `_`,
/// written in the body, or shadowed by a closure parameter.
fn receiver(method: Node, src: &[u8]) -> Option<(String, String)> {
    let (parameter, target) = receiver_parts(method)?;
    let name_node = field(parameter, "name")?;
    let target = target?;
    let name = text(name_node, src);
    let type_name = text(target, src)
        .split('[')
        .next()
        .unwrap_or("")
        .to_string();
    if name == "_" || !is_identifier(&type_name) {
        return None;
    }
    let mut stack = field(method, "body")
        .map(named_children)
        .unwrap_or_default();
    while let Some(node) = stack.pop() {
        if matches!(
            node.kind(),
            "assignment_statement"
                | "short_var_declaration"
                | "var_spec"
                | "inc_statement"
                | "dec_statement"
                | "range_clause"
        ) {
            let written = field(node, "left")
                .or_else(|| field(node, "name"))
                .unwrap_or(node);
            if std::iter::once(written)
                .chain(named_children(written))
                .any(|c| c.kind() == "identifier" && text(c, src) == name)
            {
                return None;
            }
        }
        if node.kind() == "parameter_declaration"
            && field(node, "name").is_some_and(|p| text(p, src) == name)
        {
            return None;
        }
        stack.extend(named_children(node));
    }
    Some((name, type_name))
}

/// Cortex `go_receiver_call`: `mx.handle()` inside `func (mx *Mux) ...` is
/// `(receiver:Mux).handle`; `mx.tree.find()` is `(thisfield:tree).find`.
pub fn receiver_call(x: &mut Extractor, name: &str, call: Node) -> String {
    let (receiver_chain, method) = rpartition(name);
    let (head, member) = partition(receiver_chain);
    if !(is_identifier(receiver_chain) || (is_identifier(head) && is_identifier(member))) {
        return name.to_string();
    }
    let mut scope = call.parent();
    while let Some(s) = scope {
        if matches!(s.kind(), "method_declaration" | "function_declaration") {
            break;
        }
        scope = s.parent();
    }
    let Some(scope) = scope.filter(|s| s.kind() == "method_declaration") else {
        return name.to_string();
    };
    let src = x.src;
    let evidence = x
        .go_receivers
        .entry(scope.start_byte())
        .or_insert_with(|| receiver(scope, src))
        .clone();
    match evidence {
        Some((n, _)) if n != head => name.to_string(),
        None => name.to_string(),
        Some(_) if !member.is_empty() => format!("(thisfield:{member}).{method}"),
        Some((_, ty)) => format!("(receiver:{ty}).{method}"),
    }
}

/// Cortex `_result_desc` for Go: declared results, comma-joined.
fn result_desc(node: Option<Node>, src: &[u8]) -> Option<String> {
    let node = node?;
    let parts: Vec<String> = if node.kind() == "parameter_list" {
        named_children(node)
            .into_iter()
            .filter(|c| c.kind() == "parameter_declaration")
            .map(|c| type_desc(field(c, "type"), "go", src).unwrap_or_default())
            .collect()
    } else {
        vec![type_desc(Some(node), "go", src).unwrap_or_default()]
    };
    parts.iter().any(|p| !p.is_empty()).then(|| parts.join(","))
}

/// Enclosing type specs, outermost first; None under an unnamed one.
fn owner_path(node: Node, src: &[u8]) -> Option<String> {
    let mut parts = Vec::new();
    let mut scope = node.parent();
    while let Some(s) = scope {
        if s.kind() == "type_spec" {
            parts.push(text(field(s, "name")?, src).replace("::", "."));
        }
        scope = s.parent();
    }
    parts.reverse();
    let joined = parts.join(".");
    (!joined.is_empty()).then_some(joined)
}

/// The Go rows of Cortex `_member_types`: struct fields with a plain or
/// slice type, a named slice type's element under `[]`, and a function's
/// declared results under `name()`.
pub fn member_types(root: Node, src: &[u8]) -> Vec<(String, String, String)> {
    let mut rows = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        stack.extend(named_children(node));
        match node.kind() {
            "field_declaration" => {
                let owner = owner_path(node, src);
                let kind = type_desc(field(node, "type"), "go", src);
                for name in fields(node, "name") {
                    if let (Some(o), Some(k)) = (&owner, &kind) {
                        rows.push((o.clone(), text(name, src), k.clone()));
                    }
                }
            }
            "type_spec" => {
                let kind = type_desc(field(node, "type"), "go", src);
                if let (Some(name), Some(k)) = (field(node, "name"), kind)
                    && let Some(element) = k.strip_prefix("[]")
                {
                    rows.push((text(name, src), "[]".to_string(), element.to_string()));
                }
            }
            "function_declaration" | "method_declaration" => {
                let results = result_desc(field(node, "result"), src);
                let owner = if node.kind() == "method_declaration" {
                    receiver_type(node, src).unwrap_or_default()
                } else {
                    String::new()
                };
                if let (Some(name), Some(r)) = (field(node, "name"), results)
                    && (!owner.is_empty() || node.kind() == "function_declaration")
                {
                    rows.push((owner, format!("{}()", text(name, src)), r));
                }
            }
            _ => {}
        }
    }
    rows
}
