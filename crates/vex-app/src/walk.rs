//! File discovery: a port of Cortex's `indexer/walker.py`.
//!
//! Layered ignores: hardcoded directories (Cortex's, plus more tool caches
//! and AI-agent folders), then `.gitignore`, `.vexignore`
//! and `.cortexignore` globs (Cortex's simple `fnmatch` semantics, not full
//! gitignore: `*` crosses `/`, negation is not supported). Symlinks are never
//! followed. Traversal order matches Cortex's (a stack over sorted entries).

use std::path::{Path, PathBuf};
use vex_extract::language::{HEAD_BYTES, discoverable, language_of, suffix_lower};
use vex_extract::text::{decode_replace, py_strip, splitlines};

/// Tool and runtime caches: vendored sources that crowd real results out.
pub const CACHE_DIRS: &[&str] = &[
    ".uv-cache",
    ".uv",
    ".pytest_cache",
    ".ruff_cache",
    ".hatch",
    ".nox",
    ".hypothesis",
    ".cache",
    ".eggs",
    ".zvec-grep",
    // Beyond Cortex: build and package-manager caches of other ecosystems.
    ".gradle",
    ".terraform",
    ".serverless",
    ".parcel-cache",
    ".turbo",
    ".nx",
    ".svelte-kit",
    ".angular",
    ".docusaurus",
    ".astro",
    ".output",
    ".expo",
    ".vite",
    ".webpack",
    ".wrangler",
    ".vercel",
    ".netlify",
    ".jekyll-cache",
    ".sass-cache",
    ".nyc_output",
    ".dart_tool",
    ".pnpm-store",
    ".yarn",
    ".npm",
    ".bundle",
    ".stack-work",
    ".build",
    ".ccls-cache",
    ".clangd",
    ".metals",
    ".bloop",
    ".bsp",
    ".cpcache",
    ".shadow-cljs",
    ".pixi",
    ".direnv",
    ".ipynb_checkpoints",
    ".history",
    ".vs",
];

/// Beyond Cortex: AI-agent configuration, skills, rules and chat history.
/// Not the project's code, and copies of tool docs that crowd real results
/// out. A project's own `.github`, `.husky`, `.storybook` and the like are
/// still indexed.
pub const AGENT_DIRS: &[&str] = &[
    ".claude",
    ".codex",
    ".agents",
    ".cursor",
    ".windsurf",
    ".continue",
    ".gemini",
    ".roo",
    ".kilocode",
    ".cline",
    ".clinerules",
    ".amazonq",
    ".junie",
    ".kiro",
    ".opencode",
    ".specstory",
    ".serena",
    ".planwright",
    ".vex",
];
/// Agent state kept as files or directories at the root (aider's tag
/// cache and chat history).
pub const AGENT_GLOBS: &[&str] = &[".aider*"];

/// Directories never indexed (Cortex `_HARDCODED_DIRS`, cache dirs included).
pub const HARDCODED_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "__pycache__",
    ".venv",
    "venv",
    "dist",
    "build",
    ".idea",
    ".vscode",
    "target",
    ".cortex",
    ".tox",
    ".mypy_cache",
    "others",
    "vendor",
    "third_party",
    ".next",
    ".nuxt",
    "coverage",
];
pub const HARDCODED_DIR_GLOBS: &[&str] = &["*.egg-info"];
pub const GENERATED_PATTERNS: &[&str] = &[
    "*.min.js",
    "*.min.css",
    "*_pb2.py",
    "*.pb.go",
    "*.generated.*",
    "package-lock.json",
    "*.lock",
];
pub const SECRET_PATTERNS: &[&str] = &[
    "*.pem",
    "*.key",
    ".env",
    ".env.*",
    "credentials*",
    "*secret*",
];
/// Files larger than this are skipped (codebase-memory's default).
pub const MAX_FILE_BYTES: u64 = 512 * 1024;
/// Ignore files read from the project root, in order.
pub const IGNORE_FILES: &[&str] = &[".gitignore", ".vexignore", ".cortexignore"];

#[derive(Debug, Default)]
pub struct Walk {
    /// Discovered files, as root-joined paths, in Cortex's order.
    pub files: Vec<PathBuf>,
    /// Relative paths of skipped tool/runtime cache directories.
    pub skipped_caches: Vec<String>,
}

/// A directory the walker never descends into, by its name alone.
pub fn never_indexed_dir(name: &str) -> bool {
    HARDCODED_DIRS.contains(&name)
        || CACHE_DIRS.contains(&name)
        || AGENT_DIRS.contains(&name)
        || HARDCODED_DIR_GLOBS.iter().any(|g| fnmatch(name, g))
        || AGENT_GLOBS.iter().any(|g| fnmatch(name, g))
}

fn is_cache_dir(name: &str) -> bool {
    CACHE_DIRS.contains(&name) || HARDCODED_DIR_GLOBS.iter().any(|g| fnmatch(name, g))
}

/// Ignore globs from the root's ignore files: stripped, trailing `/` removed,
/// blank lines and `#` comments dropped.
pub fn load_ignore_globs(root: &Path) -> Vec<String> {
    let mut globs = Vec::new();
    for name in IGNORE_FILES {
        let f = root.join(name);
        if !f.is_file() {
            continue;
        }
        let Ok(bytes) = std::fs::read(&f) else {
            continue;
        };
        let text = decode_replace(&bytes);
        for line in splitlines(&text) {
            let line = py_strip(line).trim_end_matches('/');
            if !line.is_empty() && !line.starts_with('#') {
                globs.push(line.to_string());
            }
        }
    }
    globs
}

fn read_head(path: &Path) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(HEAD_BYTES as u64)
        .read_to_end(&mut buf)
        .ok()?;
    Some(buf)
}

/// Discover indexable files under `root`.
pub fn walk(root: &Path) -> Walk {
    let globs = load_ignore_globs(root);
    let ignored = |rel: &str, name: &str| globs.iter().any(|g| fnmatch(rel, g) || fnmatch(name, g));
    let mut out = Walk::default();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<PathBuf> = read.flatten().map(|e| e.path()).collect();
        entries.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
        for e in entries {
            let Ok(meta) = std::fs::symlink_metadata(&e) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            let name = e
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let rel = e
                .strip_prefix(root)
                .unwrap_or(&e)
                .to_string_lossy()
                .into_owned();
            if meta.is_dir() {
                // Cortex's _HARDCODED_DIRS is the union with _CACHE_DIRS.
                if never_indexed_dir(&name) {
                    if is_cache_dir(&name) {
                        out.skipped_caches.push(rel);
                    }
                    continue;
                }
                if ignored(&rel, &name) {
                    continue;
                }
                stack.push(e);
            } else if meta.is_file() {
                let suffix = suffix_lower(&name);
                let supported = discoverable(&suffix);
                if !supported && !suffix.is_empty() {
                    continue;
                }
                if !supported && language_of(&name, read_head(&e).as_deref()).is_empty() {
                    continue;
                }
                if SECRET_PATTERNS.iter().any(|g| fnmatch(&name, g))
                    || GENERATED_PATTERNS.iter().any(|g| fnmatch(&name, g))
                    || AGENT_GLOBS.iter().any(|g| fnmatch(&name, g))
                {
                    continue;
                }
                if ignored(&rel, &name) {
                    continue;
                }
                if meta.len() > MAX_FILE_BYTES {
                    continue;
                }
                out.files.push(e);
            }
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Star,
    Any,
    Lit(char),
    /// (negated, literal chars, inclusive ranges); `never` for an empty set.
    Set {
        negated: bool,
        chars: Vec<char>,
        ranges: Vec<(char, char)>,
        never: bool,
        any: bool,
    },
}

impl Tok {
    fn matches(&self, c: char) -> bool {
        match self {
            Tok::Star => unreachable!(),
            Tok::Any => true,
            Tok::Lit(l) => *l == c,
            Tok::Set {
                negated,
                chars,
                ranges,
                never,
                any,
            } => {
                if *never {
                    return false;
                }
                if *any {
                    return true;
                }
                let hit = chars.contains(&c) || ranges.iter().any(|(lo, hi)| *lo <= c && c <= *hi);
                hit != *negated
            }
        }
    }
}

/// Python `fnmatch.translate` (3.12), as tokens rather than a regex.
fn translate(pat: &str) -> Vec<Tok> {
    let p: Vec<char> = pat.chars().collect();
    let n = p.len();
    let mut out: Vec<Tok> = Vec::new();
    let mut i = 0;
    while i < n {
        let c = p[i];
        i += 1;
        match c {
            '*' => {
                if out.last() != Some(&Tok::Star) {
                    out.push(Tok::Star);
                }
            }
            '?' => out.push(Tok::Any),
            '[' => {
                let mut j = i;
                if j < n && p[j] == '!' {
                    j += 1;
                }
                if j < n && p[j] == ']' {
                    j += 1;
                }
                while j < n && p[j] != ']' {
                    j += 1;
                }
                if j >= n {
                    out.push(Tok::Lit('['));
                    continue;
                }
                out.push(char_set(&p, i, j));
                i = j + 1;
            }
            c => out.push(Tok::Lit(c)),
        }
    }
    out
}

/// The set between `[` (exclusive, at `i`) and `]` (at `j`), following
/// translate's chunking: hyphens found by its scan form ranges, empty
/// ranges are removed, other hyphens are literal.
fn char_set(p: &[char], start: usize, j: usize) -> Tok {
    let stuff: Vec<char> = p[start..j].to_vec();
    let negated_prefix = stuff.first() == Some(&'!');
    let chunks: Vec<Vec<char>> = if !stuff.contains(&'-') {
        vec![stuff.clone()]
    } else {
        let mut chunks: Vec<Vec<char>> = Vec::new();
        let mut i = start;
        let mut k = if p[start] == '!' {
            start + 2
        } else {
            start + 1
        };
        loop {
            let found = (k..j).find(|&x| p[x] == '-');
            let Some(h) = found else { break };
            chunks.push(p[i..h].to_vec());
            i = h + 1;
            k = h + 3;
        }
        let chunk = p[i..j].to_vec();
        if chunk.is_empty() {
            if let Some(last) = chunks.last_mut() {
                last.push('-');
            }
        } else {
            chunks.push(chunk);
        }
        let mut k = chunks.len().saturating_sub(1);
        while k > 0 {
            if chunks[k - 1].last() > chunks[k].first() {
                let mut merged = chunks[k - 1][..chunks[k - 1].len() - 1].to_vec();
                merged.extend_from_slice(&chunks[k][1..]);
                chunks[k - 1] = merged;
                chunks.remove(k);
            }
            k -= 1;
        }
        chunks
    };
    // The set as translate would write it, before the `!`/`^` handling.
    let flat_len: usize =
        chunks.iter().map(Vec::len).sum::<usize>() + chunks.len().saturating_sub(1);
    if flat_len == 0 {
        return Tok::Set {
            negated: false,
            chars: vec![],
            ranges: vec![],
            never: true,
            any: false,
        };
    }
    if flat_len == 1 && chunks.len() == 1 && chunks[0] == ['!'] {
        return Tok::Set {
            negated: false,
            chars: vec![],
            ranges: vec![],
            never: false,
            any: true,
        };
    }
    let mut chars = Vec::new();
    let mut ranges = Vec::new();
    for (idx, chunk) in chunks.iter().enumerate() {
        for (ci, &c) in chunk.iter().enumerate() {
            let left_endpoint = idx + 1 < chunks.len() && ci == chunk.len() - 1;
            let right_endpoint = idx > 0 && ci == 0;
            if !left_endpoint && !right_endpoint {
                chars.push(c);
            }
        }
        if let (true, Some(&lo), Some(&hi)) = (
            idx > 0,
            idx.checked_sub(1).and_then(|p| chunks[p].last()),
            chunk.first(),
        ) {
            ranges.push((lo, hi));
        }
    }
    // translate turns a leading `!` into `^`. That `!` is always the first
    // literal of the first chunk: hyphen scanning starts two past it, and
    // range merging keeps a first chunk of two or more characters whenever
    // another chunk follows, so it is never a range endpoint.
    if negated_prefix {
        debug_assert_eq!(chars.first(), Some(&'!'));
        chars.remove(0);
    }
    Tok::Set {
        negated: negated_prefix,
        chars,
        ranges,
        never: false,
        any: false,
    }
}

/// Python `fnmatch.fnmatch` on POSIX (case-sensitive, `*` crosses `/`).
pub fn fnmatch(name: &str, pat: &str) -> bool {
    let toks = translate(pat);
    let s: Vec<char> = name.chars().collect();
    let (mut si, mut ti) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while si < s.len() {
        if ti < toks.len() && toks[ti] == Tok::Star {
            star = Some((ti, si));
            ti += 1;
        } else if ti < toks.len() && toks[ti].matches(s[si]) {
            si += 1;
            ti += 1;
        } else if let Some((st, ss)) = star {
            ti = st + 1;
            si = ss + 1;
            star = Some((st, ss + 1));
        } else {
            return false;
        }
    }
    while ti < toks.len() && toks[ti] == Tok::Star {
        ti += 1;
    }
    ti == toks.len()
}
