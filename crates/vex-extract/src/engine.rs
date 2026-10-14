//! The chunking facade (Cortex `chunker.chunk_file`) and the tree-sitter
//! plumbing every language extractor shares.

use crate::fallback::{line_chunks, source_lines};
use crate::residual::fill_gaps;
use tree_sitter::{Node, Parser, Tree};
use vex_core::Parsed;

/// AST chunking when a grammar is available, line chunking otherwise; AST
/// output is completed with residual chunks for the lines it left uncovered.
/// Pure: the caller supplies the bytes.
pub fn chunk_bytes(rel: &str, lang: &str, bytes: &[u8]) -> Parsed {
    chunk_bytes_with(rel, lang, bytes, &no_files)
}

/// Where a script's `source ./x.sh` points: given the sourcing file's
/// directory (relative to the root, "" at the top) and the path as
/// written, the file's path relative to the root when it exists under it.
/// Extraction does no I/O; the caller answers.
pub type SourceResolver<'a> = &'a dyn Fn(&str, &str) -> Option<String>;

/// A resolver for which no sourced file exists.
pub fn no_files(_dir: &str, _written: &str) -> Option<String> {
    None
}

/// `chunk_bytes` with a resolver for sourced files.
pub fn chunk_bytes_with(rel: &str, lang: &str, bytes: &[u8], resolve: SourceResolver) -> Parsed {
    let parsed = if lang.is_empty() {
        None
    } else {
        parse_ast_with(rel, lang, bytes, resolve)
    };
    match parsed {
        Some(mut p) if !p.chunks.is_empty() => {
            fill_gaps(rel, lang, &source_lines(bytes), &mut p);
            p
        }
        _ => line_chunks(rel, lang, bytes),
    }
}

/// The AST extraction for a language, or None when no extractor is wired
/// in for it (Cortex `parse_file` returning None for a missing grammar).
/// Languages are wired one at a time, each only once its golden files match.
pub fn parse_ast(rel: &str, lang: &str, bytes: &[u8]) -> Option<Parsed> {
    parse_ast_with(rel, lang, bytes, &no_files)
}

/// `parse_ast` with a resolver for sourced files (Bash).
pub fn parse_ast_with(
    rel: &str,
    lang: &str,
    bytes: &[u8],
    resolve: SourceResolver,
) -> Option<Parsed> {
    match lang {
        "zig" => crate::lang::zig::parse_zig(rel, bytes),
        "elixir" => crate::lang::elixir::parse_elixir(rel, bytes),
        "bash" => crate::lang::bash::parse_bash(rel, bytes, resolve),
        "json" => crate::lang::json::parse_json(rel, bytes),
        "julia" => crate::lang::julia::parse_julia(rel, bytes),
        "verilog" => crate::lang::verilog::parse_verilog(rel, bytes),
        "fortran" => crate::lang::fortran::parse_fortran(rel, bytes),
        "powershell" => crate::lang::powershell::parse_powershell(rel, bytes),
        "groovy" => crate::lang::groovy::parse_groovy(rel, bytes),
        "sql" => crate::lang::sql::parse_sql(rel, bytes),
        "hcl" => crate::lang::hcl::parse_hcl(rel, bytes),
        "python" => {
            let tree = parse_tree("python", bytes)?;
            Some(crate::ast::parse_file(
                rel,
                lang,
                bytes,
                &tree,
                &crate::lang::python::CONFIG,
            ))
        }
        "c" => {
            let tree = parse_tree("c", bytes)?;
            Some(crate::ast::parse_file(
                rel,
                lang,
                bytes,
                &tree,
                &crate::lang::c::CONFIG,
            ))
        }
        "cpp" => {
            let masked = crate::lang::cpp::mask_annotations(bytes);
            let tree = parse_tree("cpp", &masked)?;
            Some(crate::ast::parse_file_with_grammar_source(
                rel,
                lang,
                bytes,
                &masked,
                &tree,
                &crate::lang::cpp::CONFIG,
            ))
        }
        "go" => {
            let tree = parse_tree("go", bytes)?;
            Some(crate::ast::parse_file(
                rel,
                lang,
                bytes,
                &tree,
                &crate::lang::go::CONFIG,
            ))
        }
        "csharp" => {
            let tree = parse_tree("csharp", bytes)?;
            Some(crate::ast::parse_file(
                rel,
                lang,
                bytes,
                &tree,
                &crate::lang::csharp::CONFIG,
            ))
        }
        "objc" => crate::lang::objc::parse_objc(rel, bytes),
        "kotlin" => {
            let masked = crate::lang::kotlin::mask_soft_keywords(bytes);
            let tree = parse_tree("kotlin", &masked)?;
            Some(crate::ast::parse_file_with_grammar_source(
                rel,
                lang,
                bytes,
                &masked,
                &tree,
                &crate::lang::kotlin::CONFIG,
            ))
        }
        "php" | "ruby" => {
            let tree = parse_tree(lang, bytes)?;
            let cfg = if lang == "php" {
                &crate::lang::php::CONFIG
            } else {
                &crate::lang::ruby::CONFIG
            };
            Some(crate::ast::parse_file(rel, lang, bytes, &tree, cfg))
        }
        "swift" | "scala" | "lua" => {
            let tree = parse_tree(lang, bytes)?;
            let cfg = match lang {
                "swift" => &crate::lang::swift::CONFIG,
                "scala" => &crate::lang::scala::CONFIG,
                _ => &crate::lang::lua::CONFIG,
            };
            Some(crate::ast::parse_file(rel, lang, bytes, &tree, cfg))
        }
        "java" => {
            let tree = parse_tree("java", bytes)?;
            Some(crate::ast::parse_file(
                rel,
                lang,
                bytes,
                &tree,
                &crate::lang::java::CONFIG,
            ))
        }
        "rust" => {
            let tree = parse_tree("rust", bytes)?;
            Some(crate::ast::parse_file(
                rel,
                lang,
                bytes,
                &tree,
                &crate::lang::rust::CONFIG,
            ))
        }
        "javascript" => {
            let tree = parse_tree("javascript", bytes)?;
            Some(crate::ast::parse_file(
                rel,
                lang,
                bytes,
                &tree,
                &crate::lang::javascript::CONFIG,
            ))
        }
        "typescript" => {
            // Cortex's parser key: `tsx` for a `.tsx` suffix (Path.suffix,
            // so a bare dotfile named `.tsx` has none), else `typescript`.
            let tsx = std::path::Path::new(rel)
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("tsx"));
            let tree = parse_tree(if tsx { "tsx" } else { "typescript" }, bytes)?;
            Some(crate::ast::parse_file(
                rel,
                lang,
                bytes,
                &tree,
                &crate::lang::typescript::CONFIG,
            ))
        }
        _ => None,
    }
}

/// Parse `source` with the compiled grammar `grammar` (a vex language name,
/// `tsx` for TSX). None when the grammar is not compiled in or tree-sitter
/// declines to produce a tree.
pub fn parse_tree(grammar: &str, source: &[u8]) -> Option<Tree> {
    let language = crate::grammars::language(grammar)?;
    let mut parser = Parser::new();
    parser.set_language(&language).ok()?;
    parser.parse(source, None)
}

/// Visit every node under `root` (root included) depth-first in document
/// order, iteratively: deeply nested sources must not overflow the stack.
pub fn walk_nodes<'t>(root: Node<'t>, mut visit: impl FnMut(Node<'t>)) {
    let mut cursor = root.walk();
    loop {
        visit(cursor.node());
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.node() == root {
                return;
            }
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return;
            }
        }
    }
}
