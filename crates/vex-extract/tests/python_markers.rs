//! Python extraction against Cortex's own chunk_file output on crafted
//! sources: every call-evidence marker ((super), (instance:C), (dynamic)),
//! the rules that withhold evidence, owners, bases, bindings, truncation,
//! caller attribution and the line/row quirks (tests/fixtures/python).

use serde_json::Value;
use vex_core::canonical::parsed_bytes;
use vex_extract::engine::chunk_bytes;
use vex_extract::parity_diff;

fn cases() -> Value {
    let p =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/python/cases.json");
    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
}

#[test]
fn every_crafted_python_case_matches_cortex() {
    let mut failures = Vec::new();
    let all = cases();
    for (name, case) in all.as_object().unwrap() {
        let src = case["source"].as_str().unwrap();
        let got = String::from_utf8(parsed_bytes(Some(&chunk_bytes(
            &format!("pkg/{name}"),
            "python",
            src.as_bytes(),
        ))))
        .unwrap();
        let want = case["canonical"].as_str().unwrap();
        if got != want {
            failures.push(format!("{name}: {}", parity_diff(&got, want)));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} cases differ:\n{}",
        failures.len(),
        all.as_object().unwrap().len(),
        failures.join("\n")
    );
}

fn calls_of(case: &str) -> Vec<String> {
    let all = cases();
    let v: Value = serde_json::from_str(all[case]["canonical"].as_str().unwrap()).unwrap();
    v["calls"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c[1].as_str().unwrap().to_string())
        .collect()
}

/// Guards on the oracle itself, so a regenerated fixture cannot quietly stop
/// exercising a marker family.
#[test]
fn the_oracle_still_exercises_every_marker_family() {
    let sup = calls_of("super_forms.py");
    assert!(sup.contains(&"(super).save".into()) && sup.contains(&"(super).touch".into()));
    let inst = calls_of("instance_evidence.py");
    assert!(
        inst.contains(&"(instance:Client).connect".into()),
        "straight-line evidence"
    );
    assert!(
        inst.contains(&"(instance:mod.Factory).make".into()),
        "qualified constructor"
    );
    assert!(
        inst.contains(&"c.connect".into()),
        "withheld evidence keeps the written name"
    );
    let comp = calls_of("computed.py");
    assert!(comp.contains(&"(dynamic).run".into()) && comp.contains(&"(dynamic).update".into()));
}
