//! Tree-sitter extraction: file bytes in, Parsed out. Pure: no I/O beyond the
//! bytes it is given.

pub mod ast;
pub mod cpp_lex;
pub mod engine;
pub mod fallback;
pub mod grammars;
pub mod lang;
pub mod language;
pub mod residual;
pub mod text;
pub mod typed;

pub use grammars::{compiled_grammars, language};

/// The first differing JSON path between two canonical outputs (for test
/// and parity diagnostics).
pub fn parity_diff(got: &str, want: &str) -> String {
    fn walk(path: &str, a: &serde_json::Value, b: &serde_json::Value) -> Option<String> {
        use serde_json::Value;
        match (a, b) {
            (Value::Object(x), Value::Object(y)) => {
                let mut keys: Vec<&std::string::String> = x.keys().chain(y.keys()).collect();
                keys.sort();
                keys.dedup();
                keys.into_iter().find_map(|k| match (x.get(k), y.get(k)) {
                    (Some(l), Some(r)) => walk(&format!("{path}.{k}"), l, r),
                    (l, r) => Some(format!("{path}.{k}: {l:?} vs {r:?}")),
                })
            }
            (Value::Array(x), Value::Array(y)) => x
                .iter()
                .zip(y)
                .enumerate()
                .find_map(|(i, (l, r))| walk(&format!("{path}[{i}]"), l, r))
                .or_else(|| {
                    (x.len() != y.len())
                        .then(|| format!("{path}: length {} vs {}", x.len(), y.len()))
                }),
            _ if a == b => None,
            _ => Some(format!("{path}: {a} vs {b}")),
        }
    }
    match (serde_json::from_str(got), serde_json::from_str(want)) {
        (Ok(g), Ok(w)) => walk("$", &g, &w).unwrap_or_else(|| "bytes differ".into()),
        _ => "unparsable".into(),
    }
}
