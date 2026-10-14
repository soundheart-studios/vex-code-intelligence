//! C++: Cortex's `_CONFIG["cpp"]`, `languages/cpp.py`'s `mask_annotations`,
//! the C++ helpers of astchunk (`_cpp_type`, `_cpp_declared_name`,
//! `_cpp_recovered_name`, `_cpp_without_return_type`, `_cpp_local_types`,
//! `_cpp_enclosing_class`), `cpp_member_call` and the C++ rows of
//! `_member_types`.

use crate::ast::{
    Config, Extractor, children, cpp_depths, field, is_identifier, named_children, text,
    top_level_scopes,
};
use crate::cpp_lex::{Kind, Token, tokens};
use crate::typed::fields;
use regex::Regex;
use regex::bytes::Regex as BytesRegex;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use tree_sitter::Node;

pub const CONFIG: Config = Config {
    defs: &[
        ("function_definition", "function"),
        ("class_specifier", "class"),
        ("struct_specifier", "struct"),
        ("union_specifier", "union"),
        ("enum_specifier", "enum"),
    ],
    calls: &["call_expression"],
    class_nodes: &["class_specifier", "struct_specifier", "union_specifier"],
    imports: &[],
    qualified: false,
};

// ---- annotation masking (languages/cpp.py) ---------------------------------

const EXPORT: &[&[u8]] = &[b"BASE_EXPORT", b"URL_IPC_EXPORT"];
const LIFETIME: &[&[u8]] = &[b"LIFETIME_BOUND", b"LIFETIME_CAPTURE_BY_THIS"];
const DECLARATOR_END: &[&[u8]] = &[
    b")",
    b"const",
    b"noexcept",
    b"&",
    b"&&",
    b"override",
    b"final",
];
const BODY_START: &[&[u8]] = &[b"{", b";", b"=", b":", b"noexcept", b"override", b"final"];
const DECLARATION_START: &[&[u8]] = &[b";", b"{", b"}", b":", b">", b")", b"]"];
const SPECIFIERS: &[&[u8]] = &[
    b"constexpr",
    b"consteval",
    b"constinit",
    b"explicit",
    b"static",
    b"inline",
    b"virtual",
    b"friend",
    b"extern",
];

fn export_name(value: &[u8]) -> bool {
    static R: OnceLock<BytesRegex> = OnceLock::new();
    R.get_or_init(|| BytesRegex::new(r"^[A-Z][A-Z0-9_]*_EXPORT(?:_PRIVATE)?$").unwrap())
        .is_match(value)
}

fn trailing_macro(value: &[u8]) -> bool {
    static R: OnceLock<BytesRegex> = OnceLock::new();
    R.get_or_init(|| BytesRegex::new(r"^[A-Z][A-Z0-9]*(?:_[A-Z0-9]+)+$").unwrap())
        .is_match(value)
}

/// Blank known declaration annotations (export macros, lifetime and
/// trailing attribute macros) while preserving every byte offset and line
/// ending. Not macro expansion: unknown macros keep the grammar's reading.
pub fn mask_annotations(source: &[u8]) -> Vec<u8> {
    let toks: Vec<Token> = tokens(source);
    let value = |t: &Token| &source[t.start..t.end];
    let is_name = |t: &Token| t.kind == Kind::Name;
    let mut result = source.to_vec();
    let mut depth = 0usize;
    // The previous token that is code, skipping comments and directives.
    let code_before = |mut i: usize| -> &[u8] {
        while i > 0 {
            i -= 1;
            let t = &toks[i];
            let v = value(t);
            let trimmed = v.trim_ascii_start();
            if t.kind != Kind::Opaque
                || !(trimmed.starts_with(b"//")
                    || trimmed.starts_with(b"/*")
                    || trimmed.starts_with(b"#"))
            {
                return v;
            }
        }
        b";" // the start of the file begins a declaration
    };
    let blank = |result: &mut Vec<u8>, from: usize, to: usize, keep_newlines: bool| {
        for b in &mut result[from..to] {
            if !keep_newlines || !matches!(*b, b'\n' | b'\r') {
                *b = b' ';
            }
        }
    };
    for i in 0..toks.len() {
        let token = &toks[i];
        if token.kind == Kind::Opaque {
            continue;
        }
        let v = value(token);
        if v == b"(" {
            depth += 1;
        } else if v == b")" {
            depth = depth.saturating_sub(1);
        }
        if i == 0 {
            continue;
        }
        let previous = value(&toks[i - 1]);
        let mut end = i + 1;
        if depth > 0 {
            // Inside a parameter list only a lifetime annotation on a
            // parameter is masked.
            if !LIFETIME.contains(&v)
                || !is_name(&toks[i - 1])
                || end >= toks.len()
                || !matches!(value(&toks[end]), b")" | b"," | b"=" | b"[")
            {
                continue;
            }
            blank(&mut result, token.start, token.end, false);
            continue;
        }
        if matches!(previous, b"class" | b"struct" | b"union") {
            if v == b"COMPONENT_EXPORT" {
                // Only the documented single identifier argument qualifies.
                if i + 3 >= toks.len()
                    || value(&toks[i + 1]) != b"("
                    || !is_name(&toks[i + 2])
                    || value(&toks[i + 3]) != b")"
                {
                    continue;
                }
                end = i + 4;
            } else if !EXPORT.contains(&v)
                && (!export_name(v)
                    || end + 1 >= toks.len()
                    || !matches!(value(&toks[end + 1]), b"{" | b":" | b"final"))
            {
                continue;
            }
            if end >= toks.len() || !is_name(&toks[end]) {
                continue; // class BASE_EXPORT {} is a real class name
            }
        } else if trailing_macro(v)
            && DECLARATION_START.contains(&code_before(i))
            && end < toks.len()
            && SPECIFIERS.contains(&value(&toks[end]))
        {
            // a leading attribute macro before a specifier keyword
        } else if LIFETIME.contains(&v) || trailing_macro(v) {
            if !DECLARATOR_END.contains(&previous) {
                continue;
            }
            if !LIFETIME.contains(&v) && end < toks.len() && value(&toks[end]) == b"(" {
                let (mut close, mut nest) = (end, 0i64);
                while close < toks.len() {
                    nest += i64::from(value(&toks[close]) == b"(")
                        - i64::from(value(&toks[close]) == b")");
                    close += 1;
                    if nest == 0 {
                        break;
                    }
                }
                end = close;
            }
            // With arguments it must lead into a body or initializer list.
            let follows: &[&[u8]] = if value(&toks[end - 1]) == b")" && !LIFETIME.contains(&v) {
                &[b"{", b":"]
            } else {
                BODY_START
            };
            if end >= toks.len() || !follows.contains(&value(&toks[end])) {
                continue;
            }
        } else {
            continue;
        }
        blank(&mut result, token.start, toks[end - 1].end, true);
    }
    result
}

// ---- astchunk helpers -----------------------------------------------------

/// Cortex `_cpp_type`: the last scope segment of a declared type; any
/// `std::` type -> `std`; `auto`, primitives and the rest -> None.
pub fn cpp_type(node: Option<Node>, src: &[u8]) -> Option<String> {
    let node = node?;
    if matches!(node.kind(), "placeholder_type_specifier" | "primitive_type") {
        return None;
    }
    let full = text(node, src);
    let head = crate::text::py_strip(full.split('<').next().unwrap_or(""));
    if head.trim_start_matches(':').starts_with("std::") {
        return Some("std".to_string());
    }
    let last = head.rsplit("::").next().unwrap_or(head);
    is_identifier(last).then(|| last.to_string())
}

/// Cortex `_cpp_without_return_type`: drop everything up to the last
/// top-level whitespace; template arguments and operator names keep theirs.
pub fn without_return_type(name: &str) -> String {
    let depths = cpp_depths(name);
    let chars: Vec<char> = name.chars().collect();
    let cut = chars
        .iter()
        .enumerate()
        .filter(|(i, c)| crate::text::is_py_whitespace(**c) && depths[*i] == 0)
        .map(|(i, _)| i)
        .max();
    match cut {
        Some(c) => chars[c + 1..].iter().collect(),
        None => name.to_string(),
    }
}

/// Cortex `_cpp_recovered_name`: the real name of a function the grammar
/// split at a return type, or None when the tree is sound.
pub fn recovered_name(function: Node, src: &[u8]) -> Option<String> {
    static R: OnceLock<Regex> = OnceLock::new();
    let declarator = field(function, "declarator")?;
    let body = field(function, "body")?;
    if declarator.kind() == "function_declarator"
        || !children(function).iter().any(|c| c.kind() == "ERROR")
    {
        return None;
    }
    let between = crate::text::decode_replace(&src[declarator.start_byte()..body.start_byte()]);
    let re = R.get_or_init(|| Regex::new(r"(operator\s*[^\s(]+|~?[A-Za-z_]\w*)\s*\(").unwrap());
    re.captures(&between).map(|c| c[1].to_string())
}

/// Cortex `_cpp_declared_name`: (name, plain) for a declarator; plain is
/// false for arrays and function pointers.
pub fn declared_name(declarator: Node, src: &[u8]) -> (Option<String>, bool) {
    let mut plain = true;
    let mut d = Some(declarator);
    while let Some(n) = d {
        if matches!(n.kind(), "identifier" | "field_identifier") {
            return (Some(text(n, src)), plain);
        }
        if matches!(
            n.kind(),
            "array_declarator" | "function_declarator" | "structured_binding_declarator"
        ) {
            plain = false;
        }
        if n.kind() == "structured_binding_declarator" {
            return (None, false);
        }
        d = field(n, "declarator");
    }
    (None, false)
}

/// Cortex `_cpp_local_types`: name -> declared types of every parameter and
/// local binding of it (None for untyped or non-plain).
fn local_types(function: Node, src: &[u8]) -> HashMap<String, HashSet<Option<String>>> {
    let mut seen: HashMap<String, HashSet<Option<String>>> = HashMap::new();
    let mut stack = vec![function];
    while let Some(node) = stack.pop() {
        stack.extend(named_children(node));
        match node.kind() {
            "parameter_declaration"
            | "optional_parameter_declaration"
            | "declaration"
            | "for_range_loop" => {
                for declarator in fields(node, "declarator") {
                    let (name, plain) = declared_name(declarator, src);
                    if let Some(name) = name.filter(|n| !n.is_empty()) {
                        let ty = if plain {
                            cpp_type(field(node, "type"), src)
                        } else {
                            None
                        };
                        seen.entry(name).or_default().insert(ty);
                    }
                }
            }
            "structured_binding_declarator" | "lambda_capture_specifier" => {
                for child in named_children(node) {
                    if child.kind() == "identifier" {
                        seen.entry(text(child, src)).or_default().insert(None);
                    }
                }
            }
            _ => {}
        }
    }
    seen
}

/// Cortex `_cpp_enclosing_class`: the class whose body holds the function,
/// or the scope of an out-of-class `Cls::method` definition.
fn enclosing_class(function: Node, src: &[u8]) -> Option<String> {
    static TEMPLATE: OnceLock<Regex> = OnceLock::new();
    let mut parent = function.parent();
    while let Some(p) = parent {
        if matches!(p.kind(), "class_specifier" | "struct_specifier") {
            return field(p, "name")
                .map(|n| text(n, src).rsplit("::").next().unwrap_or("").to_string());
        }
        if matches!(p.kind(), "function_definition" | "translation_unit") {
            break;
        }
        parent = p.parent();
    }
    let mut declarator = field(function, "declarator");
    while let Some(d) = declarator.filter(|d| d.kind() != "function_declarator") {
        declarator = field(d, "declarator");
    }
    let target = declarator.and_then(|d| field(d, "declarator"))?;
    if target.kind() != "qualified_identifier" {
        return None;
    }
    let full = text(target, src);
    let scope = full.rsplit_once("::").map(|(s, _)| s).unwrap_or(&full);
    let last = scope.rsplit("::").next().unwrap_or(scope);
    let re = TEMPLATE.get_or_init(|| Regex::new(r"<.*").unwrap());
    let last = re.replace(last, "");
    let last = crate::text::py_strip(&last);
    is_identifier(last).then(|| last.to_string())
}

/// `class name -> field -> declared type` for classes in this file.
fn class_fields(root: Node, src: &[u8]) -> HashMap<String, HashMap<String, Option<String>>> {
    let mut classes: HashMap<String, HashMap<String, Option<String>>> = HashMap::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        stack.extend(named_children(node));
        if !matches!(node.kind(), "class_specifier" | "struct_specifier") {
            continue;
        }
        let (Some(name), Some(body)) = (field(node, "name"), field(node, "body")) else {
            continue;
        };
        let key = text(name, src)
            .rsplit("::")
            .next()
            .unwrap_or("")
            .to_string();
        let entries = classes.entry(key).or_default();
        for member in named_children(body) {
            if member.kind() != "field_declaration" {
                continue;
            }
            for declarator in fields(member, "declarator") {
                let (name, plain) = declared_name(declarator, src);
                if let Some(f) = name.filter(|n| !n.is_empty()) {
                    let typed = if plain {
                        cpp_type(field(member, "type"), src)
                    } else {
                        None
                    };
                    let merged = match entries.get(&f) {
                        Some(existing) if *existing != typed => None,
                        _ => typed,
                    };
                    entries.insert(f, merged);
                }
            }
        }
    }
    classes
}

fn single(types: Option<&HashSet<Option<String>>>) -> Option<String> {
    let types = types?;
    if types.len() == 1 && !types.contains(&None) {
        types.iter().next().cloned().flatten()
    } else {
        None
    }
}

/// Cortex `cpp_member_call`: `iter_->key()` is a call on whatever `iter_`
/// is, never on `this`; `Status::OK()` carries its scope.
pub fn member_call(x: &mut Extractor, name: String, call: Node) -> String {
    static TRAILING_TEMPLATE: OnceLock<Regex> = OnceLock::new();
    let src = x.src;
    let function = field(call, "function");
    if let Some(f) = function.filter(|f| f.kind() == "qualified_identifier")
        && !name.contains('.')
    {
        let written = text(f, src).replace(' ', "");
        let mut scopes = top_level_scopes(&written);
        scopes.pop();
        let re = TRAILING_TEMPLATE.get_or_init(|| Regex::new(r"<.*>$").unwrap());
        let scopes: Vec<String> = scopes
            .into_iter()
            .filter(|s| !s.is_empty())
            .map(|s| re.replace(&s, "").to_string())
            .collect();
        if scopes.is_empty() || !scopes.iter().all(|s| is_identifier(s)) {
            return name;
        }
        let scope = if scopes[0] == "std" {
            "std".to_string()
        } else {
            scopes.join(".")
        };
        return format!("(scope:{scope}).{name}");
    }
    let Some(function) = function.filter(|f| f.kind() == "field_expression") else {
        return name;
    };
    if name.contains('.') {
        return name;
    }
    let Some(receiver) = field(function, "argument").filter(|r| r.kind() != "this") else {
        return name;
    };
    let mut scope = call.parent();
    while let Some(s) = scope {
        if s.kind() == "function_definition" {
            break;
        }
        scope = s.parent();
    }
    let locals = scope.map(|s| {
        x.cpp_locals
            .entry(s.start_byte())
            .or_insert_with(|| local_types(s, src))
            .clone()
    });
    let locals = locals.unwrap_or_default();
    if receiver.kind() == "field_expression" {
        // `this->versions_->Mark()` or `cmp.field.m()` (ADR 20).
        let inner = field(receiver, "argument");
        let member = field(receiver, "field");
        if let (Some(inner), Some(member)) = (inner, member)
            && member.kind() == "field_identifier"
        {
            let f = text(member, src);
            if inner.kind() == "this" {
                return format!("(thisfield:{f}).{name}");
            }
            if inner.kind() == "identifier"
                && let Some(typed) = single(locals.get(&text(inner, src)))
                && typed != "std"
            {
                return format!("(typefield:{typed}.{f}).{name}");
            }
        }
        return format!("(member).{name}");
    }
    if receiver.kind() != "identifier" {
        return format!("(member).{name}");
    }
    let variable = text(receiver, src);
    let Some(scope) = scope else {
        return format!("(member).{name}");
    };
    let mut declared: Option<HashSet<Option<String>>> = locals.get(&variable).cloned();
    let mut owner = None;
    if declared.is_none() {
        owner = enclosing_class(scope, src);
        if let Some(o) = &owner {
            if x.cpp_fields.is_none() {
                let root = {
                    let mut r = call;
                    while let Some(p) = r.parent() {
                        r = p;
                    }
                    r
                };
                x.cpp_fields = Some(class_fields(root, src));
            }
            let field_types = x.cpp_fields.as_ref().and_then(|c| c.get(o));
            declared = field_types
                .and_then(|ft| ft.get(&variable))
                .map(|t| HashSet::from([t.clone()]));
        }
    }
    if let Some(t) = single(declared.as_ref()) {
        return format!("(type:{t}).{name}");
    }
    if declared.is_none() && owner.is_some() {
        // Not a local and not a field this file declares: a field the
        // class's header declares.
        return format!("(thisfield:{variable}).{name}");
    }
    format!("(member).{name}")
}

/// Enclosing namespaces and classes, outermost first; `(anonymous)` for an
/// unnamed namespace, None under an unnamed class or struct.
fn owner_path(node: Node, src: &[u8]) -> Option<String> {
    let mut parts = Vec::new();
    let mut scope = node.parent();
    while let Some(s) = scope {
        if matches!(
            s.kind(),
            "namespace_definition" | "class_specifier" | "struct_specifier"
        ) {
            match field(s, "name") {
                None if s.kind() == "namespace_definition" => parts.push("(anonymous)".to_string()),
                None => return None,
                Some(n) => parts.push(text(n, src).replace("::", ".")),
            }
        }
        scope = s.parent();
    }
    parts.reverse();
    let joined = parts.join(".");
    (!joined.is_empty()).then_some(joined)
}

/// The C++ rows of Cortex `_member_types`: fields with a plain type.
pub fn member_types(root: Node, src: &[u8]) -> Vec<(String, String, String)> {
    let mut rows = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        stack.extend(named_children(node));
        if node.kind() != "field_declaration" {
            continue;
        }
        let owner = owner_path(node, src);
        for declarator in fields(node, "declarator") {
            let (name, plain) = declared_name(declarator, src);
            let kind = if plain {
                cpp_type(field(node, "type"), src)
            } else {
                None
            };
            if let (Some(o), Some(n), Some(k)) = (&owner, name.filter(|n| !n.is_empty()), kind) {
                rows.push((o.clone(), n, k));
            }
        }
    }
    rows
}
