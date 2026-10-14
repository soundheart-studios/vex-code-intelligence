//! Resolver rules the benchmark packages do not exercise, each pinned by the
//! edges Cortex's own rebuild_call_edges produced from the same synthetic
//! rows (tests/fixtures/crafted, recorded from Cortex).

use serde_json::Value;
use vex_resolve::{Rows, edges_json, rebuild_call_edges};

fn rows(case: &Value) -> Rows {
    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
    let quads = |key: &str| -> Vec<(String, String, String, String)> {
        case[key]
            .as_array()
            .into_iter()
            .flatten()
            .map(|r| (s(&r[0]), s(&r[1]), s(&r[2]), s(&r[3])))
            .collect()
    };
    Rows {
        files: case["files"].as_array().unwrap().iter().map(s).collect(),
        symbols: case["symbols"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| (s(&r[0]), s(&r[1]), s(&r[2]), s(&r[3])))
            .collect(),
        abstract_qns: case["symbols"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r[4] == 1)
            .map(|r| s(&r[0]))
            .collect(),
        bindings: quads("bindings"),
        calls: case["calls"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|r| (s(&r[0]), s(&r[1]), s(&r[2]), r[3].as_i64().unwrap()))
            .collect(),
        members: quads("members"),
        packages: Default::default(),
    }
}

#[test]
fn every_crafted_case_matches_cortex() {
    let cases: Value = serde_json::from_str(include_str!("fixtures/crafted/cases.json")).unwrap();
    let mut failures = Vec::new();
    for (name, case) in cases.as_object().unwrap() {
        let got = edges_json(&rebuild_call_edges(&rows(case)));
        if got != case["edges"] {
            failures.push(format!("{name}: vex {got} vs Cortex {}", case["edges"]));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(cases.as_object().unwrap().len() >= 8);
}
