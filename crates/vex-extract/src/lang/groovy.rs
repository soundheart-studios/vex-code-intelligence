//! Groovy: Cortex's `languages/groovy.py`. A sound tree goes through the
//! shared engine (`_CONFIG["groovy"]`, Java-style imports). tree-sitter-
//! groovy 0.1.2 breaks on common code (a field initializer, or a bare
//! identifier before an annotated method, loses every method), so a broken
//! tree, and every `.gradle` script, is also read lexically (ADR 18):
//! comments and string bodies are blanked, declaration headers matched at
//! line starts, and each body's end found by brace matching. The lexical
//! result wins only when it finds more declarations.
//!
//! Cortex works in character offsets of the decoded text. Here the blanked
//! text keeps the source's byte length (a multi-byte character blanks to as
//! many spaces), so byte offsets index the source and the blanked text
//! alike and every comparison orders as Python's does.

use crate::ast::Config;
use crate::lang::adapter::{chunk, lines_of};
use crate::text::{decode_replace, is_py_whitespace, path_suffix, py_strip};
use indexmap::IndexMap;
use regex::Regex;
use std::sync::LazyLock;
use vex_core::{Chunk, Parsed, Symbol};

pub const CONFIG: Config = Config {
    defs: &[
        ("class_declaration", "class"),
        ("interface_declaration", "interface"),
        ("method_declaration", "method"),
        ("constructor_declaration", "method"),
    ],
    calls: &["method_invocation"],
    class_nodes: &["class_declaration", "interface_declaration"],
    imports: &[],
    qualified: true,
};

const MAX_CHUNK_LINES: usize = 120;

const MODIFIERS: &str = r"(?:(?:public|private|protected|static|final|abstract|synchronized|native|transient|default|def)\s+)";
const ANNOTATIONS: &str = r"(?:@[\w.]+(?:\([^()]*\))?\s+)";

/// `class|interface|enum|trait Name` at a line start.
pub static TYPE_DECL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?m)^[ \t]*{ANNOTATIONS}*{MODIFIERS}*(class|interface|enum|trait)\s+([A-Za-z_$][\w$]*)"
    ))
    .expect("valid regex")
});

/// `Type name(`, `def name(` or modifiers then `name(`; a bare `name(` is a
/// call (Gradle DSL), so something must precede the name.
pub static METHOD_HEAD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?m)^[ \t]*{ANNOTATIONS}*(?:{MODIFIERS}+(?:<[^>\n]+>\s+)?(?:[\w.$]+(?:<[^()\n]*>)?(?:\[\])*\s+)?|(?:<[^>\n]+>\s+)?[\w.$]+(?:<[^()\n]*>)?(?:\[\])*\s+)([A-Za-z_$][\w$]*)\s*\("
    ))
    .expect("valid regex")
});

/// A constructor: kept only when its name is the enclosing class's.
pub static CTOR_HEAD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?m)^[ \t]*{ANNOTATIONS}*{MODIFIERS}*([A-Z][\w$]*)\s*\("
    ))
    .expect("valid regex")
});

/// Gradle `task name(type: X) { ... }` and `task name { ... }`.
pub static GRADLE_TASK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^[ \t]*task\s+([A-Za-z_$][\w$]*)\s*(?:\([^()]*\))?\s*\{").expect("valid regex")
});

/// Spock feature methods named by a string: `def "reads the data"() {`.
pub static FEATURE_HEAD: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    fancy_regex::Regex::new(&format!(
        r#"(?m)^[ \t]*{ANNOTATIONS}*{MODIFIERS}*def\s+(['"])([^\n]*?)\1\s*\("#
    ))
    .expect("valid regex")
});

/// A call: the last segment of a dotted name before `(`.
pub static CALL: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    fancy_regex::Regex::new(r"(?<![\w$.])(?:[\w$]+\.)*([A-Za-z_$][\w$]*)\s*\(")
        .expect("valid regex")
});

static BODY_OPEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A\s*(?:throws\s+[\w.$<>,\s]+?)?\s*\{").expect("valid regex"));

fn not_a_type(word: &str) -> bool {
    matches!(
        word,
        "task"
            | "return"
            | "new"
            | "throw"
            | "else"
            | "assert"
            | "case"
            | "in"
            | "instanceof"
            | "as"
            | "if"
            | "for"
            | "while"
            | "switch"
            | "catch"
            | "try"
            | "do"
            | "import"
            | "package"
    )
}

fn not_a_name(word: &str) -> bool {
    matches!(
        word,
        "if" | "for"
            | "while"
            | "switch"
            | "catch"
            | "synchronized"
            | "return"
            | "new"
            | "super"
            | "this"
    )
}

fn is_keyword(word: &str) -> bool {
    not_a_name(word)
        || matches!(
            word,
            "else" | "try" | "throw" | "assert" | "def" | "in" | "as"
        )
}

/// Cortex `blank_non_code`: comment and string bodies become spaces (every
/// newline kept), including GString `${...}` interpolation, whose code may
/// itself hold strings. Slashy strings are not told apart from division.
pub fn blank_non_code(source: &str) -> String {
    let src = source.as_bytes();
    let mut out = src.to_vec();
    let n = src.len();
    let blank = |out: &mut Vec<u8>, a: usize, b: usize| {
        for byte in &mut out[a.min(n)..b.min(n)] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    };
    // One character's length in bytes at `i` (the escape skips a character).
    let char_len = |i: usize| -> usize { source[i..].chars().next().map_or(1, char::len_utf8) };

    fn scan_code(
        src: &[u8],
        out: &mut Vec<u8>,
        mut i: usize,
        closing: bool,
        blank: &dyn Fn(&mut Vec<u8>, usize, usize),
        char_len: &dyn Fn(usize) -> usize,
    ) -> usize {
        let n = src.len();
        let mut depth = 0usize;
        while i < n {
            let c = src[i];
            if src[i..].starts_with(b"//") {
                let j = src[i..]
                    .iter()
                    .position(|&b| b == b'\n')
                    .map_or(n, |p| i + p);
                blank(out, i, j);
                i = j;
            } else if src[i..].starts_with(b"/*") {
                let j = src[i + 2..]
                    .windows(2)
                    .position(|w| w == b"*/")
                    .map_or(n, |p| i + 2 + p + 2);
                blank(out, i, j);
                i = j;
            } else if c == b'\'' || c == b'"' {
                let triple = [c, c, c];
                let quote: &[u8] = if src[i..].starts_with(&triple) {
                    &src[i..i + 3]
                } else {
                    &src[i..i + 1]
                };
                let quote = quote.to_vec();
                i = scan_string(src, out, i + quote.len(), &quote, blank, char_len);
            } else if closing && c == b'{' {
                depth += 1;
                i += 1;
            } else if closing && c == b'}' {
                if depth == 0 {
                    return i + 1;
                }
                depth -= 1;
                i += 1;
            } else {
                i += 1;
            }
        }
        i
    }

    fn scan_string(
        src: &[u8],
        out: &mut Vec<u8>,
        mut i: usize,
        quote: &[u8],
        blank: &dyn Fn(&mut Vec<u8>, usize, usize),
        char_len: &dyn Fn(usize) -> usize,
    ) -> usize {
        let n = src.len();
        let mut start = i;
        let interpolating = quote[0] == b'"';
        while i < n {
            if src[i] == b'\\' {
                // Python's `i += 2` skips the backslash and one character.
                i += 1;
                if i < n {
                    i += char_len(i);
                }
                continue;
            }
            if src[i..].starts_with(quote) {
                blank(out, start, i);
                return i + quote.len();
            }
            if quote.len() == 1 && src[i] == b'\n' {
                blank(out, start, i); // an unterminated single-line string
                return i;
            }
            if interpolating && src[i..].starts_with(b"${") {
                blank(out, start, i + 2);
                let j = scan_code(src, out, i + 2, true, blank, char_len);
                blank(out, i + 2, j);
                start = j;
                i = j;
                continue;
            }
            i += 1;
        }
        blank(out, start, n);
        n
    }

    scan_code(src, &mut out, 0, false, &blank, &char_len);
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// Just past the bracket matching `code[i]`, or None.
fn matching(code: &[u8], i: usize, open: u8, close: u8) -> Option<usize> {
    let mut depth = 0i64;
    for (k, &b) in code.iter().enumerate().skip(i) {
        if b == open {
            depth += 1;
        } else if b == close {
            depth -= 1;
            if depth == 0 {
                return Some(k + 1);
            }
        }
    }
    None
}

/// The `{` opening a method body after its parameter list.
fn body_open(code: &str, after_params: usize) -> Option<usize> {
    BODY_OPEN
        .find(&code[after_params..])
        .map(|m| after_params + m.end() - 1)
}

/// The offset of a match's first non-whitespace character.
fn head_start(start: usize, head: &str) -> usize {
    start + head.len() - head.trim_start_matches(is_py_whitespace).len()
}

/// (kind, name, start, body_open, end) for types and methods, in source
/// order; `source` names string-named feature methods.
pub fn scan_declarations(code: &str, source: &str) -> Vec<(String, String, usize, usize, usize)> {
    let bytes = code.as_bytes();
    let mut found = Vec::new();
    for m in TYPE_DECL.captures_iter(code) {
        let whole = m.get(0).expect("match");
        let Some(brace) = code[whole.end()..].find('{').map(|p| whole.end() + p) else {
            continue;
        };
        if code[whole.end()..brace].contains([';', '=']) {
            continue;
        }
        if let Some(end) = matching(bytes, brace, b'{', b'}') {
            let kind = if &m[1] == "interface" {
                "interface"
            } else {
                "class"
            };
            found.push((
                kind.to_string(),
                m[2].to_string(),
                head_start(whole.start(), whole.as_str()),
                brace,
                end,
            ));
        }
    }
    let heads = METHOD_HEAD
        .captures_iter(code)
        .map(|m| ("method", m))
        .chain(CTOR_HEAD.captures_iter(code).map(|m| ("constructor", m)));
    for (kind, m) in heads {
        let whole = m.get(0).expect("match");
        let name = &m[1];
        let head = whole.as_str();
        let type_word = head
            .split(is_py_whitespace)
            .find(|p| !p.is_empty())
            .unwrap_or("");
        if kind == "method" && (not_a_name(name) || not_a_type(type_word)) {
            continue;
        }
        let paren = whole.end() - 1;
        let Some(close) = matching(bytes, paren, b'(', b')') else {
            continue;
        };
        let Some(brace) = body_open(code, close) else {
            continue;
        };
        if let Some(end) = matching(bytes, brace, b'{', b'}') {
            found.push((
                kind.to_string(),
                name.to_string(),
                head_start(whole.start(), head),
                brace,
                end,
            ));
        }
    }
    for m in FEATURE_HEAD.captures_iter(code).flatten() {
        let whole = m.get(0).expect("match");
        let g2 = m.get(2).expect("group 2");
        let name = py_strip(&source[g2.start()..g2.end()]).to_string();
        let close = matching(bytes, whole.end() - 1, b'(', b')');
        let brace = close.and_then(|c| body_open(code, c));
        let end = brace.and_then(|b| matching(bytes, b, b'{', b'}'));
        if let (false, Some(brace), Some(end)) = (name.is_empty(), brace, end) {
            found.push((
                "method".to_string(),
                name,
                head_start(whole.start(), whole.as_str()),
                brace,
                end,
            ));
        }
    }
    for m in GRADLE_TASK.captures_iter(code) {
        let whole = m.get(0).expect("match");
        let brace = whole.end() - 1;
        if let Some(end) = matching(bytes, brace, b'{', b'}') {
            found.push((
                "task".to_string(),
                m[1].to_string(),
                head_start(whole.start(), whole.as_str()),
                brace,
                end,
            ));
        }
    }
    found.sort_by_key(|d| d.2);
    found
}

/// Cortex `parse_groovy`. None when the grammar is not compiled in.
pub fn parse_groovy(rel: &str, source_bytes: &[u8]) -> Option<Parsed> {
    let tree = crate::engine::parse_tree("groovy", source_bytes)?;
    let ast = crate::ast::parse_file(rel, "groovy", source_bytes, &tree, &CONFIG);
    // A build script's tasks are never AST declarations, so .gradle files
    // are always scanned; other files only when the grammar failed.
    let gradle = path_suffix(rel).to_lowercase() == ".gradle";
    if !(gradle || tree.root_node().has_error()) {
        return Some(ast);
    }
    let source = decode_replace(source_bytes);
    let code = blank_non_code(&source);
    let lines = lines_of(source_bytes);
    let qual_prefix = rel.replace('/', ".");
    let mut line_starts = vec![0usize];
    line_starts.extend(
        source
            .bytes()
            .enumerate()
            .filter(|(_, b)| *b == b'\n')
            .map(|(k, _)| k + 1),
    );
    // Zero-based line of a byte offset.
    let line_of = |offset: usize| -> usize { line_starts.partition_point(|&s| s <= offset) - 1 };

    let mut chunks: Vec<Chunk> = Vec::new();
    let mut symbols: Vec<Symbol> = Vec::new();
    let mut seen: std::collections::HashSet<(String, usize)> = std::collections::HashSet::new();
    let mut containers: Vec<(String, usize, usize)> = Vec::new();
    let mut methods: Vec<(String, usize, usize)> = Vec::new();
    let add_chunk =
        |chunks: &mut Vec<Chunk>, start: usize, end: usize, kind: &str, names: &[String]| {
            if lines.is_empty() {
                return;
            }
            let end = end.min(start + MAX_CHUNK_LINES).min(lines.len() - 1);
            if start > end {
                return;
            }
            if let Some(c) = chunk(
                rel,
                &lines,
                start,
                end,
                kind,
                "groovy",
                Some(&names.join(",")),
            ) {
                chunks.push(c);
            }
        };

    for (kind, name, start, body, end) in scan_declarations(&code, &source) {
        if methods.iter().any(|(_, b, e)| *b < start && start < *e) {
            continue; // closures and locals in a method body are not members
        }
        let mut owner = String::new();
        for (qn, b, e) in &containers {
            if *b < start && start < *e {
                owner = qn.clone(); // innermost wins: containers are in order
            }
        }
        let kind = if kind == "constructor" {
            if owner.is_empty() || owner.rsplit('.').next() != Some(name.as_str()) {
                continue;
            }
            "method".to_string() // as the AST path records constructors
        } else {
            kind
        };
        let local = if owner.is_empty() {
            name.clone()
        } else {
            format!("{owner}.{name}")
        };
        let full = format!("{qual_prefix}.{local}");
        if !seen.insert((full.clone(), start)) {
            continue; // every definition site, not the first (ADR 19)
        }
        let (first, last) = (line_of(start), line_of(end - 1));
        symbols.push(Symbol {
            qualified_name: full.clone(),
            name: name.clone(),
            kind: kind.clone(),
            file_path: rel.to_string(),
            start_line: first + 1,
            end_line: last + 1,
            declaration: false,
        });
        if kind == "method" || kind == "task" {
            methods.push((full, body, end));
            add_chunk(
                &mut chunks,
                first,
                last,
                &kind,
                std::slice::from_ref(&local),
            );
        } else {
            containers.push((local, body, end));
        }
    }

    for (owner, body, end) in &containers {
        let start = line_of(*body);
        let owner_qn = format!("{qual_prefix}.{owner}");
        let header_start = symbols
            .iter()
            .find(|s| s.qualified_name == owner_qn)
            .map(|s| s.start_line - 1)
            .unwrap_or(0);
        let member_prefix = format!("{owner_qn}.");
        let first_member = symbols
            .iter()
            .filter(|s| s.qualified_name.starts_with(&member_prefix))
            .map(|s| s.start_line as i64 - 1)
            .min()
            .unwrap_or(line_of(end - 1) as i64 + 1);
        let header_end = (start as i64).max(first_member - 1) as usize;
        let short = owner.rsplit('.').next().unwrap_or(owner).to_string();
        add_chunk(&mut chunks, header_start, header_end, "class", &[short]);
    }
    let first_def = symbols
        .iter()
        .map(|s| s.start_line - 1)
        .min()
        .unwrap_or(lines.len());
    if first_def > 0 {
        add_chunk(
            &mut chunks,
            0,
            (first_def - 1).min(MAX_CHUNK_LINES),
            "module",
            &[],
        );
    }

    let mut refs: IndexMap<String, u64> = IndexMap::new();
    let mut calls: IndexMap<(String, String), u64> = IndexMap::new();
    for m in CALL.captures_iter(&code).flatten() {
        let g1 = m.get(1).expect("group 1");
        let name = g1.as_str();
        if is_keyword(name) {
            continue;
        }
        let offset = g1.start();
        let mut caller: Option<(&str, usize)> = None;
        for (qn, b, e) in &methods {
            if *b < offset && offset < *e && caller.is_none_or(|(_, cb)| *b > cb) {
                caller = Some((qn, *b));
            }
        }
        if caller.is_none() {
            let line = line_of(offset);
            if symbols
                .iter()
                .any(|s| s.start_line - 1 == line && s.name == name)
            {
                continue; // the declaration header itself
            }
        }
        let caller = caller.map_or_else(|| format!("file:{rel}"), |(qn, _)| qn.to_string());
        *calls.entry((caller, name.to_string())).or_insert(0) += 1;
        *refs.entry(name.to_string()).or_insert(0) += 1;
    }
    chunks.sort_by_key(|c| c.start_line);
    // Most broken trees still hold every declaration; the scanner replaces
    // the AST only where it finds more.
    if symbols.len() > ast.symbols.len() {
        Some(Parsed {
            chunks,
            symbols,
            imports: ast.imports,
            refs,
            bindings: ast.bindings,
            calls,
            members: Vec::new(),
        })
    } else {
        Some(ast)
    }
}
