//! The compiled grammars are exactly Cortex's: locked crate versions and
//! structural fingerprints both match docs/grammars.md.

use serde_json::Value;
use std::path::Path;

fn workspace() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
}

fn pins() -> Value {
    let doc =
        std::fs::read_to_string(workspace().join("docs/grammars.md")).expect("docs/grammars.md");
    let start = doc.find("```json").expect("a ```json block") + "```json".len();
    let end = start + doc[start..].find("```").expect("closing fence");
    serde_json::from_str(&doc[start..end]).expect("valid pin JSON")
}

#[test]
fn every_pinned_language_is_compiled_in_with_its_fingerprint() {
    let pins = pins();
    for (name, want) in pins["languages"].as_object().unwrap() {
        let lang =
            vex_extract::language(name).unwrap_or_else(|| panic!("{name} is not compiled in"));
        // A recorded exception: the same tables emitted under an older ABI.
        let abi = match pins["abi_exceptions"][name].as_object() {
            Some(e) if e["crate"] == lang.abi_version() => e["wheel"].clone(),
            _ => lang.abi_version().into(),
        };
        let got = serde_json::json!({
            "abi": abi,
            "kinds": lang.node_kind_count(),
            "fields": lang.field_count(),
            "states": lang.parse_state_count(),
        });
        assert_eq!(
            &got, want,
            "{name}: grammar differs from Cortex's Python wheel"
        );
    }
}

#[test]
fn locked_crate_versions_match_the_pins() {
    let lock: toml::Table = std::fs::read_to_string(workspace().join("Cargo.lock"))
        .unwrap()
        .parse()
        .unwrap();
    let packages = lock["package"].as_array().unwrap();
    for (krate, want) in pins()["crates"].as_object().unwrap() {
        let versions: Vec<&str> = packages
            .iter()
            .filter(|p| p["name"].as_str() == Some(krate))
            .filter_map(|p| p["version"].as_str())
            .collect();
        assert_eq!(versions, [want.as_str().unwrap()], "{krate} in Cargo.lock");
    }
}

#[test]
fn compiled_grammars_lists_exactly_the_pinned_languages() {
    let mut pinned: Vec<String> = pins()["languages"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    pinned.sort();
    assert_eq!(vex_extract::compiled_grammars(), pinned);
}

#[test]
fn every_pinned_crate_records_its_source_commit() {
    let pins = pins();
    for krate in pins["crates"].as_object().unwrap().keys() {
        let sha = pins["commits"][krate]
            .as_str()
            .unwrap_or_else(|| panic!("{krate} has no source commit"));
        assert!(
            sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()),
            "{krate}: {sha:?}"
        );
    }
}
