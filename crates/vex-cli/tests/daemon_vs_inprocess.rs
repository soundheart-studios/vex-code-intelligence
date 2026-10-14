//! The CLI as a daemon client. Every query command answers byte for byte
//! the same through the daemon as in-process; a daemon that cannot be
//! spawned costs nothing but the wait (the command runs in-process); and
//! `vex mcp` tool calls are served by the daemon when it runs. Routing is
//! proved by the daemon's own request counter, not inferred.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use vex_daemon::client::{self, Connect};
use vex_daemon::protocol::{Request, Response};
use vex_test_support::TestHome;

struct Env {
    home: TestHome,
    root: PathBuf,
}

const FILES: &[(&str, &str)] = &[
    (
        "app/config.py",
        "import json\n\n\ndef parse_config(path):\n    with open(path) as f:\n        return json.load(f)\n\n\nclass Loader:\n    def load(self, name):\n        return parse_config(name)\n",
    ),
    (
        "app/auth.py",
        "from app.config import parse_config\n\n\ndef check_token(token):\n    return token.startswith('vex_')\n\n\ndef login(user, token):\n    if not check_token(token):\n        raise ValueError('bad token')\n    return parse_config(user)\n",
    ),
];

fn git(root: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@localhost",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?}");
}

impl Env {
    fn new() -> Env {
        let home = TestHome::new();
        let root = home.vex_home().join("proj");
        for (rel, text) in FILES {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "base"]);
        let env = Env {
            root: root.canonicalize().unwrap(),
            home,
        };
        let (code, _, err) = env.vex("never", &["init"]);
        assert_eq!(code, Some(0), "{err}");
        // Uncommitted work for `changes` to report, indexed so the index is
        // fresh: a stale answer differs by design (the daemon dispatches a
        // refresh, the in-process CLI has no dispatcher).
        std::fs::write(
            env.root.join("app/config.py"),
            FILES[0].1.replace("json.load(f)", "json.loads(f.read())"),
        )
        .unwrap();
        let (code, _, err) = env.vex("never", &["index"]);
        assert_eq!(code, Some(0), "{err}");
        env
    }

    fn vex_home(&self) -> PathBuf {
        self.home.vex_home().join("vexhome")
    }

    fn command(&self, autostart: &str) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_vex"));
        c.current_dir(&self.root)
            .env("VEX_HOME", self.vex_home())
            .env("VEX_SOCKET", self.home.socket())
            .env("VEX_EMBED_BACKEND", "hash")
            .env("VEX_DAEMON_AUTOSTART", autostart)
            .env("VEX_DAEMON_IDLE_EXIT_SECS", "120")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }

    fn vex(&self, autostart: &str, args: &[&str]) -> (Option<i32>, String, String) {
        let out = self.command(autostart).args(args).output().unwrap();
        (
            out.status.code(),
            String::from_utf8(out.stdout).unwrap(),
            String::from_utf8(out.stderr).unwrap(),
        )
    }

    fn daemon(&self) -> Option<vex_daemon::client::Conn> {
        // The handshake must match the CLI's: same version and settings.
        let version = self.handshake();
        match client::connect(self.home.socket(), &version) {
            Connect::Ready(c) => Some(c),
            _ => None,
        }
    }

    fn requests(&self) -> u64 {
        let mut c = self.daemon().expect("daemon running");
        match c.request(&Request::Status).unwrap() {
            Response::Status { status } => status["requests"].as_u64().unwrap(),
            other => panic!("{other:?}"),
        }
    }

    fn stop_daemon(&self) {
        if let Some(mut c) = self.daemon() {
            let _ = c.request(&Request::Shutdown);
        }
        let start = Instant::now();
        while self.home.socket().exists() && start.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        self.stop_daemon();
    }
}

impl Env {
    /// The handshake the CLI greets the daemon with under this env.
    fn handshake(&self) -> String {
        let out = self
            .command("never")
            .args(["--json", "--version"])
            .output()
            .unwrap();
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        v["daemon_handshake"]
            .as_str()
            .expect("daemon_handshake")
            .to_string()
    }
}

fn redact(text: &str) -> String {
    let mut out = text.to_string();
    for key in ["took_ms", "selector_ms", "response_bytes", "index_age_s"] {
        let needle = format!("\"{key}\":");
        let mut result = String::new();
        let mut rest = out.as_str();
        while let Some(i) = rest.find(&needle) {
            result.push_str(&rest[..i + needle.len()]);
            rest = &rest[i + needle.len()..];
            let end = rest.find([',', '}']).unwrap_or(rest.len());
            result.push_str("<redacted>");
            rest = &rest[end..];
        }
        result.push_str(rest);
        out = result;
    }
    out
}

const COMMANDS: &[&[&str]] = &[
    &["--json", "search", "parse_config"],
    &["--json", "search", "--list", "parse config", "token check"],
    &["--json", "expand", "parse_config"],
    &[
        "--json",
        "expand",
        "parse_config",
        "login",
        "--detail",
        "signature",
    ],
    &["--json", "context", "parse_config"],
    &["--json", "impact", "parse_config"],
    &["--json", "impact", "parse_config", "--direction", "both"],
    &["--json", "grep", "def "],
    &["--json", "changes"],
    &["--json", "map"],
    &["--json", "status"],
    &["--json", "projects"],
    &["--json", "search", "zzqqxxnotaword"],
    &["--json", "expand", "no_such_symbol"],
    &["--json", "search", "parse_config", "--root", "app"],
    &["--json", "status", "app"],
    &["--json", "status", "missing-dir"],
    &["--json", "grep", "def", "--root", "missing-dir"],
    &["search", "parse_config"],
    &["grep", "token"],
    &["status"],
];

#[test]
fn every_query_command_is_byte_identical_via_the_daemon() {
    let env = Env::new();
    let mut in_process = Vec::new();
    for args in COMMANDS {
        in_process.push(env.vex("never", args));
    }
    // The first routed command spawns the daemon.
    let mut via_daemon = Vec::new();
    for args in COMMANDS {
        via_daemon.push(env.vex("always", args));
    }
    // A path that does not exist is reported in-process (so the message
    // keeps the user's spelling); everything else reaches the daemon.
    let local_only = COMMANDS
        .iter()
        .filter(|a| a.contains(&"missing-dir"))
        .count();
    let served = env.requests();
    assert_eq!(
        served,
        (COMMANDS.len() - local_only) as u64,
        "commands that reached the daemon"
    );
    for ((args, local), remote) in COMMANDS.iter().zip(&in_process).zip(&via_daemon) {
        assert_eq!(local.0, remote.0, "exit code differs for {args:?}");
        assert_eq!(
            redact(&local.1),
            redact(&remote.1),
            "stdout differs for {args:?}"
        );
        assert_eq!(local.2, remote.2, "stderr differs for {args:?}");
    }
    // A command that writes (an index refresh) goes through it too.
    let before = env.requests();
    let (code, _, err) = env.vex("always", &["--json", "index"]);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(env.requests(), before + 1);
}

#[test]
fn a_routed_command_resolves_paths_against_the_clients_directory() {
    let env = Env::new();
    // Start the daemon from the project root.
    let (code, _, _) = env.vex("always", &["--json", "status"]);
    assert_eq!(code, Some(0));
    // A client elsewhere (no project there) must not get the root's answer.
    let elsewhere = env.home.vex_home().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let run = |autostart: &str| {
        let out = env
            .command(autostart)
            .current_dir(&elsewhere)
            .args(["--json", "status"])
            .output()
            .unwrap();
        (out.status.code(), String::from_utf8(out.stdout).unwrap())
    };
    let before = env.requests();
    let (local, remote) = (run("never"), run("always"));
    assert_eq!(env.requests(), before + 1, "the status call was not routed");
    assert_eq!(local, remote);
    assert_eq!(local.0, Some(2), "no index there: {}", local.1);
}

#[test]
fn a_client_with_other_settings_supersedes_the_running_daemon() {
    let env = Env::new();
    let (code, _, _) = env.vex("always", &["--json", "status"]);
    assert_eq!(code, Some(0));
    let old = env.daemon().expect("the first daemon").pid;
    // Another embedding backend: another handshake. The old daemon drains
    // and this client's own takes over and answers.
    let keyword = |autostart: &str| {
        let out = env
            .command(autostart)
            .env("VEX_EMBED_BACKEND", "none")
            .args(["--json", "search", "parse_config"])
            .output()
            .unwrap();
        (out.status.code(), String::from_utf8(out.stdout).unwrap())
    };
    let local = keyword("never");
    let remote = keyword("always");
    assert_eq!(redact(&local.1), redact(&remote.1));
    let version = {
        let out = env
            .command("never")
            .env("VEX_EMBED_BACKEND", "none")
            .args(["--json", "--version"])
            .output()
            .unwrap();
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        v["daemon_handshake"].as_str().unwrap().to_string()
    };
    let mut c = match client::connect(env.home.socket(), &version) {
        Connect::Ready(c) => c,
        other => panic!("no daemon with the new settings: {other:?}"),
    };
    assert_ne!(c.pid, old, "the old daemon still serves");
    match c.request(&Request::Status).unwrap() {
        Response::Status { status } => assert_eq!(status["requests"], 1),
        other => panic!("{other:?}"),
    }
    let _ = c.request(&Request::Shutdown);
}

#[test]
fn with_the_daemon_unspawnable_commands_still_succeed_in_process() {
    let env = Env::new();
    let (code_local, out_local, _) = env.vex("never", &["--json", "search", "parse_config"]);
    // A socket in a directory that does not exist: the spawned daemon
    // cannot bind, so nothing ever answers.
    let start = Instant::now();
    let out = env
        .command("always")
        .env(
            "VEX_SOCKET",
            env.home.vex_home().join("no/such/dir/vex.sock"),
        )
        .args(["--json", "search", "parse_config"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), code_local);
    assert_eq!(
        redact(&String::from_utf8(out.stdout).unwrap()),
        redact(&out_local)
    );
    assert!(
        start.elapsed() < Duration::from_secs(8),
        "the fallback waited {:?}",
        start.elapsed()
    );
    // The binary itself missing is the same, and costs no wait at all.
    let start = Instant::now();
    let out = env
        .command("always")
        .env("VEX_DAEMON_EXE", env.home.vex_home().join("no-such-vex"))
        .args(["--json", "status"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "a missing daemon binary still waited {:?}",
        start.elapsed()
    );
}

fn mcp_search(env: &Env, autostart: &str) -> String {
    let mut child = env
        .command(autostart)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut send = |m: Value| {
        writeln!(stdin, "{m}").unwrap();
        stdin.flush().unwrap();
    };
    let mut read = |id: u64| -> Value {
        loop {
            let mut line = String::new();
            assert!(stdout.read_line(&mut line).unwrap() > 0, "vex mcp closed");
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == id {
                return v;
            }
        }
    };
    send(
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {},
        "clientInfo": {"name": "t", "version": "0"}}}),
    );
    read(1);
    send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    send(
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
        "name": "search", "arguments": {"query": "parse_config"}}}),
    );
    let v = read(2);
    let _ = child.kill();
    let _ = child.wait();
    v["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn vex_mcp_is_served_by_the_daemon_when_it_runs() {
    let env = Env::new();
    let local = mcp_search(&env, "never");
    // Start the daemon with an ordinary command, then count.
    let (code, _, err) = env.vex("always", &["--json", "status"]);
    assert_eq!(code, Some(0), "{err}");
    let before = env.requests();
    let remote = mcp_search(&env, "always");
    assert_eq!(
        env.requests(),
        before + 1,
        "the MCP call did not reach the daemon"
    );
    assert_eq!(redact(&local), redact(&remote));
}

#[test]
fn a_stale_query_through_the_daemon_refreshes_in_the_background() {
    let env = Env::new();
    std::fs::write(
        env.root.join("app/late.py"),
        "def arrived_late():\n    return 1\n",
    )
    .unwrap();
    // The daemon may already have caught up (it refreshes changed projects
    // as it starts); if not, the stale answer says a refresh is under way.
    let (code, out, _) = env.vex("always", &["--json", "search", "arrived_late"]);
    if code == Some(3) {
        let doc: Value = serde_json::from_str(&out).unwrap();
        let reindex = doc["freshness"]["reindex"].as_str().unwrap_or("");
        assert!(
            matches!(reindex, "dispatched" | "in_flight" | "cooldown"),
            "{out}"
        );
    } else {
        assert_eq!(code, Some(0), "{out}");
    }
    let start = Instant::now();
    loop {
        let (code, out, _) = env.vex("always", &["--json", "search", "arrived_late"]);
        if code == Some(0) {
            assert!(out.contains("app/late.py"), "{out}");
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "the background refresh never landed: {out}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}
