//! Typed member-call evidence for languages whose functions declare their
//! locals' types (Cortex `_TYPED_SCOPES`): `_plain_type`, `_type_desc`,
//! `_declared_types`, `_member_chain`, `typed_receiver_call` and
//! `binding_exprs`. Language branches sit exactly where Cortex's do; each is
//! filled in as its language is wired (Go now; Java and Rust later).
//!
//! Binding expressions are carried in call names as Python's `json.dumps`
//! writes them, so [`py_dumps`] reproduces that format, `ensure_ascii`
//! escaping included.

use crate::ast::{Extractor, call_name, field, is_identifier, named_children, text};
use indexmap::{IndexMap, IndexSet};
use regex::Regex;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::OnceLock;
use tree_sitter::Node;

/// Function nodes whose parameters and locals type a receiver.
pub fn typed_scopes(language: &str) -> Option<&'static [&'static str]> {
    match language {
        "go" => Some(&["function_declaration", "method_declaration"]),
        "java" => Some(&["method_declaration", "constructor_declaration"]),
        "rust" => Some(&["function_item"]),
        _ => None,
    }
}

/// `node.children_by_field_name(name)`.
pub fn fields<'t>(node: Node<'t>, name: &str) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    node.children_by_field_name(name, &mut cursor).collect()
}

/// Python `s.rpartition('.')`: (before, after); ("", s) without a dot.
pub fn rpartition(s: &str) -> (&str, &str) {
    s.rsplit_once('.').unwrap_or(("", s))
}

/// Python `s.partition('.')`: (before, after); (s, "") without a dot.
pub fn partition(s: &str) -> (&str, &str) {
    s.split_once('.').unwrap_or((s, ""))
}

/// Cortex `_plain_type`: `*node`, `&'a Prerelease`, `List<String>` -> node,
/// Prerelease, List; None for anything one identifier cannot name.
pub fn plain_type(node: Option<Node>, src: &[u8]) -> Option<String> {
    static REJECT: OnceLock<Regex> = OnceLock::new();
    static STRIP: OnceLock<Regex> = OnceLock::new();
    let reject = REJECT.get_or_init(|| Regex::new(r"\b(?:dyn|impl)\b|[\[(.:]").unwrap());
    let strip = STRIP.get_or_init(|| Regex::new(r"&|\*|'\w+|\bmut\b").unwrap());
    let full = text(node?, src);
    let head = full.split('<').next().unwrap_or("");
    if reject.is_match(head) {
        return None;
    }
    let stripped = strip.replace_all(head, " ");
    let t = crate::text::py_strip(&stripped);
    (is_identifier(t) && t != "var" && t != "Self").then(|| t.to_string())
}

/// Cortex `_type_desc`: a plain type, or `[]Elem` for a Go slice or array.
pub fn type_desc(node: Option<Node>, language: &str, src: &[u8]) -> Option<String> {
    let mut node = node?;
    while language == "go" && node.kind() == "pointer_type" && node.named_child_count() > 0 {
        node = node.named_child(0)?;
    }
    if language == "go" && matches!(node.kind(), "slice_type" | "array_type") {
        return plain_type(field(node, "element"), src).map(|e| format!("[]{e}"));
    }
    plain_type(Some(node), src)
}

/// Cortex `_declared_types`: name -> its one declared plain type, or None
/// when it is a local bound untyped or with disagreeing types.
pub fn declared_types(scope: Node, language: &str, src: &[u8]) -> HashMap<String, Option<String>> {
    let mut seen: IndexMap<String, IndexSet<Option<String>>> = IndexMap::new();
    let bind = |seen: &mut IndexMap<String, IndexSet<Option<String>>>,
                name: Option<Node>,
                ty: Option<String>| {
        if let Some(n) = name.filter(|n| n.kind() == "identifier") {
            seen.entry(text(n, src)).or_default().insert(ty);
        }
    };
    let unknown = |seen: &mut IndexMap<String, IndexSet<Option<String>>>, node: Option<Node>| {
        let mut stack: Vec<Node> = node.into_iter().collect();
        while let Some(current) = stack.pop() {
            if current.kind() == "identifier" {
                seen.entry(text(current, src)).or_default().insert(None);
            }
            stack.extend(named_children(current));
        }
    };
    let mut stack = vec![scope];
    while let Some(node) = stack.pop() {
        stack.extend(named_children(node));
        let t = node.kind();
        if language == "java" {
            if matches!(t, "formal_parameter" | "enhanced_for_statement") {
                bind(
                    &mut seen,
                    field(node, "name"),
                    plain_type(field(node, "type"), src),
                );
            } else if t == "local_variable_declaration" {
                let ty = plain_type(field(node, "type"), src);
                for declarator in fields(node, "declarator") {
                    bind(&mut seen, field(declarator, "name"), ty.clone());
                }
            } else if matches!(
                t,
                "catch_formal_parameter" | "spread_parameter" | "inferred_parameters"
            ) {
                let target = if t == "catch_formal_parameter" {
                    field(node, "name")
                } else {
                    Some(node)
                };
                unknown(&mut seen, target);
            } else if t == "lambda_expression"
                && let Some(p) = field(node, "parameters").filter(|p| p.kind() == "identifier")
            {
                unknown(&mut seen, Some(p));
            }
        } else if language == "rust" {
            let pattern = field(node, "pattern");
            if matches!(t, "parameter" | "let_declaration")
                && pattern.is_some_and(|p| p.kind() == "identifier")
            {
                bind(&mut seen, pattern, plain_type(field(node, "type"), src));
            } else if pattern.is_some() {
                // for, if let, match arms, destructuring
                unknown(&mut seen, pattern);
            } else if t == "closure_parameters" {
                // `|x|`; `|x: T|` is a parameter
                for child in named_children(node) {
                    if child.kind() == "identifier" {
                        bind(&mut seen, Some(child), None);
                    }
                }
            }
        } else if language == "go" {
            if matches!(t, "parameter_declaration" | "var_spec") {
                let ty = plain_type(field(node, "type"), src);
                for name in fields(node, "name") {
                    bind(&mut seen, Some(name), ty.clone());
                }
            } else if t == "variadic_parameter_declaration" {
                unknown(&mut seen, field(node, "name"));
            } else if t == "short_var_declaration"
                || (t == "range_clause"
                    && crate::ast::children(node).iter().any(|c| c.kind() == ":="))
            {
                unknown(&mut seen, field(node, "left"));
            } else if t == "type_switch_statement" {
                unknown(&mut seen, field(node, "alias"));
            }
        }
    }
    seen.into_iter()
        .map(|(name, types)| {
            let only = (types.len() == 1)
                .then(|| types.into_iter().next().flatten())
                .flatten();
            (name, only)
        })
        .collect()
}

/// Cortex `_member_chain`: `r.f.m` for a call through one field of an
/// identifier, `self` or `this`.
pub fn member_chain(call: Node, language: &str, src: &[u8]) -> Option<String> {
    let (method, outer, inner_key, field_key, outer_kind) = if language == "java" {
        (
            field(call, "name"),
            field(call, "object"),
            "object",
            "field",
            "field_access",
        )
    } else {
        let outer_kind = if language == "go" {
            "selector_expression"
        } else {
            "field_expression"
        };
        let function = field(call, "function").filter(|f| f.kind() == outer_kind)?;
        let inner_key = if language == "go" { "operand" } else { "value" };
        (
            field(function, "field"),
            field(function, inner_key),
            inner_key,
            "field",
            outer_kind,
        )
    };
    let (method, outer) = (method?, outer?);
    if outer.kind() != outer_kind {
        return None;
    }
    let head = field(outer, inner_key)?;
    let member = field(outer, field_key)?;
    if !matches!(head.kind(), "identifier" | "self" | "this") {
        return None;
    }
    let parts = [text(head, src), text(member, src), text(method, src)];
    parts
        .iter()
        .all(|p| is_identifier(p))
        .then(|| parts.join("."))
}

/// Python `json.dumps(value)` (`compact`: `separators=(',', ':')`), with
/// `ensure_ascii`.
pub fn py_dumps(value: &Value, compact: bool) -> String {
    let mut out = String::new();
    dump(value, compact, &mut out);
    out
}

fn dump(value: &Value, compact: bool, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => {
            out.push('"');
            for c in s.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    '\u{8}' => out.push_str("\\b"),
                    '\u{c}' => out.push_str("\\f"),
                    // Python escapes everything outside ' '..'~', DEL included.
                    c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                        let mut buf = [0u16; 2];
                        for unit in c.encode_utf16(&mut buf) {
                            out.push_str(&format!("\\u{unit:04x}"));
                        }
                    }
                    c => out.push(c),
                }
            }
            out.push('"');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(if compact { "," } else { ", " });
                }
                dump(item, compact, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (k, v)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(if compact { "," } else { ", " });
                }
                dump(&Value::String(k.clone()), compact, out);
                out.push_str(if compact { ":" } else { ": " });
                dump(v, compact, out);
            }
            out.push('}');
        }
    }
}

/// Cortex `typed_receiver_call`: `(type:T).m` when the receiver has one
/// declared plain type in the function, `(typefield:T.f).m` through one of
/// its fields, `(thisfield:f).m` through self/this, `(expr:[...]).m` when a
/// Go or Rust local's type is an expression the resolver evaluates.
pub fn typed_receiver_call(x: &mut Extractor, name: &str, call: Node) -> String {
    let (receiver, method) = rpartition(name);
    let (head, member) = partition(receiver);
    if !(is_identifier(receiver) || (is_identifier(head) && is_identifier(member))) {
        return name.to_string();
    }
    if !member.is_empty() && (head == "self" || head == "this") {
        return format!("(thisfield:{member}).{method}");
    }
    let Some(scopes) = typed_scopes(x.language) else {
        return name.to_string();
    };
    let mut scope = call.parent();
    while let Some(s) = scope {
        if scopes.contains(&s.kind()) {
            break;
        }
        scope = s.parent();
    }
    let Some(scope) = scope else {
        return name.to_string();
    };
    let (src, language) = (x.src, x.language);
    let locals = x
        .declared_types
        .entry(scope.start_byte())
        .or_insert_with(|| declared_types(scope, language, src));
    let is_local = locals.contains_key(head);
    let declared = locals.get(head).cloned().flatten();
    if let Some(d) = &declared {
        return if member.is_empty() {
            format!("(type:{d}).{method}")
        } else {
            format!("(typefield:{d}.{member}).{method}")
        };
    }
    if matches!(language, "go" | "rust")
        && let Some(mut expression) = binding_exprs(x, scope).get(head).cloned()
    {
        if !member.is_empty() {
            expression = json!(["field", expression, member]);
        }
        return format!("(expr:{}).{method}", py_dumps(&expression, true));
    }
    if !member.is_empty() {
        return name.to_string();
    }
    if language == "java"
        && !is_local
        && head != "this"
        && head != "super"
        && head.chars().next().is_some_and(char::is_lowercase)
    {
        // Not a local and not a class by Java's naming convention: a field
        // of this class, `out.emit()`.
        return format!("(implicitfield:{head}).{method}");
    }
    name.to_string()
}

/// Cortex `binding_exprs`: name -> type expression for locals with no
/// single declared type, when every binding of the name agrees.
pub fn binding_exprs(x: &mut Extractor, scope: Node) -> IndexMap<String, Value> {
    let key = scope.start_byte();
    if let Some(cached) = x.binding_cache.get(&key) {
        return cached.clone();
    }
    // An initializer's own evidence sees none.
    x.binding_cache.insert(key, IndexMap::new());
    let language = x.language;
    let src = x.src;
    let mut seen: IndexMap<String, IndexSet<Option<String>>> = IndexMap::new();
    let bind = |seen: &mut IndexMap<String, IndexSet<Option<String>>>,
                name: Node,
                expression: Option<Value>| {
        if name.kind() == "identifier" {
            seen.entry(text(name, src))
                .or_default()
                .insert(expression.map(|e| py_dumps(&e, false)));
        }
    };
    let positional = |expression: &Option<Value>, index: usize| -> Option<Value> {
        match expression {
            Some(Value::Array(e)) if e.first() == Some(&json!("ret")) => {
                Some(json!(["ret", e[1], index, e[3]]))
            }
            _ => None,
        }
    };
    let mut stack = vec![scope];
    while let Some(node) = stack.pop() {
        stack.extend(named_children(node));
        let kind = node.kind();
        if language == "rust" {
            let pattern = field(node, "pattern");
            if matches!(kind, "parameter" | "let_declaration")
                && let Some(pattern) = pattern
            {
                let desc = plain_type(field(node, "type"), src);
                let value = field(node, "value");
                if pattern.kind() == "identifier" {
                    let expression = match &desc {
                        Some(d) => Some(json!(["T", d])),
                        None => expr_of(x, value),
                    };
                    bind(&mut seen, pattern, expression);
                } else if pattern.kind() == "tuple_pattern" && desc.is_none() {
                    let call = expr_of(x, value);
                    for (i, part) in named_children(pattern).into_iter().enumerate() {
                        bind(&mut seen, part, positional(&call, i));
                    }
                } else {
                    for part in named_children(pattern) {
                        bind(&mut seen, part, None);
                    }
                }
            } else if let Some(pattern) = pattern {
                // for, if let, match arms
                let mut ids = vec![pattern];
                while let Some(current) = ids.pop() {
                    if current.kind() == "identifier" {
                        bind(&mut seen, current, None);
                    }
                    ids.extend(named_children(current));
                }
            } else if kind == "closure_parameters" {
                for child in named_children(node) {
                    if child.kind() == "identifier" {
                        bind(&mut seen, child, None);
                    }
                }
            }
            continue;
        }
        if language != "go" {
            continue;
        }
        if matches!(kind, "parameter_declaration" | "var_spec") {
            let desc = type_desc(field(node, "type"), language, src);
            let value = field(node, "value");
            for name in fields(node, "name") {
                let expression = match &desc {
                    Some(d) => Some(json!(["T", d])),
                    None => match value {
                        Some(v) if v.named_child_count() == 1 => expr_of(x, v.named_child(0)),
                        _ => None,
                    },
                };
                bind(&mut seen, name, expression);
            }
        } else if kind == "short_var_declaration" {
            let left = field(node, "left").map(named_children).unwrap_or_default();
            let right = field(node, "right").map(named_children).unwrap_or_default();
            if right.len() == 1 && right[0].kind() == "call_expression" {
                let call = expr_of(x, Some(right[0]));
                for (i, n) in left.iter().enumerate() {
                    bind(&mut seen, *n, positional(&call, i));
                }
            } else if left.len() == right.len() {
                for (n, v) in left.iter().zip(&right) {
                    let e = expr_of(x, Some(*v));
                    bind(&mut seen, *n, e);
                }
            } else {
                for n in &left {
                    bind(&mut seen, *n, None);
                }
            }
        } else if kind == "range_clause"
            && crate::ast::children(node).iter().any(|c| c.kind() == ":=")
        {
            let left = field(node, "left").map(named_children).unwrap_or_default();
            let element = json!(["elem", expr_of(x, field(node, "right"))]);
            for (i, n) in left.iter().enumerate() {
                bind(&mut seen, *n, (i == 1).then(|| element.clone()));
            }
        } else if matches!(
            kind,
            "variadic_parameter_declaration" | "type_switch_statement"
        ) && let Some(target) = field(node, "name").or_else(|| field(node, "alias"))
        {
            for n in std::iter::once(target).chain(named_children(target)) {
                bind(&mut seen, n, None);
            }
        }
    }
    let agreed: IndexMap<String, Value> = seen
        .into_iter()
        .filter_map(|(name, keys)| {
            if keys.len() != 1 {
                return None;
            }
            let key = keys.into_iter().next().flatten()?;
            Some((name, serde_json::from_str(&key).expect("our own JSON")))
        })
        .collect();
    fn substitute(
        expression: Option<&Value>,
        agreed: &IndexMap<String, Value>,
        depth: usize,
    ) -> Option<Value> {
        let expression = expression?;
        if depth > 6 || expression.is_null() {
            return None;
        }
        let items = expression.as_array()?;
        match items.first().and_then(Value::as_str) {
            Some("var") => substitute(
                agreed.get(items[1].as_str().unwrap_or("")),
                agreed,
                depth + 1,
            ),
            Some(kind @ ("field" | "elem")) => {
                let inner = substitute(items.get(1), agreed, depth + 1)?;
                let mut out = vec![json!(kind), inner];
                out.extend(items.iter().skip(2).cloned());
                Some(Value::Array(out))
            }
            _ => Some(expression.clone()),
        }
    }
    let result: IndexMap<String, Value> = agreed
        .iter()
        .filter_map(|(n, e)| substitute(Some(e), &agreed, 0).map(|s| (n.clone(), s)))
        .collect();
    x.binding_cache.insert(key, result.clone());
    result
}

/// `expr_of` inside Cortex `binding_exprs`.
fn expr_of(x: &mut Extractor, node: Option<Node>) -> Option<Value> {
    let node = node?;
    let src = x.src;
    let language = x.language;
    match node.kind() {
        "parenthesized_expression" | "reference_expression" | "unary_expression" => {
            let kids = named_children(node);
            expr_of(x, kids.last().copied())
        }
        "identifier" => Some(json!(["var", text(node, src)])),
        "self" => Some(json!(["self"])),
        "selector_expression" => {
            let operand = expr_of(x, field(node, "operand"));
            let member = field(node, "field")
                .map(|f| text(f, src))
                .unwrap_or_default();
            Some(json!(["field", operand, member]))
        }
        "field_expression" if language == "rust" => {
            let member = field(node, "field")?;
            let value = expr_of(x, field(node, "value"));
            Some(json!(["field", value, text(member, src)]))
        }
        "index_expression" => {
            let target = field(node, "operand").or_else(|| node.named_child(0));
            Some(json!(["elem", expr_of(x, target)]))
        }
        "composite_literal" => {
            type_desc(field(node, "type"), language, src).map(|d| json!(["T", d]))
        }
        kind @ ("call_expression" | "try_expression") => {
            let unwrap = kind == "try_expression";
            let call = if unwrap {
                node.named_child(0).unwrap_or(node)
            } else {
                node
            };
            if call.kind() != "call_expression" {
                return None;
            }
            let written = call_name(call, src, x.cfg.qualified).filter(|w| !w.is_empty())?;
            let evidence = x.instance_call(written, call);
            // Only a call the package can resolve by itself: a bare function
            // or one already carrying type evidence.
            if evidence.is_empty() || (evidence.contains('.') && !evidence.starts_with('(')) {
                return None;
            }
            Some(json!(["ret", evidence, null, unwrap]))
        }
        _ => None,
    }
}
