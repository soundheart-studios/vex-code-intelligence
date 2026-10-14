//! Every MCP tool returns exactly what the CLI's `--json` prints for the
//! same arguments, both through the real `vex` binary on one fixture
//! project. A CLI error document `{"schema": 1, "error": c, "message": m}`
//! reads `VEX ERROR (c): m` over MCP. Timings, and the index version a
//! forced rebuild draws afresh, are the only per-call values and are
//! redacted on both sides.

mod common;

use serde_json::{Value, json};

/// Replace the value after `"key":` up to the next `,` or `}`.
fn redact_value(text: &str, key: &str) -> String {
    let needle = format!("\"{key}\":");
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(&needle) {
        out.push_str(&rest[..at + needle.len()]);
        rest = &rest[at + needle.len()..];
        let end = rest.find([',', '}']).unwrap_or(rest.len());
        out.push_str(&format!("\"<{key}>\""));
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

fn redact(text: &str) -> String {
    [
        "took_ms",
        "selector_ms",
        // Counts the selector_ms digits, so it moves with the clock.
        "response_bytes",
        "index_age_s",
        "index_version",
    ]
    .iter()
    .fold(text.trim_end().to_string(), |t, k| redact_value(&t, k))
}

/// What MCP must return for this CLI output.
fn expected(cli_stdout: &str) -> String {
    let doc: Value = serde_json::from_str(cli_stdout).unwrap();
    let keys: Vec<&String> = doc.as_object().unwrap().keys().collect();
    let mut sorted = keys.clone();
    sorted.sort();
    if sorted == ["error", "message", "schema"] {
        format!(
            "VEX ERROR ({}): {}",
            doc["error"].as_str().unwrap(),
            doc["message"].as_str().unwrap()
        )
    } else {
        redact(cli_stdout)
    }
}

fn assert_same(
    env: &common::Env,
    mcp: &mut common::Session,
    tool: &str,
    args: Value,
    cli: &[&str],
) {
    let (text, is_error) = mcp.call(tool, args.clone());
    assert!(!is_error, "{tool} {args}: {text}");
    let mut full = vec!["--json"];
    full.extend_from_slice(cli);
    let (_, stdout) = env.cli(&full);
    assert_eq!(
        redact(&text),
        expected(&stdout),
        "{tool} {args} vs vex {cli:?}"
    );
}

#[test]
fn every_tool_answers_like_the_cli() {
    let env = common::Env::new(true);
    let mut mcp = env.mcp();
    let same = |mcp: &mut common::Session, tool: &str, args: Value, cli: &[&str]| {
        assert_same(&env, mcp, tool, args, cli)
    };
    same(&mut mcp, "list_projects", json!({}), &["projects"]);

    same(
        &mut mcp,
        "search",
        json!({"query": "signature"}),
        &["search", "signature"],
    );
    same(
        &mut mcp,
        "search",
        json!({"query": ["timestamp signer", "base64 decode"]}),
        &["search", "--list", "timestamp signer", "base64 decode"],
    );
    same(
        &mut mcp,
        "search",
        json!({"query": ["hub"]}),
        &["search", "--list", "hub"],
    );
    same(
        &mut mcp,
        "search",
        json!({"query": "signature", "top_k": 2, "path": "graphpkg", "max_bytes": 900}),
        &[
            "search",
            "signature",
            "--top-k",
            "2",
            "--path",
            "graphpkg",
            "--max-bytes",
            "900",
        ],
    );
    same(
        &mut mcp,
        "search",
        json!({"query": ["a1", "b2", "c3", "d4", "e5", "f6", "g7"]}),
        &["search", "a1", "b2", "c3", "d4", "e5", "f6", "g7"],
    );
    same(
        &mut mcp,
        "search",
        json!({"query": ["sign", "decode"], "select": "model"}),
        &["search", "sign", "decode", "--select", "model"],
    );
    same(
        &mut mcp,
        "search",
        json!({"query": "hub", "force_fresh": true}),
        &["search", "hub", "--force-fresh"],
    );

    same(
        &mut mcp,
        "context",
        json!({"symbol": "mid"}),
        &["context", "mid"],
    );
    same(
        &mut mcp,
        "context",
        json!({"symbol": "control", "detail": "structure"}),
        &["context", "control", "--detail", "structure"],
    );
    same(
        &mut mcp,
        "context",
        json!({"symbol": "want_bytes", "confidence_min": 0.0}),
        &["context", "want_bytes", "--confidence-min", "0"],
    );
    same(
        &mut mcp,
        "context",
        json!({"symbol": "nosuch"}),
        &["context", "nosuch"],
    );

    same(
        &mut mcp,
        "impact",
        json!({"symbol": "leaf"}),
        &["impact", "leaf"],
    );
    same(
        &mut mcp,
        "impact",
        json!({"symbol": ["hub", "nosuch"]}),
        &["impact", "hub", "nosuch"],
    );
    same(
        &mut mcp,
        "impact",
        json!({"symbol": ["hub"]}),
        &["impact", "--list", "hub"],
    );
    same(
        &mut mcp,
        "impact",
        json!({"symbol": "hub", "direction": "both", "depth": 1}),
        &["impact", "hub", "--direction", "both", "--depth", "1"],
    );
    same(
        &mut mcp,
        "impact",
        json!({"symbol": "top", "direction": "downstream", "confidence_min": 0.5}),
        &[
            "impact",
            "top",
            "--direction",
            "downstream",
            "--confidence-min",
            "0.5",
        ],
    );

    // confidence_min only bites for 'both'.
    same(
        &mut mcp,
        "impact",
        json!({"symbol": "want_bytes", "direction": "both", "depth": 1, "confidence_min": 0.0}),
        &[
            "impact",
            "want_bytes",
            "--direction",
            "both",
            "--depth",
            "1",
            "--confidence-min",
            "0",
        ],
    );

    same(&mut mcp, "map", json!({}), &["map"]);
    same(
        &mut mcp,
        "map",
        json!({"focus": ["graphpkg/"], "budget_tokens": 200}),
        &["map", "--focus", "graphpkg/", "--budget-tokens", "200"],
    );
    same(&mut mcp, "map", json!({"focus": null}), &["map"]);

    same(
        &mut mcp,
        "expand",
        json!({"target": "want_bytes"}),
        &["expand", "want_bytes"],
    );
    same(
        &mut mcp,
        "expand",
        json!({"target": ["hub", "leaf"], "detail": "signature"}),
        &["expand", "hub", "leaf", "--detail", "signature"],
    );
    same(
        &mut mcp,
        "expand",
        json!({"target": ["hub"], "max_bytes": 2000}),
        &["expand", "--list", "hub", "--max-bytes", "2000"],
    );
    same(
        &mut mcp,
        "expand",
        json!({"target": "nosuch"}),
        &["expand", "nosuch"],
    );

    same(
        &mut mcp,
        "grep",
        json!({"pattern": "def hub", "context_lines": 1}),
        &["grep", "def hub", "-C", "1"],
    );
    same(
        &mut mcp,
        "grep",
        json!({"pattern": "(x", "literal": true, "path": "graphpkg", "max_results": 3}),
        &[
            "grep",
            "(x",
            "--literal",
            "--path",
            "graphpkg",
            "--max-results",
            "3",
        ],
    );
    same(&mut mcp, "grep", json!({"pattern": "("}), &["grep", "("]);
    // A literal that is both inside and outside the path, past the cap.
    same(
        &mut mcp,
        "grep",
        json!({"pattern": "def ", "path": "graphpkg", "max_results": 2}),
        &["grep", "def ", "--path", "graphpkg", "--max-results", "2"],
    );

    same(&mut mcp, "detect_changes", json!({}), &["changes"]);
    same(
        &mut mcp,
        "detect_changes",
        json!({"base": "nosuch"}),
        &["changes", "--base", "nosuch"],
    );

    same(&mut mcp, "reindex", json!({}), &["index"]);
    same(
        &mut mcp,
        "reindex",
        json!({"force": true}),
        &["index", "--force"],
    );

    // An unindexed edit: stale answers, then changes once reindexed.
    let hub = env.root.join("graphpkg/hub.py");
    let text = std::fs::read_to_string(&hub).unwrap();
    std::fs::write(&hub, text.replace("    return 1\n", "    return 2\n")).unwrap();
    same(
        &mut mcp,
        "search",
        json!({"query": "leaf"}),
        &["search", "leaf"],
    );
    same(
        &mut mcp,
        "map",
        json!({"budget_tokens": 120}),
        &["map", "--budget-tokens", "120"],
    );
    // A refusal from a stale index carries freshness, so it stays a
    // document rather than becoming VEX ERROR text.
    let (refused, _) = mcp.call("expand", json!({"target": "nosuch"}));
    assert!(
        refused.starts_with("{\"schema\":1,\"error\":\"not_found\""),
        "{refused}"
    );
    same(
        &mut mcp,
        "expand",
        json!({"target": "nosuch"}),
        &["expand", "nosuch"],
    );
    same(
        &mut mcp,
        "detect_changes",
        json!({"max_depth": 1}),
        &["changes", "--max-depth", "1"],
    );
    // reindex changes state, so each side gets an equivalent edit of its
    // own: same file, same shape of change, same work to do.
    let (reindexed, _) = mcp.call("reindex", json!({}));
    std::fs::write(&hub, text.replace("    return 1\n", "    return 3\n")).unwrap();
    let (_, stdout) = env.cli(&["--json", "index"]);
    assert_eq!(
        redact(&reindexed),
        expected(&stdout),
        "reindex after an edit"
    );
    same(
        &mut mcp,
        "detect_changes",
        json!({"base": "HEAD"}),
        &["changes", "--base", "HEAD"],
    );
}

#[test]
fn select_model_is_accepted_and_reports_rules_v1() {
    let env = common::Env::new(true);
    let mut mcp = env.mcp();
    let (text, is_error) = mcp.call(
        "search",
        json!({"query": ["timestamp signer", "base64 decode"], "select": "model"}),
    );
    assert!(!is_error, "{text}");
    let doc: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(doc["selector"]["model"], "rules-v1", "{}", doc["selector"]);
    assert_eq!(
        doc["selector"]["provider"], "heuristic",
        "{}",
        doc["selector"]
    );
}

#[test]
fn project_names_ids_and_paths_resolve() {
    let env = common::Env::new(true);
    let mut mcp = env.mcp();
    let (listing, _) = mcp.call("list_projects", json!({}));
    let projects: Value = serde_json::from_str(&listing).unwrap();
    let id = projects["projects"][0]["project_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (here, _) = mcp.call("map", json!({}));
    let root = env.root.display().to_string();
    for project in [id.as_str(), "qa", root.as_str(), "."] {
        let (text, is_error) = mcp.call("map", json!({"project": project}));
        assert!(!is_error, "{project}: {text}");
        assert_eq!(text, here, "project={project}");
    }
    let (text, is_error) = mcp.call("map", json!({"project": "  qa  "}));
    assert!(!is_error);
    assert_eq!(text, here, "a padded name is trimmed");
    let (text, _) = mcp.call("map", json!({"project": "q"}));
    assert!(
        text.starts_with("VEX ERROR (not_found): unknown project 'q'"),
        "{text}"
    );
    let (text, _) = mcp.call("map", json!({"project": "nosuch"}));
    assert_eq!(
        text,
        "VEX ERROR (not_found): unknown project 'nosuch'; call list_projects() for the registered ones"
    );
}

#[test]
fn list_projects_marks_a_root_that_is_gone() {
    let env = common::Env::new(true);
    let other = env.home.vex_home().join("other");
    std::fs::create_dir_all(&other).unwrap();
    common::git(&other, &["init", "-q", "-b", "main"]);
    std::fs::write(other.join("a.py"), "def a():\n    return 1\n").unwrap();
    common::git(&other, &["add", "-A"]);
    common::git(&other, &["commit", "-q", "-m", "a"]);
    let (code, out) = env.cli(&["init", other.to_str().unwrap()]);
    assert_eq!(code, 0, "{out}");
    std::fs::remove_dir_all(&other).unwrap();
    let mut mcp = env.mcp();
    let (text, _) = mcp.call("list_projects", json!({}));
    let doc: Value = serde_json::from_str(&text).unwrap();
    let present: Vec<(String, bool)> = doc["projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["name"].as_str().unwrap().to_string(),
                p["present"].as_bool().unwrap(),
            )
        })
        .collect();
    let mut present = present;
    present.sort();
    assert_eq!(
        present,
        [("other".to_string(), false), ("qa".to_string(), true)]
    );
    let (_, stdout) = env.cli(&["--json", "projects"]);
    assert_eq!(text, stdout.trim_end());
}

#[test]
fn malformed_arguments_are_tool_errors() {
    let env = common::Env::new(true);
    let mut mcp = env.mcp();
    for (tool, args, message) in [
        ("search", json!({}), "missing required argument 'query'"),
        (
            "search",
            json!({"query": 5}),
            "'query' must be a string or a list of strings",
        ),
        (
            "search",
            json!({"query": ["a", 1]}),
            "'query' must be a string or a list of strings",
        ),
        (
            "search",
            json!({"query": "a", "top_k": "5"}),
            "'top_k' must be an integer",
        ),
        (
            "search",
            json!({"query": "a", "top_k": 2.5}),
            "'top_k' must be an integer",
        ),
        (
            "search",
            json!({"query": "a", "force_fresh": 1}),
            "'force_fresh' must be a boolean",
        ),
        (
            "context",
            json!({"symbol": "a", "confidence_min": "x"}),
            "'confidence_min' must be a number",
        ),
        (
            "context",
            json!({"symbol": ["a"]}),
            "'symbol' must be a string",
        ),
        (
            "map",
            json!({"focus": "auth"}),
            "'focus' must be a list of strings",
        ),
        (
            "grep",
            json!({"pattern": "a", "literal": "yes"}),
            "'literal' must be a boolean",
        ),
        (
            "expand",
            json!({"target": "a", "detail": 3}),
            "'detail' must be a string",
        ),
        (
            "detect_changes",
            json!({"project": 3}),
            "'project' must be a string",
        ),
    ] {
        let (text, is_error) = mcp.call(tool, args.clone());
        assert!(is_error, "{tool} {args}: {text}");
        assert_eq!(
            text,
            format!("VEX ERROR (invalid_request): {message}"),
            "{tool} {args}"
        );
    }
    // Whole numbers given as floats are integers, as pydantic takes them.
    let (_, is_error) = mcp.call("search", json!({"query": "hub", "top_k": 2.0}));
    assert!(!is_error);
    let r = mcp.request("tools/call", json!({"name": "remember", "arguments": {}}));
    assert!(
        r["error"].is_object(),
        "an unknown tool is a protocol error: {r}"
    );
}

#[test]
fn a_missing_home_or_index_still_answers() {
    let env = common::Env::new(false);
    let mut mcp = env.mcp();
    let (text, _) = mcp.call("map", json!({}));
    assert!(text.starts_with("VEX ERROR (no_index): "), "{text}");
    let (text, _) = mcp.call("list_projects", json!({}));
    assert_eq!(text, "{\"schema\":1,\"projects\":[]}");
    let mut homeless = env.mcp_without_home();
    let (text, _) = homeless.call("search", json!({"query": "x"}));
    assert_eq!(
        text,
        "VEX ERROR (no_home): neither VEX_HOME nor HOME is set"
    );
}
