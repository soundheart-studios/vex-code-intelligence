//! Pathological input: deep nesting must not overflow the stack, and a file
//! no AST extractor handles still gets chunks through the line fallback.

use vex_core::canonical::parsed_bytes;
use vex_extract::engine::{chunk_bytes, parse_tree, walk_nodes};
use vex_extract::fallback::line_chunks;

fn deep_python(depth: usize) -> Vec<u8> {
    let mut s = String::from("x = ");
    s.push_str(&"[".repeat(depth));
    s.push_str(&"]".repeat(depth));
    s.push('\n');
    s.into_bytes()
}

/// Run on a thread with a deliberately small stack: an implementation that
/// recursed per nesting level would overflow here long before 10k levels.
fn on_small_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .expect("no stack overflow")
}

#[test]
fn ten_thousand_levels_of_nesting_parse_and_walk_iteratively() {
    let nodes = on_small_stack(|| {
        let src = deep_python(10_000);
        let tree = parse_tree("python", &src).expect("tree-sitter produces a tree");
        let mut count = 0usize;
        walk_nodes(tree.root_node(), |_| count += 1);
        // Depth along the nested lists, in O(depth): `Node::parent` searches
        // from the root, so measuring each node's depth that way is cubic.
        let mut depth = 0usize;
        let mut node = tree.root_node();
        while let Some(child) = node.named_child(node.named_child_count().saturating_sub(1)) {
            node = child;
            depth += 1;
        }
        (count, depth)
    });
    assert!(nodes.0 > 20_000, "walked {} nodes", nodes.0);
    assert!(nodes.1 > 9_000, "the tree really is {} deep", nodes.1);
}

#[test]
fn walk_nodes_visits_in_document_order_and_stays_inside_the_subtree() {
    let src = b"def f():\n    return g(1)\n\ndef h():\n    pass\n";
    let tree = parse_tree("python", src).unwrap();
    let first_def = tree.root_node().named_child(0).unwrap();
    let mut kinds = Vec::new();
    walk_nodes(first_def, |n| kinds.push(n.kind().to_string()));
    assert_eq!(
        kinds.first().map(String::as_str),
        Some("function_definition")
    );
    assert!(kinds.iter().any(|k| k == "call"));
    assert!(
        !kinds.iter().any(|k| k == "pass_statement"),
        "walked past the subtree into h(): {kinds:?}"
    );
    let mut starts = Vec::new();
    walk_nodes(tree.root_node(), |n| starts.push(n.start_byte()));
    assert!(
        starts.windows(2).all(|w| w[0] <= w[1]),
        "pre-order visits never go backwards"
    );
}

#[test]
fn deep_nesting_through_the_facade_still_yields_chunks() {
    let chunks = on_small_stack(|| {
        chunk_bytes("deep.py", "python", &deep_python(10_000))
            .chunks
            .len()
    });
    assert!(chunks >= 1);
}

#[test]
fn a_file_no_extractor_handles_falls_back_to_line_chunks() {
    // A language with no AST extractor (Cortex: parse_file returns None)
    // must produce exactly the line chunker's output, never nothing.
    // Every grammar Cortex parses has an extractor now; Markdown is line
    // chunked in Cortex too.
    let src = b"# Title\n\nSome prose about `main()`.\n\n".repeat(40);
    let via_facade = parsed_bytes(Some(&chunk_bytes("README.md", "markdown", &src)));
    assert_eq!(
        via_facade,
        parsed_bytes(Some(&line_chunks("README.md", "markdown", &src)))
    );
    assert!(!chunk_bytes("README.md", "markdown", &src).chunks.is_empty());
}

#[test]
fn an_unknown_grammar_yields_no_tree() {
    assert!(parse_tree("no-such-language", b"x").is_none());
}
