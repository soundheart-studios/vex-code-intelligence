//! Canonical JSON: the exact bytes Cortex's `scripts/export_golden.py`
//! writes, i.e. `json.dumps(v, sort_keys=True, separators=(",", ":"),
//! ensure_ascii=False) + "\n"`.
//!
//! The writer sorts object keys itself rather than relying on serde_json's
//! map order, which a `preserve_order` feature enabled anywhere in the
//! dependency graph would silently change.

use crate::model::Parsed;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

/// Every Parsed field in Cortex's canonical shape and order
/// (`export_golden.canonical`). `None` is the empty Parsed Cortex writes for a
/// file it could not parse.
pub fn to_canonical(parsed: Option<&Parsed>) -> Value {
    let Some(p) = parsed else {
        return json!({"chunks": [], "symbols": [], "imports": [], "refs": {}, "bindings": {}, "calls": [], "members": []});
    };
    let mut chunks: Vec<&crate::Chunk> = p.chunks.iter().collect();
    chunks
        .sort_by(|a, b| (a.start_line, a.end_line, &a.id).cmp(&(b.start_line, b.end_line, &b.id)));
    let chunks: Vec<Value> = chunks
        .into_iter()
        .map(|c| {
            json!({
                "id": c.id, "file_path": c.file_path, "start_line": c.start_line, "end_line": c.end_line,
                "kind": c.kind, "language": c.language,
                "metadata": c.metadata.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<Map<_, _>>(),
                "content_sha256": format!("{:x}", Sha256::digest(c.content.as_bytes())),
            })
        })
        .collect();

    let mut symbols: Vec<&crate::Symbol> = p.symbols.iter().collect();
    symbols.sort_by(|a, b| {
        (a.start_line, a.end_line, &a.qualified_name).cmp(&(
            b.start_line,
            b.end_line,
            &b.qualified_name,
        ))
    });
    let symbols: Vec<Value> = symbols
        .into_iter()
        .map(|s| {
            json!({
                "qualified_name": s.qualified_name, "name": s.name, "kind": s.kind, "file_path": s.file_path,
                "start_line": s.start_line, "end_line": s.end_line, "declaration": s.declaration,
            })
        })
        .collect();

    let mut imports = p.imports.clone();
    imports.sort();
    let refs: Map<String, Value> = p
        .refs
        .iter()
        .map(|(k, v)| (k.clone(), Value::from(*v)))
        .collect();
    let bindings: Map<String, Value> = p
        .bindings
        .iter()
        .map(|(k, (m, o))| (k.clone(), json!([m, o])))
        .collect();
    let mut calls: Vec<(&String, &String, u64)> =
        p.calls.iter().map(|((c, n), k)| (c, n, *k)).collect();
    calls.sort();
    let mut members: Vec<&(String, String, String)> = p.members.iter().collect();
    members.sort();

    json!({
        "chunks": chunks,
        "symbols": symbols,
        "imports": imports,
        "refs": refs,
        "bindings": bindings,
        "calls": calls.into_iter().map(|(c, n, k)| json!([c, n, k])).collect::<Vec<_>>(),
        "members": members.into_iter().map(|(o, m, t)| json!([o, m, t])).collect::<Vec<_>>(),
    })
}

/// Canonical bytes of a value, trailing newline included.
pub fn to_bytes(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    write_value(&mut out, value);
    out.push(b'\n');
    out
}

/// The canonical golden-file bytes for a Parsed.
pub fn parsed_bytes(parsed: Option<&Parsed>) -> Vec<u8> {
    to_bytes(&to_canonical(parsed))
}

fn write_value(out: &mut Vec<u8>, v: &Value) {
    match v {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Value::Number(n) => out.extend_from_slice(n.to_string().as_bytes()),
        Value::String(s) => write_str(out, s),
        Value::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_value(out, item);
            }
            out.push(b']');
        }
        Value::Object(map) => {
            // Python sorts keys by code point; UTF-8 byte order is the same.
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push(b'{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_str(out, k);
                out.push(b':');
                write_value(out, &map[k]);
            }
            out.push(b'}');
        }
    }
}

/// Python's `py_encode_basestring` with `ensure_ascii=False`: escape `"`,
/// `\` and C0 controls only; `\b \f \n \r \t` take their short forms, other
/// controls `\u00xx` in lowercase hex. DEL, U+2028/U+2029 and every
/// non-ASCII character are written raw.
fn write_str(out: &mut Vec<u8>, s: &str) {
    out.push(b'"');
    for ch in s.chars() {
        match ch {
            '"' => out.extend_from_slice(b"\\\""),
            '\\' => out.extend_from_slice(b"\\\\"),
            '\n' => out.extend_from_slice(b"\\n"),
            '\r' => out.extend_from_slice(b"\\r"),
            '\t' => out.extend_from_slice(b"\\t"),
            '\u{08}' => out.extend_from_slice(b"\\b"),
            '\u{0c}' => out.extend_from_slice(b"\\f"),
            c if (c as u32) < 0x20 => {
                out.extend_from_slice(format!("\\u{:04x}", c as u32).as_bytes())
            }
            c => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    out.push(b'"');
}
