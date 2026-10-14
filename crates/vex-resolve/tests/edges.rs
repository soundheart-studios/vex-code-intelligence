//! The resolver against Cortex: for every call-graph benchmark package, the
//! rows Cortex's own index held (in its SELECT order) go in and the golden
//! CALLS edges must come out exactly (src, dst, confidence, files).

use serde_json::Value;
use std::path::Path;
use vex_resolve::{
    Rows, edges_json, normpath, rebuild_call_edges, resolve_module, rows_from_json,
    strip_trailing_template,
};

const PACKAGES: &[&str] = &[
    "chi",
    "express",
    "itsdangerous",
    "javapoet",
    "kotlinpoet",
    "ky",
    "leveldb",
    "mediatr",
    "psr7",
    "rack",
    "semver",
];

fn fixture(pkg: &str) -> Value {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/rows/{pkg}.json"));
    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
}

#[test]
fn every_benchmark_package_resolves_to_cortexs_edges() {
    let mut failures = Vec::new();
    for pkg in PACKAGES {
        let v = fixture(pkg);
        let got = edges_json(&rebuild_call_edges(&rows_from_json(&v)));
        if got != v["edges"] {
            let (g, w) = (got.as_array().unwrap(), v["edges"].as_array().unwrap());
            let missing: Vec<&Value> = w.iter().filter(|e| !g.contains(e)).take(3).collect();
            let extra: Vec<&Value> = g.iter().filter(|e| !w.contains(e)).take(3).collect();
            failures.push(format!(
                "{pkg}: {} vs {} edges; missing {missing:?}; extra {extra:?}",
                g.len(),
                w.len()
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The fixture's edges are the golden export's, when the export is present
/// (a guard that the row oracle was captured faithfully).
#[test]
fn the_row_oracle_edges_are_the_golden_edges() {
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let rev = std::fs::read_to_string(root.join("golden/REVISION")).unwrap();
    let golden = Path::new(&home)
        .join("Documents/cortex-corpora/golden")
        .join(rev.trim())
        .join("edges");
    if !golden.is_dir() {
        assert!(
            std::env::var_os("CI").is_some(),
            "the golden export is missing"
        );
        return;
    }
    for pkg in PACKAGES {
        let want: Value =
            serde_json::from_slice(&std::fs::read(golden.join(format!("{pkg}.json"))).unwrap())
                .unwrap();
        assert_eq!(fixture(pkg)["edges"], want, "{pkg}");
    }
}

#[test]
fn a_later_duplicate_edge_replaces_the_earlier_one() {
    // One caller reaches b.py.helper twice: `mod.helper` is import-bound
    // (0.95), then bare `helper` is a unique other-file name match (0.60).
    // Cortex stores edges with INSERT OR REPLACE, so the LAST one wins.
    let rows = |calls: &[&str]| Rows {
        files: ["a.py", "b.py"].iter().map(|s| s.to_string()).collect(),
        symbols: vec![(
            "b.py.helper".into(),
            "helper".into(),
            "function".into(),
            "b.py".into(),
        )],
        bindings: vec![("a.py".into(), "mod".into(), "b".into(), "".into())],
        calls: calls
            .iter()
            .map(|c| ("a.py".to_string(), "a.py.f".to_string(), c.to_string(), 1))
            .collect(),
        ..Rows::default()
    };
    let conf = |calls: &[&str]| -> Vec<f64> {
        rebuild_call_edges(&rows(calls))
            .into_iter()
            .map(|e| e.confidence)
            .collect()
    };
    assert_eq!(conf(&["mod.helper"]), [0.95]);
    assert_eq!(conf(&["helper"]), [0.60]);
    assert_eq!(
        conf(&["mod.helper", "helper"]),
        [0.60],
        "the later row wins"
    );
    assert_eq!(
        conf(&["helper", "mod.helper"]),
        [0.95],
        "the later row wins"
    );
}

#[test]
fn runtime_method_names_never_take_the_name_guess_tiers() {
    let rows = Rows {
        files: ["a.py", "b.py"].iter().map(|s| s.to_string()).collect(),
        symbols: vec![
            (
                "b.py.parse".into(),
                "parse".into(),
                "function".into(),
                "b.py".into(),
            ),
            (
                "b.py.compute".into(),
                "compute".into(),
                "function".into(),
                "b.py".into(),
            ),
        ],
        calls: vec![
            ("a.py".into(), "a.py.f".into(), "parse".into(), 1),
            ("a.py".into(), "a.py.f".into(), "compute".into(), 1),
        ],
        ..Rows::default()
    };
    let dsts: Vec<String> = rebuild_call_edges(&rows)
        .into_iter()
        .map(|e| format!("{} {}", e.dst, e.confidence))
        .collect();
    assert_eq!(dsts, ["b.py.compute 0.6"]);
}

#[test]
fn python_path_semantics() {
    // posixpath.normpath
    for (i, o) in [
        ("", "."),
        ("a/./b", "a/b"),
        ("a/../b", "b"),
        ("../a", "../a"),
        ("a/b/../../..", ".."),
        ("/a/../..", "/"),
        ("//a", "//a"),
        ("///a", "/a"),
    ] {
        assert_eq!(normpath(i), o, "normpath({i:?})");
    }
    // re.sub(r"<[^.]*>$", "", s)
    for (i, o) in [
        ("SkipList<Key, Comparator>", "SkipList"),
        ("A<B<C>>", "A"),
        ("x.Y<T>", "x.Y"),
        ("a<b.c>", "a<b.c>"),
        ("Plain", "Plain"),
        ("A<>", "A"),
    ] {
        assert_eq!(strip_trailing_template(i), o, "{i:?}");
    }
}

#[test]
fn module_resolution_follows_cortex() {
    let files = [
        "src/pkg/mod.py",
        "src/pkg/__init__.py",
        "web/app.ts",
        "web/util/index.ts",
        "src/lib.rs",
        "src/error.rs",
        "go/x/y/z.go",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let none = Default::default();
    assert_eq!(
        resolve_module("./util", "web/app.ts", &files, &none),
        "web/util/index.ts"
    );
    assert_eq!(
        resolve_module("crate", "src/error.rs", &files, &none),
        "src/lib.rs"
    );
    assert_eq!(
        resolve_module("crate::error", "src/lib.rs", &files, &none),
        "src/error.rs"
    );
    assert_eq!(
        resolve_module("github.com/o/y", "go/a.go", &files, &none),
        "dir:go/x/y"
    );
    assert_eq!(
        resolve_module("pkg.mod", "src/a.py", &files, &none),
        "src/pkg/mod.py"
    );
    assert_eq!(
        resolve_module("pkg", "src/a.py", &files, &none),
        "src/pkg/__init__.py"
    );
    assert_eq!(resolve_module("", "src/a.py", &files, &none), "");
    // Python relative imports go through the JS resolver in Cortex and so
    // resolve nothing for .py files; ported as is.
    assert_eq!(
        resolve_module(".mod", "src/pkg/__init__.py", &files, &none),
        ""
    );
}
