//! Python-compatible JSON and repr, for payloads that must be byte-identical
//! to Cortex's wire output.
//!
//! Cortex answers with `json.dumps(payload)`: insertion-ordered keys, `", "`
//! and `": "` separators, `\uXXXX` escapes for everything outside ASCII, and
//! floats written as `repr(float)`. Byte budgets are measured on that text
//! (`len(json.dumps(...))`), so matching values is not enough; the bytes
//! decide which results fit. `Json` keeps order; `dumps` writes it the way
//! Python does; `repr_str` is `repr(str)`, which recovery hints embed.

use crate::pyprintable::PRINTABLE;
use std::fmt::Write;

/// A JSON value that keeps object keys in insertion order.
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    /// An object from `(key, value)` pairs, in order.
    pub fn obj<K: Into<String>>(pairs: Vec<(K, Json)>) -> Json {
        Json::Obj(pairs.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }

    /// The value under `key`, for objects.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Set `key` (replacing in place, or appending), as `dict[key] = v`.
    pub fn set(&mut self, key: &str, value: Json) {
        if let Json::Obj(pairs) = self {
            match pairs.iter_mut().find(|(k, _)| k == key) {
                Some(slot) => slot.1 = value,
                None => pairs.push((key.to_string(), value)),
            }
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Python truthiness, for `if payload.get(k):` style checks.
    pub fn truthy(&self) -> bool {
        match self {
            Json::Null => false,
            Json::Bool(b) => *b,
            Json::Int(i) => *i != 0,
            Json::Float(f) => *f != 0.0,
            Json::Str(s) => !s.is_empty(),
            Json::Arr(a) => !a.is_empty(),
            Json::Obj(o) => !o.is_empty(),
        }
    }

    /// Convert to serde_json (key order is lost unless serde_json keeps it).
    pub fn to_value(&self) -> serde_json::Value {
        use serde_json::Value;
        match self {
            Json::Null => Value::Null,
            Json::Bool(b) => Value::Bool(*b),
            Json::Int(i) => Value::from(*i),
            Json::Float(f) => serde_json::Number::from_f64(*f).map_or(Value::Null, Value::Number),
            Json::Str(s) => Value::String(s.clone()),
            Json::Arr(a) => Value::Array(a.iter().map(Json::to_value).collect()),
            Json::Obj(o) => {
                Value::Object(o.iter().map(|(k, v)| (k.clone(), v.to_value())).collect())
            }
        }
    }
}

impl From<&str> for Json {
    fn from(s: &str) -> Json {
        Json::Str(s.to_string())
    }
}
impl From<String> for Json {
    fn from(s: String) -> Json {
        Json::Str(s)
    }
}
impl From<&String> for Json {
    fn from(s: &String) -> Json {
        Json::Str(s.clone())
    }
}
impl From<bool> for Json {
    fn from(b: bool) -> Json {
        Json::Bool(b)
    }
}
impl From<i64> for Json {
    fn from(i: i64) -> Json {
        Json::Int(i)
    }
}
impl From<usize> for Json {
    fn from(i: usize) -> Json {
        Json::Int(i as i64)
    }
}
impl From<f64> for Json {
    fn from(f: f64) -> Json {
        Json::Float(f)
    }
}
impl<T: Into<Json>> From<Option<T>> for Json {
    fn from(o: Option<T>) -> Json {
        o.map_or(Json::Null, Into::into)
    }
}
impl<T: Into<Json>> From<Vec<T>> for Json {
    fn from(v: Vec<T>) -> Json {
        Json::Arr(v.into_iter().map(Into::into).collect())
    }
}

/// How `json.dumps` was called.
#[derive(Debug, Clone, Copy)]
pub struct Style {
    pub ensure_ascii: bool,
    /// `separators=(",", ":")` instead of the default `(", ", ": ")`.
    pub compact: bool,
}

/// `json.dumps(v)`.
pub const DEFAULT: Style = Style {
    ensure_ascii: true,
    compact: false,
};
/// `json.dumps(v, ensure_ascii=False, separators=(",", ":"))`.
pub const COMPACT_RAW: Style = Style {
    ensure_ascii: false,
    compact: true,
};

/// `json.dumps(value, ...)` in the given style.
pub fn dumps(value: &Json, style: Style) -> String {
    let mut out = String::new();
    write_json(&mut out, value, style);
    out
}

fn write_json(out: &mut String, v: &Json, style: Style) {
    let (item_sep, key_sep) = if style.compact {
        (",", ":")
    } else {
        (", ", ": ")
    };
    match v {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Int(i) => {
            let _ = write!(out, "{i}");
        }
        Json::Float(f) => out.push_str(&json_float(*f)),
        Json::Str(s) => write_str(out, s, style.ensure_ascii),
        Json::Arr(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(item_sep);
                }
                write_json(out, item, style);
            }
            out.push(']');
        }
        Json::Obj(pairs) => {
            out.push('{');
            for (i, (k, item)) in pairs.iter().enumerate() {
                if i > 0 {
                    out.push_str(item_sep);
                }
                write_str(out, k, style.ensure_ascii);
                out.push_str(key_sep);
                write_json(out, item, style);
            }
            out.push('}');
        }
    }
}

/// `json.dumps` of a str: `"`, `\` and C0 controls escaped (short forms for
/// `\b \f \n \r \t`), and with `ensure_ascii` every non-ASCII character and
/// DEL as `\uxxxx`, astral characters as surrogate pairs.
fn write_str(out: &mut String, s: &str, ensure_ascii: bool) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c if ensure_ascii && (c as u32) >= 0x7f => {
                let mut units = [0u16; 2];
                for u in c.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{u:04x}");
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `json.dumps(float)`: `repr`, with Python's spellings of the non-finite.
pub fn json_float(f: f64) -> String {
    if f.is_nan() {
        "NaN".into()
    } else if f.is_infinite() {
        if f > 0.0 {
            "Infinity".into()
        } else {
            "-Infinity".into()
        }
    } else {
        float_repr(f)
    }
}

/// `repr(float)` for finite values: the shortest round-tripping digits,
/// fixed-point when the decimal exponent is in [-4, 16), else scientific
/// with a signed, at-least-two-digit exponent (`1e-05`, `1.5e+16`).
pub fn float_repr(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let sci = format!("{:e}", f.abs());
    let (mantissa, exp) = sci.split_once('e').expect("LowerExp has an exponent");
    let exp: i32 = exp.parse().expect("integer exponent");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let sign = if f.is_sign_negative() { "-" } else { "" };
    if !(-4..16).contains(&exp) {
        let (head, tail) = digits.split_at(1);
        let frac = if tail.is_empty() {
            String::new()
        } else {
            format!(".{tail}")
        };
        let esign = if exp < 0 { '-' } else { '+' };
        return format!("{sign}{head}{frac}e{esign}{:02}", exp.abs());
    }
    if exp >= 0 {
        let point = exp as usize + 1;
        let (int, frac) = if digits.len() > point {
            (digits[..point].to_string(), digits[point..].to_string())
        } else {
            (format!("{digits:0<point$}"), "0".to_string())
        };
        format!("{sign}{int}.{frac}")
    } else {
        let zeros = "0".repeat((-exp - 1) as usize);
        format!("{sign}0.{zeros}{digits}")
    }
}

/// Python `round(x, n)` for n in 0..=17: the exact binary value rounded half
/// to even, which `{:.n}` formatting also does.
pub fn py_round(x: f64, n: usize) -> f64 {
    format!("{x:.n$}").parse().unwrap_or(x)
}

/// CPython `str.isprintable()` for one character.
pub fn is_printable(c: char) -> bool {
    let cp = c as u32;
    if cp < 0x80 {
        return (0x20..0x7f).contains(&cp);
    }
    PRINTABLE
        .binary_search_by(|&(lo, hi)| {
            if hi < cp {
                std::cmp::Ordering::Less
            } else if lo > cp {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// `repr(str)`: single quotes unless the text has a `'` and no `"`; the
/// chosen quote and `\` escaped; `\t \n \r` short; other non-printables as
/// `\xhh`, `\uhhhh` or `\Uhhhhhhhh`.
pub fn repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        let cp = c as u32;
        match c {
            c if c == quote || c == '\\' => {
                out.push('\\');
                out.push(c);
            }
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if is_printable(c) => out.push(c),
            _ if cp <= 0xff => {
                let _ = write!(out, "\\x{cp:02x}");
            }
            _ if cp <= 0xffff => {
                let _ = write!(out, "\\u{cp:04x}");
            }
            _ => {
                let _ = write!(out, "\\U{cp:08x}");
            }
        }
    }
    out.push(quote);
    out
}

/// Python `str.split()` with no argument.
pub fn py_split(s: &str) -> impl Iterator<Item = &str> {
    s.split(vex_extract::text::is_py_whitespace)
        .filter(|p| !p.is_empty())
}

/// `s[:n]` in code points.
pub fn take_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}
