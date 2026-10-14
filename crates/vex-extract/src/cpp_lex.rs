//! A hand port of Cortex's C-family token regex (`languages/cpp.py`
//! `_TOKEN`), which no Rust regex crate can express: it runs over raw bytes
//! and matches raw strings with a back-reference (`(?P=delimiter)`).
//!
//! ```text
//! (?P<opaque> ^[ \t]*\#(?:\\\r?\n|[^\r\n])*          directive (MULTILINE ^)
//!           | //[^\n]*                                 line comment
//!           | /\*[\s\S]*?(?:\*/|\Z)                    block comment
//!           | (?:u8|u|U|L)?R"(?P<d>[^ ()\\\t\r\n]{0,16})\([\s\S]*?\)(?P=d)"
//!           | (?:u8|u|U|L)?"(?:\\[\s\S]|[^"\\])*(?:"|\Z)
//!           | (?:u8|u|U|L)?'(?:\\[\s\S]|[^'\\])*(?:'|\Z))
//! | (?P<name>[A-Za-z_][A-Za-z_0-9]*) | (?P<punct>[^\s])
//! ```
//!
//! Matching follows `re.finditer`: at each position the alternatives are
//! tried in order and the first that matches wins; where none matches, the
//! scan moves one byte on. `\s` is ASCII whitespace (bytes pattern).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A directive, comment or literal.
    Opaque,
    Name,
    Punct,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: Kind,
    pub start: usize,
    pub end: usize,
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

fn is_name_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_name_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

const PREFIXES: [&[u8]; 5] = [b"u8", b"u", b"U", b"L", b""];

/// Every token of `src`, in order.
pub fn tokens(src: &[u8]) -> Vec<Token> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < src.len() {
        if let Some(end) = opaque(src, i) {
            out.push(Token {
                kind: Kind::Opaque,
                start: i,
                end,
            });
            // A zero-width match cannot occur (every opaque form consumes at
            // least two bytes), so the scan always advances.
            i = end;
        } else if is_name_start(src[i]) {
            let mut j = i + 1;
            while j < src.len() && is_name_char(src[j]) {
                j += 1;
            }
            out.push(Token {
                kind: Kind::Name,
                start: i,
                end: j,
            });
            i = j;
        } else if !is_space(src[i]) {
            out.push(Token {
                kind: Kind::Punct,
                start: i,
                end: i + 1,
            });
            i += 1;
        } else {
            i += 1;
        }
    }
    out
}

/// The end of an opaque token starting at `i`, trying the forms in order.
fn opaque(s: &[u8], i: usize) -> Option<usize> {
    directive(s, i)
        .or_else(|| line_comment(s, i))
        .or_else(|| block_comment(s, i))
        .or_else(|| {
            PREFIXES.iter().find_map(|p| {
                s[i..]
                    .starts_with(p)
                    .then(|| raw_string(s, i + p.len()))
                    .flatten()
            })
        })
        .or_else(|| {
            PREFIXES.iter().find_map(|p| {
                s[i..]
                    .starts_with(p)
                    .then(|| quoted(s, i + p.len(), b'"'))
                    .flatten()
            })
        })
        .or_else(|| {
            PREFIXES.iter().find_map(|p| {
                s[i..]
                    .starts_with(p)
                    .then(|| quoted(s, i + p.len(), b'\''))
                    .flatten()
            })
        })
}

/// `^[ \t]*\#(?:\\\r?\n|[^\r\n])*` with MULTILINE: `^` is the start of input
/// or just after `\n` (not `\r`).
fn directive(s: &[u8], i: usize) -> Option<usize> {
    if i > 0 && s[i - 1] != b'\n' {
        return None;
    }
    let mut j = i;
    while j < s.len() && matches!(s[j], b' ' | b'\t') {
        j += 1;
    }
    if s.get(j) != Some(&b'#') {
        return None;
    }
    j += 1;
    loop {
        match s.get(j) {
            Some(b'\\') if s.get(j + 1) == Some(&b'\n') => j += 2,
            Some(b'\\') if s.get(j + 1) == Some(&b'\r') && s.get(j + 2) == Some(&b'\n') => j += 3,
            Some(b'\r') | Some(b'\n') | None => return Some(j),
            Some(_) => j += 1,
        }
    }
}

fn line_comment(s: &[u8], i: usize) -> Option<usize> {
    if !s[i..].starts_with(b"//") {
        return None;
    }
    Some(
        s[i..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(s.len(), |p| i + p),
    )
}

/// `/\*[\s\S]*?(?:\*/|\Z)`: the earliest `*/` after the opener, else the end.
fn block_comment(s: &[u8], i: usize) -> Option<usize> {
    if !s[i..].starts_with(b"/*") {
        return None;
    }
    let body = i + 2;
    Some(
        s[body..]
            .windows(2)
            .position(|w| w == b"*/")
            .map_or(s.len(), |p| body + p + 2),
    )
}

/// `R"(?P<d>[^ ()\\\t\r\n]{0,16})\([\s\S]*?\)(?P=d)"` from `p`. The delimiter
/// set excludes `(`, so backtracking into `{0,16}` can never find a shorter
/// delimiter followed by `(`: the full run must be ≤ 16 bytes. Unterminated
/// raw strings do not match at all (there is no `\Z` fallback).
fn raw_string(s: &[u8], p: usize) -> Option<usize> {
    if !s[p..].starts_with(b"R\"") {
        return None;
    }
    let d0 = p + 2;
    let mut d1 = d0;
    while d1 < s.len() && !matches!(s[d1], b' ' | b'(' | b')' | b'\\' | b'\t' | b'\r' | b'\n') {
        d1 += 1;
    }
    if d1 - d0 > 16 || s.get(d1) != Some(&b'(') {
        return None;
    }
    let delim = &s[d0..d1];
    let mut k = d1 + 1;
    while k < s.len() {
        if s[k] == b')'
            && s[k + 1..].starts_with(delim)
            && s.get(k + 1 + delim.len()) == Some(&b'"')
        {
            return Some(k + 2 + delim.len());
        }
        k += 1;
    }
    None
}

/// `q(?:\\[\s\S]|[^q\\])*(?:q|\Z)` from `p`. The loop stops at an unescaped
/// quote, at the end, or at a lone backslash ending the input; in that last
/// case no backtracking can reach a quote or the end, so it fails.
fn quoted(s: &[u8], p: usize, q: u8) -> Option<usize> {
    if s.get(p) != Some(&q) {
        return None;
    }
    let mut j = p + 1;
    loop {
        match s.get(j) {
            None => return Some(j),
            Some(&b) if b == q => return Some(j + 1),
            Some(b'\\') => {
                if j + 1 < s.len() {
                    j += 2;
                } else {
                    return None;
                }
            }
            Some(_) => j += 1,
        }
    }
}

/// Cortex `header_code`: blank every comment, literal and directive (except
/// an `#import`, itself Objective-C evidence) to spaces, keeping `\n`/`\r`
/// so offsets and lines are preserved.
pub fn header_code(src: &[u8]) -> Vec<u8> {
    let mut out = src.to_vec();
    for t in tokens(src) {
        if t.kind != Kind::Opaque || is_import_directive(&src[t.start..t.end]) {
            continue;
        }
        for b in &mut out[t.start..t.end] {
            if *b != b'\n' && *b != b'\r' {
                *b = b' ';
            }
        }
    }
    out
}

/// `re.match(rb'\s*#\s*import\b', token)`.
fn is_import_directive(tok: &[u8]) -> bool {
    let mut j = 0;
    while j < tok.len() && is_space(tok[j]) {
        j += 1;
    }
    if tok.get(j) != Some(&b'#') {
        return false;
    }
    j += 1;
    while j < tok.len() && is_space(tok[j]) {
        j += 1;
    }
    tok[j..].starts_with(b"import") && !tok.get(j + 6).is_some_and(|&b| is_name_char(b))
}
