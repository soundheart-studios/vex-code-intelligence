//! Language detection: a pure port of Cortex's `chunker.language_of`. The
//! caller supplies the file's head bytes (the first [`HEAD_BYTES`]) for the
//! two cases that look inside a file: `.h` headers and extensionless files.

use crate::cpp_lex::header_code;
use crate::text::{decode_replace, path_name, path_suffix, py_strip};

/// How much of a file `language_of` may need (Cortex reads 64 KiB of a `.h`
/// header and at most 256 bytes of a shebang line).
pub const HEAD_BYTES: usize = 64 * 1024;

/// Extension -> language, entry for entry Cortex's `_EXT_LANG`.
pub const EXT_LANG: &[(&str, &str)] = &[
    (".py", "python"),
    (".js", "javascript"),
    (".jsx", "javascript"),
    (".mjs", "javascript"),
    (".ts", "typescript"),
    (".mts", "typescript"),
    (".cts", "typescript"),
    (".tsx", "typescript"),
    (".go", "go"),
    (".rs", "rust"),
    (".java", "java"),
    (".kt", "kotlin"),
    (".kts", "kotlin"),
    (".rb", "ruby"),
    (".groovy", "groovy"),
    (".gradle", "groovy"),
    (".scala", "scala"),
    (".php", "php"),
    (".lua", "lua"),
    (".luau", "lua"),
    (".toc", "lua"),
    (".zig", "zig"),
    (".ex", "elixir"),
    (".exs", "elixir"),
    (".json", "json"),
    (".jl", "julia"),
    (".v", "verilog"),
    (".vh", "verilog"),
    (".sv", "verilog"),
    (".svh", "verilog"),
    (".f", "fortran"),
    (".for", "fortran"),
    (".f77", "fortran"),
    (".f90", "fortran"),
    (".f95", "fortran"),
    (".f03", "fortran"),
    (".f08", "fortran"),
    (".ps1", "powershell"),
    (".psm1", "powershell"),
    (".psd1", "powershell"),
    (".tf", "hcl"),
    (".tfvars", "hcl"),
    (".hcl", "hcl"),
    (".c", "c"),
    (".h", "c"),
    (".cpp", "cpp"),
    (".cc", "cpp"),
    (".cxx", "cpp"),
    (".hpp", "cpp"),
    (".hh", "cpp"),
    (".cs", "csharp"),
    (".cu", "cpp"),
    (".cuh", "cpp"),
    (".metal", "cpp"),
    (".m", "objc"),
    (".mm", "objc"),
    (".swift", "swift"),
    (".md", "markdown"),
    (".sql", "sql"),
    (".sh", "bash"),
    (".bash", "bash"),
];

/// Formats discovered and indexed through the line fallback even though no
/// language maps to them (Cortex `DISCOVERABLE_EXTENSIONS`).
pub const LINE_ONLY_EXTENSIONS: &[&str] = &[".yaml", ".yml", ".toml"];

const SHEBANG_LANG: &[(&str, &str)] = &[
    ("python", "python"),
    ("python2", "python"),
    ("python3", "python"),
    ("node", "javascript"),
    ("nodejs", "javascript"),
    ("ruby", "ruby"),
    ("lua", "lua"),
    ("php", "php"),
    ("bash", "bash"),
    ("sh", "bash"),
    ("dash", "bash"),
    ("zsh", "bash"),
    ("ksh", "bash"),
];

const OBJC_HEADER_MARKERS: &[&[u8]] = &[
    b"@interface",
    b"@protocol",
    b"@implementation",
    b"@import",
    b"#import",
];
const CPP_HEADER_MARKERS: &[&[u8]] =
    &[b"class ", b"namespace ", b"template<", b"template <", b"::"];

fn lookup(table: &[(&str, &'static str)], key: &str) -> Option<&'static str> {
    table.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Whether the walker may discover a file with this (lowercased) suffix.
pub fn discoverable(suffix_lower: &str) -> bool {
    lookup(EXT_LANG, suffix_lower).is_some() || LINE_ONLY_EXTENSIONS.contains(&suffix_lower)
}

/// The lowercased Python suffix of a path.
pub fn suffix_lower(path: &str) -> String {
    path_suffix(path).to_lowercase()
}

/// Cortex `language_of`. `head` is the file's first [`HEAD_BYTES`], or
/// `None` when the file could not be read; it is only consulted for `.h`
/// files and files without a suffix. Returns "" for no language.
pub fn language_of(path: &str, head: Option<&[u8]>) -> &'static str {
    let suffix = suffix_lower(path);
    if suffix == ".h" {
        let Some(head) = head else { return "c" };
        let head = &head[..head.len().min(HEAD_BYTES)];
        let code = header_code(head);
        if OBJC_HEADER_MARKERS.iter().any(|m| contains(&code, m)) {
            return "objc";
        }
        if CPP_HEADER_MARKERS.iter().any(|m| contains(&code, m)) {
            return "cpp";
        }
        return "c";
    }
    let routed = lookup(EXT_LANG, &suffix).unwrap_or("");
    if !routed.is_empty() || !suffix.is_empty() {
        return routed;
    }
    head.map_or("", shebang_language)
}

/// The shebang branch: `readline(256)`, then `shlex.split` of the command.
fn shebang_language(head: &[u8]) -> &'static str {
    let limit = head.len().min(256);
    let line_end = head[..limit]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(limit, |p| p + 1);
    let first_line = decode_replace(&head[..line_end]);
    let Some(rest) = first_line.strip_prefix("#!") else {
        return "";
    };
    let Some(command) = shlex::split(py_strip(rest)) else {
        return "";
    };
    let Some(first) = command.first() else {
        return "";
    };
    let mut interpreter = path_name(first).to_string();
    if interpreter == "env" {
        match env_interpreter(command[1..].to_vec()) {
            Some(i) => interpreter = i,
            None => return "",
        }
    }
    lookup(SHEBANG_LANG, &interpreter).unwrap_or("")
}

/// The program `env [options] [NAME=value...] program` would run, following
/// Cortex's option handling exactly; None where Cortex declines to guess.
fn env_interpreter(mut args: Vec<String>) -> Option<String> {
    while !args.is_empty() {
        let mut option = args.remove(0);
        if option == "-S" || option == "--split-string" {
            let first = args.first()?.clone();
            let mut split = shlex::split(&first)?;
            split.extend(args.drain(1..));
            args = split;
            continue;
        }
        if matches!(option.as_str(), "-u" | "--unset" | "-C" | "--chdir") {
            if args.is_empty() {
                return None;
            }
            args.remove(0);
            continue;
        }
        if option == "-i"
            || option == "--ignore-environment"
            || option.starts_with("--unset=")
            || option.starts_with("--chdir=")
        {
            continue;
        }
        if option
            .split_once('=')
            .is_some_and(|(name, _)| is_identifier(name))
        {
            continue;
        }
        if option == "--" {
            if args.is_empty() {
                return None;
            }
            option = args.remove(0);
        }
        if option.starts_with('-') {
            return None; // an unknown env option: do not guess an interpreter
        }
        return Some(path_name(&option).to_string());
    }
    None
}

/// Python `str.isidentifier()` (XID_Start/XID_Continue, approximated with
/// Rust's alphabetic/alphanumeric classes plus `_`).
fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_alphabetic() => chars.all(|c| c == '_' || c.is_alphanumeric()),
        _ => false,
    }
}
