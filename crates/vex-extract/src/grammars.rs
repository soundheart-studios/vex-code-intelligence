//! The grammars compiled into this binary, selected by Cargo features and
//! pinned to Cortex's versions (docs/grammars.md).

use tree_sitter::Language;

// tree-sitter-hcl 1.2.0 is vendored (crates.io stops at 1.1.0) and built
// by build.rs.
#[cfg(feature = "lang-hcl")]
unsafe extern "C" {
    fn tree_sitter_hcl() -> *const ();
}
#[cfg(feature = "lang-hcl")]
const HCL: tree_sitter_language::LanguageFn =
    unsafe { tree_sitter_language::LanguageFn::from_raw(tree_sitter_hcl) };

/// The tree-sitter language for a vex language name, if compiled in.
pub fn language(name: &str) -> Option<Language> {
    let lang = match name {
        #[cfg(feature = "lang-python")]
        "python" => tree_sitter_python::LANGUAGE,
        #[cfg(feature = "lang-javascript")]
        "javascript" => tree_sitter_javascript::LANGUAGE,
        #[cfg(feature = "lang-typescript")]
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
        #[cfg(feature = "lang-typescript")]
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX,
        #[cfg(feature = "lang-go")]
        "go" => tree_sitter_go::LANGUAGE,
        #[cfg(feature = "lang-java")]
        "java" => tree_sitter_java::LANGUAGE,
        #[cfg(feature = "lang-rust")]
        "rust" => tree_sitter_rust::LANGUAGE,
        #[cfg(feature = "lang-c")]
        "c" => tree_sitter_c::LANGUAGE,
        #[cfg(feature = "lang-cpp")]
        "cpp" => tree_sitter_cpp::LANGUAGE,
        #[cfg(feature = "lang-csharp")]
        "csharp" => tree_sitter_c_sharp::LANGUAGE,
        #[cfg(feature = "lang-objc")]
        "objc" => tree_sitter_objc::LANGUAGE,
        #[cfg(feature = "lang-swift")]
        "swift" => tree_sitter_swift::LANGUAGE,
        #[cfg(feature = "lang-scala")]
        "scala" => tree_sitter_scala::LANGUAGE,
        #[cfg(feature = "lang-lua")]
        "lua" => tree_sitter_lua::LANGUAGE,
        #[cfg(feature = "lang-zig")]
        "zig" => tree_sitter_zig::LANGUAGE,
        #[cfg(feature = "lang-elixir")]
        "elixir" => tree_sitter_elixir::LANGUAGE,
        #[cfg(feature = "lang-bash")]
        "bash" => tree_sitter_bash::LANGUAGE,
        #[cfg(feature = "lang-json")]
        "json" => tree_sitter_json::LANGUAGE,
        #[cfg(feature = "lang-julia")]
        "julia" => tree_sitter_julia::LANGUAGE,
        #[cfg(feature = "lang-verilog")]
        "verilog" => tree_sitter_verilog::LANGUAGE,
        #[cfg(feature = "lang-fortran")]
        "fortran" => tree_sitter_fortran::LANGUAGE,
        #[cfg(feature = "lang-powershell")]
        "powershell" => tree_sitter_powershell::LANGUAGE,
        #[cfg(feature = "lang-groovy")]
        "groovy" => tree_sitter_groovy::LANGUAGE,
        #[cfg(feature = "lang-kotlin")]
        "kotlin" => tree_sitter_kotlin_ng::LANGUAGE,
        #[cfg(feature = "lang-php")]
        "php" => tree_sitter_php::LANGUAGE_PHP,
        #[cfg(feature = "lang-ruby")]
        "ruby" => tree_sitter_ruby::LANGUAGE,
        #[cfg(feature = "lang-sql")]
        "sql" => tree_sitter_sequel::LANGUAGE,
        #[cfg(feature = "lang-hcl")]
        "hcl" => HCL,
        _ => return None,
    };
    Some(lang.into())
}

/// Every compiled-in language name, sorted.
pub fn compiled_grammars() -> Vec<&'static str> {
    const ALL: &[&str] = &[
        "bash",
        "c",
        "cpp",
        "csharp",
        "elixir",
        "fortran",
        "go",
        "groovy",
        "hcl",
        "java",
        "javascript",
        "json",
        "julia",
        "kotlin",
        "lua",
        "objc",
        "php",
        "powershell",
        "python",
        "ruby",
        "rust",
        "scala",
        "sql",
        "swift",
        "tsx",
        "typescript",
        "verilog",
        "zig",
    ];
    ALL.iter()
        .copied()
        .filter(|n| language(n).is_some())
        .collect()
}
