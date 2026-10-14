//! Python text semantics the port must reproduce exactly.

/// Python's `str.splitlines()` (without `keepends`): splits on `\n`, `\r`,
/// `\r\n`, `\x0b`, `\x0c`, `\x1c`, `\x1d`, `\x1e`, `\x85`, U+2028 and U+2029.
/// A trailing separator does not produce a trailing empty line.
pub fn splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let is_break = matches!(
            c,
            '\n' | '\r'
                | '\u{0b}'
                | '\u{0c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if !is_break {
            continue;
        }
        out.push(&s[start..i]);
        let mut next = i + c.len_utf8();
        if c == '\r' && chars.peek().map(|&(_, n)| n) == Some('\n') {
            chars.next();
            next += 1;
        }
        start = next;
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// Python's `bytes.decode("utf-8", errors="replace")`. Both follow Unicode's
/// "maximal subpart" substitution, one U+FFFD per invalid subsequence.
pub fn decode_replace(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Python's `str.strip()` with no argument: Unicode whitespace both ends.
pub fn py_strip(s: &str) -> &str {
    s.trim_matches(is_py_whitespace)
}

/// `str.isspace()` for one character, as Python defines whitespace
/// (Unicode White_Space plus the C0 separators \x1c-\x1f).
pub fn is_py_whitespace(c: char) -> bool {
    c.is_whitespace() || matches!(c, '\u{1c}'..='\u{1f}')
}

/// Python 3.12 `PurePosixPath.name`: the final component after dropping
/// empty and `.` components (`PurePath("a/.").name == "a"`,
/// `PurePath(".").name == ""`).
pub fn path_name(path: &str) -> &str {
    path.split('/')
        .rfind(|p| !p.is_empty() && *p != ".")
        .unwrap_or("")
}

/// Python 3.12 `PurePath.suffix`: the final component's last `.`-suffix,
/// empty for names starting with their only dot or ending in a dot.
pub fn path_suffix(path: &str) -> &str {
    let name = path_name(path);
    match name.rfind('.') {
        Some(i) if i > 0 && i < name.len() - 1 => &name[i..],
        _ => "",
    }
}
