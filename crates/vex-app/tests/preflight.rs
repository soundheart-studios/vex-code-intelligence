//! preflight.scan against Cortex's report on the declarative fixture tree.

mod common;

use common::{build_tree, fixture};
use serde_json::{Value, json};
use std::collections::HashSet;
use vex_app::preflight::{Report, scan};
use vex_test_support::TestHome;

fn as_json(r: &Report) -> Value {
    let dirs: serde_json::Map<String, Value> = r
        .dirs
        .iter()
        .map(|(k, d)| {
            (k.clone(), json!({
                "files": d.files, "est_chunks": d.est_chunks, "fallback_chunks": d.fallback_chunks,
                "generated_files": d.generated_files, "generated_chunks": d.generated_chunks,
                "data_files": d.data_files, "data_chunks": d.data_chunks,
                "languages": d.languages.iter().collect::<Vec<_>>(),
            }))
        })
        .collect();
    json!({
        "dirs": dirs, "total_files": r.total_files, "total_est_chunks": r.total_est_chunks,
        "suggestions": r.suggestions, "suggested_savings": r.suggested_savings, "ignored_caches": r.ignored_caches,
    })
}

#[test]
fn the_scan_report_equals_cortexs() {
    let h = TestHome::new();
    let root = h.vex_home().join("tree");
    build_tree(&root);
    assert_eq!(
        as_json(&scan(&root, &HashSet::new())),
        fixture("expected.json")["preflight"]
    );
}

#[test]
fn already_indexed_files_cost_nothing_like_cortex() {
    let h = TestHome::new();
    let root = h.vex_home().join("tree");
    build_tree(&root);
    let already: HashSet<String> = ["main.py", "src/gen/one.py"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(
        as_json(&scan(&root, &already)),
        fixture("expected.json")["preflight_already_indexed"]
    );
}

#[test]
fn the_report_flags_generated_bulk_data_and_nested_repos() {
    let expected = fixture("expected.json");
    let s = expected["preflight"]["suggestions"].as_array().unwrap();
    // Sanity on the oracle itself, so a regenerated fixture cannot quietly
    // stop exercising the three suggestion paths.
    for want in ["data", "packages/other", "src/gen"] {
        assert!(s.iter().any(|x| x == want), "{want} missing from {s:?}");
    }
    assert!(
        s.iter().any(|x| x == "gen2/inner") && !s.iter().any(|x| x == "gen2"),
        "a suggested child drops its suggested parent: {s:?}"
    );
    assert_eq!(
        expected["preflight"]["dirs"]["exact"]["data_files"], 1,
        "exactly 32 KiB is bulk data, one byte less is not"
    );
    assert!(
        !s.iter().any(|x| x == "libs/sub"),
        "a submodule is not a nested repository"
    );
}
