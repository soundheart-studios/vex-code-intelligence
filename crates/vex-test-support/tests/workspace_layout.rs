//! Every crate the architecture spec names is a workspace member.

use std::process::Command;

const ARCHITECTURE: &[&str] = &[
    "vex-core",
    "vex-extract",
    "vex-resolve",
    "vex-store",
    "vex-embed",
    "vex-search",
    "vex-app",
    "vex-daemon",
    "vex-cli",
    "vex-mcp",
    "xtask",
];

#[test]
fn every_architecture_crate_is_a_workspace_member() {
    let out = Command::new(env!("CARGO"))
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo metadata");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let names: Vec<&str> = meta["packages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|p| p["name"].as_str())
        .collect();
    let missing: Vec<&&str> = ARCHITECTURE.iter().filter(|c| !names.contains(c)).collect();
    assert!(
        missing.is_empty(),
        "architecture crates missing from the workspace: {missing:?}"
    );
}

#[test]
fn the_cli_crate_builds_the_vex_binary() {
    let out = Command::new(env!("CARGO"))
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo metadata");
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let cli = meta["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "vex-cli")
        .expect("vex-cli");
    let bins: Vec<&str> = cli["targets"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["kind"].as_array().unwrap().iter().any(|k| k == "bin"))
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert_eq!(bins, ["vex"]);
}
