//! Canonical JSON is byte-identical to Cortex's export: a hand-built Parsed
//! against Python's own canonical()+dumps(), the escaping and key-order rules,
//! and a byte-exact round trip of real golden files.

use indexmap::IndexMap;
use serde_json::Value;
use vex_core::canonical::{parsed_bytes, to_bytes};
use vex_core::ids::{chunk_id, residual_chunk_id};
use vex_core::{Chunk, Parsed, Symbol};

fn chunk(
    rel: &str,
    row0: usize,
    end: usize,
    content: &str,
    kind: &str,
    symbols: Option<&str>,
) -> Chunk {
    let mut meta = std::collections::BTreeMap::new();
    if let Some(s) = symbols {
        meta.insert("symbols".to_string(), Value::from(s));
    }
    Chunk::with_metadata(
        chunk_id(rel, row0, content),
        rel,
        row0 + 1,
        end,
        content,
        kind,
        "python",
        meta,
    )
}

fn sym(qn: &str, name: &str, kind: &str, start: usize, end: usize, declaration: bool) -> Symbol {
    Symbol {
        qualified_name: qn.into(),
        name: name.into(),
        kind: kind.into(),
        file_path: "pkg/mod.py".into(),
        start_line: start,
        end_line: end,
        declaration,
    }
}

/// The same Parsed tests/fixtures/oracle/handbuilt.json was produced from.
fn handbuilt() -> Parsed {
    let rel = "pkg/mod.py";
    let a = chunk(rel, 0, 4, "import os\n\n\nclass A:", "module", Some(""));
    let b = chunk(
        rel,
        4,
        6,
        "    def m(self):\n        return \"q\\\\\" + '\t'",
        "function",
        Some("m"),
    );
    let rc = "x = 'ü\u{2028}\u{1b}'";
    let r = Chunk::with_metadata(
        residual_chunk_id(rel, 9, rc),
        rel,
        9,
        9,
        rc,
        "residual",
        "python",
        Default::default(),
    );
    let mut refs = IndexMap::new();
    for (k, v) in [("zeta", 2), ("Alpha", 1), ("émile", 3), ("_x", 1)] {
        refs.insert(k.to_string(), v);
    }
    let mut bindings = IndexMap::new();
    bindings.insert("os".to_string(), ("os".to_string(), "os".to_string()));
    bindings.insert("B".to_string(), ("pkg.b".to_string(), "Bee".to_string()));
    let mut calls = IndexMap::new();
    calls.insert(
        ("pkg.mod.py.A.m".to_string(), "(instance:C).run".to_string()),
        2,
    );
    calls.insert(("file:pkg/mod.py".to_string(), "print".to_string()), 1);
    calls.insert(
        (
            "pkg.mod.py.A.m".to_string(),
            "(expr:[\"T\",\"X\"]).go".to_string(),
        ),
        1,
    );
    Parsed {
        chunks: vec![b, r, a],
        symbols: vec![
            sym("pkg.mod.py.A.m", "m", "function", 5, 6, false),
            sym("pkg.mod.py.A", "A", "class", 4, 6, false),
            sym("pkg.mod.py.Zeta", "Zeta", "class", 4, 6, true),
        ],
        imports: vec!["os".into(), "abc".into(), "os".into()],
        refs,
        bindings,
        calls,
        members: vec![
            ("A".into(), "(base)0".into(), "Base".into()),
            ("A".into(), "x".into(), "int".into()),
        ],
    }
}

#[test]
fn a_handbuilt_parsed_is_byte_equal_to_cortexs_canonical_output() {
    let want = include_bytes!("fixtures/oracle/handbuilt.json");
    let got = parsed_bytes(Some(&handbuilt()));
    assert_eq!(String::from_utf8_lossy(&got), String::from_utf8_lossy(want));
}

#[test]
fn an_unparsable_file_is_the_empty_canonical_parsed() {
    assert_eq!(
        parsed_bytes(None),
        include_bytes!("fixtures/oracle/empty.json")
    );
}

#[test]
fn an_empty_parsed_equals_the_unparsable_shape() {
    assert_eq!(parsed_bytes(Some(&Parsed::default())), parsed_bytes(None));
}

#[test]
fn escaping_and_key_order_match_python_json_dumps() {
    let want = include_bytes!("fixtures/oracle/escape.json");
    // Parse Python's output and write it back: every rule must hold for the
    // bytes to survive (controls, short escapes, raw DEL, raw U+2028/2029 and
    // astral characters, code-point key order, nested maps).
    let v: Value = serde_json::from_slice(want).unwrap();
    assert_eq!(
        String::from_utf8_lossy(&to_bytes(&v)),
        String::from_utf8_lossy(want)
    );
}

#[test]
fn real_golden_files_round_trip_byte_for_byte() {
    let fixtures: &[(&str, &[u8])] = &[
        (
            "allfields",
            include_bytes!(
                "fixtures/golden/language-475_chromium_build_rust_tests_test_proc_macro_crate_2_crate_src_lib.rs.json"
            ),
        ),
        (
            "capped",
            include_bytes!(
                "fixtures/golden/language-supplement_Pester_tst_functions_assert_Collection_Should-Any.Tests.ps1.json"
            ),
        ),
        (
            "declaration",
            include_bytes!(
                "fixtures/golden/chromium-369_base_android_java_src_org_chromium_base_supplier_SyncOneshotSupplier.java.json"
            ),
        ),
        (
            "escapes",
            include_bytes!(
                "fixtures/golden/language-supplement_cobra_zsh_completions_test.go.json"
            ),
        ),
        (
            "residual",
            include_bytes!(
                "fixtures/golden/language-475_DataStructures.jl_test_test_splay_tree.jl.json"
            ),
        ),
    ];
    for (label, bytes) in fixtures {
        let v: Value = serde_json::from_slice(bytes).unwrap();
        assert_eq!(
            &to_bytes(&v)[..],
            *bytes,
            "{label}: canonical writer changed the golden bytes"
        );
    }
}

#[test]
fn the_writer_sorts_keys_regardless_of_insertion_order() {
    let mut m = serde_json::Map::new();
    for k in ["b", "a", "é", "B", "_"] {
        m.insert(k.to_string(), Value::from(1));
    }
    assert_eq!(
        to_bytes(&Value::Object(m)),
        "{\"B\":1,\"_\":1,\"a\":1,\"b\":1,\"é\":1}\n".as_bytes()
    );
}

/// Every golden output, when the export is present. Ignored by default (the
/// export is not committed); `cargo test -p vex-core -- --ignored` runs it.
#[test]
#[ignore]
fn every_golden_output_round_trips() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../golden/data/parsed");
    let mut stack = vec![root.clone()];
    let mut n = 0;
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d)
            .expect("golden/data is linked")
            .flatten()
        {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let bytes = std::fs::read(&p).unwrap();
                let v: Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(to_bytes(&v), bytes, "{}", p.display());
                n += 1;
            }
        }
    }
    assert_eq!(n, 1267, "every parsed output under {}", root.display());
}
