//! `vex init --agent claude|codex|all` through the binary: the harness
//! config gains a bare `vex mcp` server and nothing else changes, re-runs
//! are no-ops, unsafe configs are refused untouched, and the installed
//! vex-code-context skill names only tools `vex mcp` advertises.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use vex_test_support::TestHome;

struct Env {
    home: TestHome,
    root: PathBuf,
}

impl Env {
    /// A committed one-file project, not yet registered.
    fn new() -> Env {
        let home = TestHome::new();
        let root = home.vex_home().join("proj");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.py"), "def helper(x):\n    return x\n").unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["add", "-A"],
            &["commit", "-q", "-m", "init"],
        ] {
            let ok = Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@localhost"])
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .current_dir(&root)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?}");
        }
        let root = root.canonicalize().unwrap();
        Env { home, root }
    }

    fn command(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_vex"));
        c.current_dir(&self.root)
            .env("VEX_EMBED_BACKEND", "none")
            .env("VEX_DAEMON_AUTOSTART", "never")
            .env("VEX_HOME", self.home.vex_home().join("vexhome"));
        c
    }

    /// `vex <args>`: exit code, stdout, stderr.
    fn vex(&self, args: &[&str]) -> (i32, String, String) {
        let out = self.command().args(args).output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8(out.stdout).unwrap(),
            String::from_utf8(out.stderr).unwrap(),
        )
    }

    /// `vex --json init --agent ...`: exit code and the parsed report.
    fn init(&self, agents: &[&str]) -> (i32, Value) {
        let mut args = vec!["--json", "init"];
        for a in agents {
            args.extend(["--agent", a]);
        }
        let (code, out, err) = self.vex(&args);
        let doc = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out} {err}"));
        (code, doc)
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.path(rel)).unwrap()
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.path(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
}

fn action<'a>(report: &'a Value, agent: &str, key: &str) -> &'a str {
    report["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["agent"] == agent)
        .unwrap_or_else(|| panic!("no {agent} in {report}"))[key]
        .as_str()
        .unwrap()
}

const MCP_JSON: &str = r#"{
  "$schema": "https://example.invalid/mcp.schema.json",
  "mcpServers": {
    "zeta": {
      "args": ["serve", "--port",   "9"],
      "command": "zeta-server",
      "env": {"B": "2", "A": "1"}
    },
    "alpha": {"command": "alpha"}
  },
  "other": [1, 2,   3]
}
"#;

#[test]
fn claude_adds_a_vex_server_and_keeps_every_other_entry_verbatim() {
    let e = Env::new();
    e.write(".mcp.json", MCP_JSON);
    let (code, report) = e.init(&["claude"]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(action(&report, "claude", "config_action"), "added");
    let text = e.read(".mcp.json");
    let doc: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        doc["mcpServers"]["vex"],
        json!({"command": "vex", "args": ["mcp"]})
    );
    // Other servers and keys keep their exact text, in their order.
    for kept in [
        r#""$schema": "https://example.invalid/mcp.schema.json""#,
        r#""zeta": {
      "args": ["serve", "--port",   "9"],
      "command": "zeta-server",
      "env": {"B": "2", "A": "1"}
    }"#,
        r#""alpha": {"command": "alpha"}"#,
        r#""other": [1, 2,   3]"#,
    ] {
        assert!(text.contains(kept), "lost {kept} in\n{text}");
    }
    let order: Vec<usize> = [
        "\"$schema\"",
        "\"zeta\"",
        "\"alpha\"",
        "\"vex\"",
        "\"other\"",
    ]
    .iter()
    .map(|k| text.find(k).unwrap())
    .collect();
    assert!(
        order.windows(2).all(|w| w[0] < w[1]),
        "order changed:\n{text}"
    );

    // A re-run changes nothing.
    let (code, report) = e.init(&["claude"]);
    assert_eq!(code, 0);
    assert_eq!(action(&report, "claude", "config_action"), "unchanged");
    assert_eq!(e.read(".mcp.json"), text);

    // A drifted vex entry is put back in place.
    e.write(
        ".mcp.json",
        &text.replace(r#""command": "vex""#, r#""command": "/opt/old/vex""#),
    );
    let (_, report) = e.init(&["claude"]);
    assert_eq!(action(&report, "claude", "config_action"), "updated");
    assert_eq!(e.read(".mcp.json"), text);
}

#[test]
fn claude_creates_the_config_and_names_bare_vex() {
    let e = Env::new();
    let (code, report) = e.init(&["claude"]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(action(&report, "claude", "config_action"), "added");
    assert_eq!(
        e.read(".mcp.json"),
        "{\n  \"mcpServers\": {\n    \"vex\": {\n      \"command\": \"vex\",\n      \"args\": [\n        \"mcp\"\n      ]\n    }\n  }\n}\n"
    );
    let (code, report) = e.init(&["codex"]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(
        e.read(".codex/config.toml"),
        "# >>> vex: generated by `vex init --agent codex`; edits inside this block are replaced\n\
         [mcp_servers.vex]\ncommand = \"vex\"\nargs = [\"mcp\"]\n# <<< vex\n"
    );
}

#[cfg(unix)]
#[test]
fn an_unreadable_config_is_never_overwritten() {
    use std::os::unix::fs::PermissionsExt;
    let e = Env::new();
    e.write(".mcp.json", MCP_JSON);
    let path = e.path(".mcp.json");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    let (code, report) = e.init(&["claude"]);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(code, 1, "{report}");
    assert_eq!(report["error"], "agent_config", "{report}");
    assert_eq!(e.read(".mcp.json"), MCP_JSON);
}

#[test]
fn a_no_op_rerun_does_not_rewrite_any_file() {
    let e = Env::new();
    e.write(".codex/config.toml", CONFIG_TOML);
    let (code, _) = e.init(&["all"]);
    assert_eq!(code, 0);
    let stamp = |rel: &str| std::fs::metadata(e.path(rel)).unwrap().modified().unwrap();
    let files = [
        ".mcp.json",
        ".codex/config.toml",
        ".claude/skills/vex-code-context/SKILL.md",
        ".agents/skills/vex-code-context/references/freshness.md",
    ];
    let before: Vec<_> = files.iter().map(|f| stamp(f)).collect();
    std::thread::sleep(std::time::Duration::from_millis(50));
    let (_, report) = e.init(&["all"]);
    for agent in ["claude", "codex"] {
        assert_eq!(action(&report, agent, "config_action"), "unchanged");
        assert_eq!(action(&report, agent, "skill_action"), "unchanged");
    }
    let after: Vec<_> = files.iter().map(|f| stamp(f)).collect();
    assert_eq!(before, after, "a no-op re-run rewrote a file");
}

#[test]
fn an_unparseable_config_is_refused_and_left_alone() {
    let e = Env::new();
    for (rel, text, agent) in [
        (".mcp.json", "{ \"mcpServers\": [ nope", "claude"),
        (".mcp.json", "[1, 2]", "claude"),
        (".mcp.json", "{\"mcpServers\": [1]}", "claude"),
        (".codex/config.toml", "[mcp_servers\nbroken = ", "codex"),
    ] {
        e.write(rel, text);
        let (code, report) = e.init(&[agent]);
        assert_eq!(code, 1, "{rel}: {report}");
        assert_eq!(report["error"], "agent_config", "{report}");
        assert!(
            report["message"]
                .as_str()
                .unwrap()
                .contains("left unchanged"),
            "{report}"
        );
        assert_eq!(e.read(rel), text, "{rel} was modified");
        std::fs::remove_file(e.path(rel)).unwrap();
    }
}

const CONFIG_TOML: &str = "model = \"o4\"\n\n[mcp_servers.other]\ncommand = \"other\"\n";

#[test]
fn codex_appends_a_fenced_block_once_and_keeps_the_rest() {
    let e = Env::new();
    e.write(".codex/config.toml", CONFIG_TOML);
    let (code, report) = e.init(&["codex"]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(action(&report, "codex", "config_action"), "added");
    let text = e.read(".codex/config.toml");
    assert!(text.starts_with(CONFIG_TOML), "{text}");
    let table: toml::Table = text.parse().unwrap();
    assert_eq!(table["model"].as_str(), Some("o4"));
    assert_eq!(
        table["mcp_servers"]["other"]["command"].as_str(),
        Some("other")
    );
    let vex = &table["mcp_servers"]["vex"];
    assert_eq!(vex["command"].as_str(), Some("vex"));
    assert_eq!(vex["args"].as_array().unwrap().len(), 1);
    assert_eq!(vex["args"][0].as_str(), Some("mcp"));

    let (_, report) = e.init(&["codex"]);
    assert_eq!(action(&report, "codex", "config_action"), "unchanged");
    assert_eq!(e.read(".codex/config.toml"), text);

    // Edits inside the fence are replaced; text around it survives.
    let edited = text.replace("args = [\"mcp\"]", "args = [\"serve\"]") + "\n[extra]\nk = 1\n";
    e.write(".codex/config.toml", &edited);
    let (_, report) = e.init(&["codex"]);
    assert_eq!(action(&report, "codex", "config_action"), "updated");
    assert_eq!(e.read(".codex/config.toml"), text + "\n[extra]\nk = 1\n");
}

#[test]
fn codex_separates_its_block_from_a_config_without_a_final_newline() {
    let e = Env::new();
    e.write(".codex/config.toml", "model = \"o4\"");
    let (code, report) = e.init(&["codex"]);
    assert_eq!(code, 0, "{report}");
    let text = e.read(".codex/config.toml");
    assert!(text.starts_with("model = \"o4\"\n\n# >>> vex"), "{text}");
}

#[test]
fn codex_keeps_a_vex_server_written_by_hand() {
    let e = Env::new();
    let by_hand = "[mcp_servers.vex]\ncommand = \"/usr/local/bin/vex\"\nargs = [\"mcp\"]\n";
    e.write(".codex/config.toml", by_hand);
    let (code, report) = e.init(&["codex"]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(action(&report, "codex", "config_action"), "kept");
    assert_eq!(e.read(".codex/config.toml"), by_hand);
}

/// The bundle as shipped in resources/vex-code-context.
fn shipped() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources/vex-code-context");
    let mut files = Vec::new();
    for rel in [
        "SKILL.md",
        "references/change-safety.md",
        "references/freshness.md",
        "references/host-setup.md",
        "references/reading-results.md",
    ] {
        files.push((
            rel.to_string(),
            std::fs::read_to_string(dir.join(rel)).unwrap(),
        ));
    }
    files
}

#[test]
fn the_skill_lands_where_each_harness_looks_and_respects_a_users_own() {
    let e = Env::new();
    let (code, report) = e.init(&["all"]);
    assert_eq!(code, 0, "{report}");
    for (agent, dir) in [
        ("claude", ".claude/skills/vex-code-context"),
        ("codex", ".agents/skills/vex-code-context"),
    ] {
        assert_eq!(action(&report, agent, "skill_action"), "added");
        for (rel, body) in shipped() {
            assert_eq!(e.read(&format!("{dir}/{rel}")), body, "{dir}/{rel}");
        }
    }
    // A managed file that drifted is restored; a re-run is otherwise a no-op.
    e.write(
        ".claude/skills/vex-code-context/references/freshness.md",
        "stale\n",
    );
    let (_, report) = e.init(&["claude", "claude"]);
    assert_eq!(
        report["agents"].as_array().unwrap().len(),
        1,
        "duplicates collapse"
    );
    assert_eq!(action(&report, "claude", "skill_action"), "updated");
    let (_, report) = e.init(&["claude"]);
    assert_eq!(action(&report, "claude", "skill_action"), "unchanged");
    // A SKILL.md vex did not write is never replaced.
    e.write(".agents/skills/vex-code-context/SKILL.md", "my own notes\n");
    let (_, report) = e.init(&["codex"]);
    assert_eq!(action(&report, "codex", "skill_action"), "kept");
    assert_eq!(
        e.read(".agents/skills/vex-code-context/SKILL.md"),
        "my own notes\n"
    );
}

#[test]
fn without_agent_init_writes_nothing_into_the_repo_and_says_how() {
    let e = Env::new();
    let (code, _, err) = e.vex(&["init"]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("vex init --agent claude"), "{err}");
    for rel in [".mcp.json", ".codex", ".claude", ".agents"] {
        assert!(!e.path(rel).exists(), "{rel} written without --agent");
    }
    let (_, report) = e.init(&[]);
    assert_eq!(report["agents"], json!([]));
    let (code, _, err) = e.vex(&["init", "--agent", "cursor"]);
    assert_eq!(code, 1, "an unknown agent is a usage error: {err}");
}

#[test]
fn a_project_already_wired_gets_no_tip_and_keeps_its_config() {
    let e = Env::new();
    let (code, _) = e.init(&["codex"]);
    assert_eq!(code, 0);
    let config = e.read(".codex/config.toml");
    let (code, _, err) = e.vex(&["init"]);
    assert_eq!(code, 0, "{err}");
    assert!(!err.contains("--agent"), "tip for a wired project: {err}");
    assert_eq!(e.read(".codex/config.toml"), config);
}

/// The tool names `vex mcp` advertises, over stdio JSON-RPC.
fn advertised_tools(e: &Env) -> Vec<String> {
    let mut child = e
        .command()
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut rpc = |msg: Value, want: Option<u64>| -> Option<Value> {
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
        let id = want?;
        loop {
            let mut line = String::new();
            assert!(stdout.read_line(&mut line).unwrap() > 0);
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == json!(id) {
                return Some(v);
            }
        }
    };
    rpc(
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "t", "version": "0"}}}),
        Some(1),
    );
    rpc(
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        None,
    );
    let listed = rpc(
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
        Some(2),
    )
    .unwrap();
    let _ = child.kill();
    let _ = child.wait();
    listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn the_skill_names_only_tools_vex_mcp_advertises() {
    let e = Env::new();
    let tools = advertised_tools(&e);
    assert_eq!(tools.len(), 9, "{tools:?}");
    // Every MCP tool name Cortex has ever offered, so a stale reference to
    // one vex dropped (memory, deprecated tools) is caught.
    let known: Vec<&str> = [
        "list_projects",
        "search",
        "context",
        "impact",
        "map",
        "expand",
        "grep",
        "detect_changes",
        "reindex",
        "remember",
        "recall",
        "supersede",
        "investigate",
        "expand_many",
        "traverse",
    ]
    .to_vec();
    let mut mentioned = std::collections::BTreeSet::new();
    for (rel, body) in shipped() {
        // Backticked words, and calls written as `name(`.
        for part in body.split('`').skip(1).step_by(2) {
            let word = part.split('(').next().unwrap();
            let is_call = part.contains('(')
                && !word.is_empty()
                && word.chars().all(|c| c.is_ascii_lowercase() || c == '_');
            if known.contains(&word) || is_call {
                assert!(
                    tools.iter().any(|t| t == word),
                    "{rel} names `{word}`, which vex mcp does not advertise"
                );
                mentioned.insert(word.to_string());
            }
        }
        assert!(
            !body.to_lowercase().contains("cortex"),
            "{rel} mentions Cortex"
        );
    }
    for tool in &tools {
        assert!(
            mentioned.contains(tool),
            "the skill never mentions `{tool}`"
        );
    }
}
