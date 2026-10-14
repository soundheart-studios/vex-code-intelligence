//! `vex mcp` on MCP protocol 2026-07-28, the version current Claude Code
//! negotiates: `server/discover` instead of `initialize`, and list results
//! that must say how long they stay fresh (`ttlMs`) and who may cache them
//! (`cacheScope`). Without those two fields the client rejects `tools/list`
//! and the agent sees no tools at all.

mod common;

use serde_json::{Value, json};

const V2026: &str = "2026-07-28";

fn meta(version: &str) -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": version,
        "io.modelcontextprotocol/clientCapabilities": {},
        "io.modelcontextprotocol/clientInfo": {"name": "vex-tests", "version": "0"}
    })
}

#[test]
fn discover_offers_2026_07_28() {
    let env = common::Env::new(false);
    let mut mcp = env.mcp_inline();
    let r = mcp.request("server/discover", json!({"_meta": meta(V2026)}));
    let versions = r["result"]["supportedVersions"].as_array().expect("{r}");
    assert!(versions.contains(&json!(V2026)), "{r}");
}

#[test]
fn tools_list_on_2026_07_28_carries_cache_fields() {
    let env = common::Env::new(false);
    let mut mcp = env.mcp_inline();
    mcp.request("server/discover", json!({"_meta": meta(V2026)}));
    let r = mcp.request("tools/list", json!({"_meta": meta(V2026)}));
    let result = &r["result"];
    assert_eq!(result["tools"].as_array().map(Vec::len), Some(9), "{r}");
    assert!(result["ttlMs"].is_u64(), "ttlMs must be a number: {r}");
    assert!(
        matches!(result["cacheScope"].as_str(), Some("public" | "private")),
        "cacheScope must be public or private: {r}"
    );
}

#[test]
fn tools_call_works_on_2026_07_28() {
    let env = common::Env::new(true);
    let mut mcp = env.mcp_inline();
    mcp.request("server/discover", json!({"_meta": meta(V2026)}));
    let r = mcp.request(
        "tools/call",
        json!({"_meta": meta(V2026), "name": "list_projects", "arguments": {}}),
    );
    let text = r["result"]["content"][0]["text"].as_str().expect("{r}");
    assert!(text.contains("projects"), "{r}");
}

#[test]
fn tools_list_after_initialize_keeps_the_older_shape() {
    let env = common::Env::new(false);
    let mut mcp = env.mcp();
    let r = mcp.request("tools/list", json!({}));
    let result = r["result"].as_object().expect("{r}");
    assert!(!result.contains_key("ttlMs"), "{r}");
    assert!(!result.contains_key("cacheScope"), "{r}");
}
