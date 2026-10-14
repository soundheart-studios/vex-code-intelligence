//! Kotlin: Cortex's `_CONFIG["kotlin"]`, its soft-keyword mask and
//! `_kotlin_recovered_class`. Imports bind like Java's.

use crate::ast::{Config, children, named_children, text};
use tree_sitter::Node;

pub const CONFIG: Config = Config {
    defs: &[
        ("function_declaration", "function"),
        ("class_declaration", "class"),
        ("object_declaration", "class"),
        ("companion_object", "class"),
        ("property_declaration", "property"),
    ],
    calls: &["call_expression"],
    class_nodes: &[
        "class_declaration",
        "object_declaration",
        "companion_object",
    ],
    imports: &[],
    qualified: true,
};

/// Modifiers tree-sitter-kotlin reads as keywords even as an assignment
/// target (`sealed = "foo"`), and `by` read as continuing delegation.
const SOFT: &[&[u8]] = &[
    b"by",
    b"sealed",
    b"open",
    b"abstract",
    b"override",
    b"private",
    b"public",
    b"internal",
    b"protected",
    b"lateinit",
    b"inline",
    b"suspend",
    b"tailrec",
    b"infix",
    b"external",
    b"final",
    b"vararg",
    b"noinline",
    b"crossinline",
    b"dynamic",
];

/// Cortex `_mask_kotlin_soft_keywords`: a statement-initial soft keyword
/// assigned to (`[ \t]*` then `=` or `+=`, not `==`) becomes underscores of
/// the same length, keeping offsets, lines and every name.
pub fn mask_soft_keywords(source: &[u8]) -> Vec<u8> {
    let mut out = source.to_vec();
    let mut line = 0;
    while line < source.len() {
        let mut i = line;
        while i < source.len() && matches!(source[i], b' ' | b'\t') {
            i += 1;
        }
        // The pattern's alternatives in order; the first that is followed
        // by an assignment wins.
        for word in SOFT {
            if !source[i..].starts_with(word) {
                continue;
            }
            let mut k = i + word.len();
            while k < source.len() && matches!(source[k], b' ' | b'\t') {
                k += 1;
            }
            if source.get(k) == Some(&b'+') {
                k += 1;
            }
            if source.get(k) == Some(&b'=') && source.get(k + 1) != Some(&b'=') {
                for b in &mut out[i..i + word.len()] {
                    *b = b'_';
                }
                break;
            }
        }
        line = match source[line..].iter().position(|&b| b == b'\n') {
            Some(p) => line + p + 1,
            None => source.len(),
        };
    }
    out
}

/// Cortex `_kotlin_recovered_class`: the name of a class the grammar failed
/// to build (an ERROR led by `class Name`, or `annotation class Name` read
/// as an infix expression under stacked annotations), or None.
pub fn recovered_class(node: Node, src: &[u8]) -> Option<String> {
    if node.kind() == "ERROR" {
        let kids = children(node);
        for (index, child) in kids.iter().take(3).enumerate() {
            if matches!(child.kind(), "class" | "interface")
                && let Some(after) = kids.get(index + 1)
                && matches!(
                    after.kind(),
                    "identifier" | "type_identifier" | "simple_identifier"
                )
            {
                return Some(text(*after, src));
            }
            if !matches!(
                child.kind(),
                "modifiers" | "annotation" | "class_modifier" | "visibility_modifier"
            ) {
                break;
            }
        }
        return None;
    }
    let mut node = node;
    while node.kind() == "annotated_expression" {
        match named_children(node).last() {
            Some(last) => node = *last,
            None => break,
        }
    }
    if node.kind() == "infix_expression" {
        let words: Vec<String> = named_children(node).iter().map(|c| text(*c, src)).collect();
        if words.len() == 3
            && words[0] == "annotation"
            && words[1] == "class"
            && crate::ast::is_identifier(&words[2])
        {
            return Some(words[2].clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // Expected values are Cortex's own `_mask_kotlin_soft_keywords` on the
    // same input.
    #[test]
    fn soft_keyword_assignments_are_masked_like_cortex() {
        let src = b"  sealed = \"foo\"\nopen += 1\nby = 2\noverride == x\nval same = sealed\n\tinternal  +=  3\nprivated = 1\n";
        assert_eq!(
            String::from_utf8(mask_soft_keywords(src)).unwrap(),
            "  ______ = \"foo\"\n____ += 1\n__ = 2\noverride == x\nval same = sealed\n\t________  +=  3\nprivated = 1\n"
        );
    }

    #[test]
    fn a_comparison_or_a_line_break_is_not_an_assignment() {
        let src = b"open == 1\nopen\n= 2\n";
        assert_eq!(mask_soft_keywords(src), src.to_vec());
    }
}
