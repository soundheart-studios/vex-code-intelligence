//! Which paths are implementation and which are tests, mocks or docs
//! (Cortex `paths.py`).

/// Directory names that mean "this is not the thing you asked about".
pub const NON_SOURCE_SEGMENTS: &[&str] = &[
    "test",
    "tests",
    "__tests__",
    "spec",
    "specs",
    "mock",
    "mocks",
    "__mocks__",
    "fixture",
    "fixtures",
    "testdata",
    "docs",
    "doc",
    "examples",
    "example",
    "benchmarks",
    "benchmark",
    "bench",
    "node_modules",
    "dist",
    "build",
    "vendor",
];

/// Filename markers: `dispatch.test.ts`, `store.spec.tsx`.
pub const NON_SOURCE_NAME_PARTS: &[&str] = &[".test.", ".spec.", ".stories."];

/// Non-empty path segments, with `\` read as `/`.
pub fn segments(file_path: &str) -> Vec<String> {
    file_path
        .replace('\\', "/")
        .split('/')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

pub fn is_non_source(file_path: &str) -> bool {
    let parts = segments(file_path);
    let dirs = &parts[..parts.len().saturating_sub(1)];
    if dirs
        .iter()
        .any(|s| NON_SOURCE_SEGMENTS.contains(&s.to_lowercase().as_str()))
    {
        return true;
    }
    let name = parts.last().map(|s| s.to_lowercase()).unwrap_or_default();
    NON_SOURCE_NAME_PARTS.iter().any(|m| name.contains(m))
        || name.ends_with("_test.go")
        || name.starts_with("test_")
        || name == "conftest.py"
}

/// Directory names that mean "this is the implementation", at any depth.
pub const SOURCE_SEGMENTS: &[&str] = &[
    "src", "lib", "app", "apps", "packages", "internal", "pkg", "cmd", "source",
];

/// Extensions that hold code.
pub const CODE_EXTENSIONS: &[&str] = &[
    ".py", ".pyi", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".mts", ".cts", ".c", ".h", ".cc",
    ".cpp", ".cxx", ".hpp", ".hh", ".hxx", ".m", ".mm", ".go", ".rs", ".java", ".kt", ".kts",
    ".swift", ".scala", ".rb", ".php", ".cs", ".fs", ".ex", ".exs", ".erl", ".hs", ".ml", ".lua",
    ".jl", ".zig", ".dart", ".sh", ".bash", ".zsh", ".ps1", ".sql", ".proto", ".metal", ".cu",
    ".cuh", ".glsl", ".comp", ".vert", ".frag", ".wgsl", ".v", ".sv", ".f90", ".vb", ".pl", ".r",
    ".clj",
];

/// The file name's extension (from its last dot, not a leading one) is code.
pub fn is_code_file(file_path: &str) -> bool {
    let parts = segments(file_path);
    let name = parts.last().map(String::as_str).unwrap_or("");
    match name.rfind('.') {
        Some(dot) if dot > 0 => CODE_EXTENSIONS.contains(&name[dot..].to_lowercase().as_str()),
        _ => false,
    }
}

/// Implementation: not non-source, and in a source directory or a code file.
pub fn is_source(file_path: &str) -> bool {
    if is_non_source(file_path) {
        return false;
    }
    let parts = segments(file_path);
    let dirs = &parts[..parts.len().saturating_sub(1)];
    if dirs
        .iter()
        .any(|s| SOURCE_SEGMENTS.contains(&s.to_lowercase().as_str()))
    {
        return true;
    }
    is_code_file(file_path)
}
