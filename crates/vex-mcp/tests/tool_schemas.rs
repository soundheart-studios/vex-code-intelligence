//! `tools/list` from the real `vex mcp`: exactly Cortex's nine code tools,
//! in Cortex's order, each with Cortex's exact input schema (recorded in
//! fixtures/cortex_tools.json from Cortex). Descriptions are vex's
//! own and must not send an agent to Cortex.

mod common;

use serde_json::{Value, json};

fn cortex_tools() -> Vec<Value> {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cortex_tools.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn tools_list_advertises_cortex_tools_with_their_schemas() {
    let env = common::Env::new(false);
    let mut mcp = env.mcp();
    let listed = mcp.request("tools/list", json!({}));
    let tools = listed["result"]["tools"].as_array().unwrap();
    let cortex = cortex_tools();
    let names = |ts: &[Value]| -> Vec<String> {
        ts.iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(names(tools), names(&cortex), "tool names and order");
    for (ours, theirs) in tools.iter().zip(&cortex) {
        let name = &ours["name"];
        assert_eq!(
            ours["inputSchema"], theirs["inputSchema"],
            "{name}: input schema"
        );
        let description = ours["description"].as_str().unwrap();
        assert!(!description.trim().is_empty(), "{name}: description");
        assert!(
            !description.to_lowercase().contains("cortex"),
            "{name}: description mentions Cortex"
        );
    }
}

#[test]
fn the_compiled_table_matches_what_is_served() {
    // vex_mcp::tools() is what the server lists; the two must not drift.
    let env = common::Env::new(false);
    let mut mcp = env.mcp();
    let listed = mcp.request("tools/list", json!({}));
    let served = serde_json::to_value(vex_mcp::tools()).unwrap();
    for (ours, served) in listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .zip(served.as_array().unwrap())
    {
        assert_eq!(ours["name"], served["name"]);
        assert_eq!(ours["inputSchema"], served["inputSchema"]);
    }
}
