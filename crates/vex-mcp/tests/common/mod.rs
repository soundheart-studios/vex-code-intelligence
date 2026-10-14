//! Drive the real `vex` binary: `vex mcp` over stdio JSON-RPC, and the CLI
//! for comparison. The binary belongs to vex-cli, so it is built here once
//! (cargo releases its lock before running tests) into the same target
//! directory as this test.

#![allow(dead_code)]

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::OnceLock;

/// The `vex` binary, built on first use.
pub fn vex_bin() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let exe = std::env::current_exe().unwrap();
        let profile_dir = exe.parent().unwrap().parent().unwrap().to_path_buf();
        let target_dir = profile_dir.parent().unwrap();
        let mut build = Command::new(env!("CARGO"));
        build
            .args([
                "build",
                "-q",
                "-p",
                "vex-cli",
                "--bin",
                "vex",
                "--target-dir",
            ])
            .arg(target_dir);
        if profile_dir.file_name().is_some_and(|n| n == "release") {
            build.arg("--release");
        }
        let status = build.status().unwrap();
        assert!(status.success(), "building the vex binary failed");
        profile_dir.join(format!("vex{}", std::env::consts::EXE_SUFFIX))
    })
}

pub fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .env("GIT_AUTHOR_NAME", "vex")
        .env("GIT_AUTHOR_EMAIL", "vex@example.invalid")
        .env("GIT_COMMITTER_NAME", "vex")
        .env("GIT_COMMITTER_EMAIL", "vex@example.invalid")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap()
        .status;
    assert!(status.success(), "git {args:?}");
}

fn copy_tree(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            std::fs::create_dir_all(&dest).unwrap();
            copy_tree(&entry.path(), &dest);
        } else {
            std::fs::write(dest, std::fs::read(entry.path()).unwrap()).unwrap();
        }
    }
}

/// A VEX_HOME and a committed fixture project (qa base plus the graph
/// overlay), indexed by `vex init` unless `index` is false.
pub struct Env {
    pub home: vex_test_support::TestHome,
    pub root: PathBuf,
}

impl Env {
    pub fn new(index: bool) -> Env {
        let home = vex_test_support::TestHome::new();
        let root = home.vex_home().join("qa");
        std::fs::create_dir_all(&root).unwrap();
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        copy_tree(&manifest.join("../../tests/fixtures/repos/qa/base"), &root);
        copy_tree(
            &manifest.join("../vex-app/tests/fixtures/graph/overlay"),
            &root,
        );
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "base"]);
        let root = root.canonicalize().unwrap();
        let env = Env { home, root };
        if index {
            let (code, out) = env.cli(&["init"]);
            assert_eq!(code, 0, "vex init: {out}");
        }
        env
    }

    pub fn vex_home(&self) -> PathBuf {
        self.home.vex_home().join("vexhome")
    }

    fn command(&self) -> Command {
        let mut c = Command::new(vex_bin());
        c.current_dir(&self.root)
            .env("VEX_EMBED_BACKEND", "none")
            .env("VEX_DAEMON_AUTOSTART", "never")
            .env("VEX_HOME", self.vex_home())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }

    /// `vex <args>` in the project: exit code and stdout.
    pub fn cli(&self, args: &[&str]) -> (i32, String) {
        let out = self.command().args(args).output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8(out.stdout).unwrap(),
        )
    }

    /// A `vex mcp` session in the project, initialised.
    pub fn mcp(&self) -> Session {
        Session::start(self.command())
    }

    /// A `vex mcp` session on protocol 2026-07-28: no `initialize`; every
    /// request carries the protocol version in its own `_meta`, as current
    /// Claude Code sends it.
    pub fn mcp_inline(&self) -> Session {
        Session::spawn(self.command())
    }

    /// A `vex mcp` session with neither VEX_HOME nor HOME set.
    pub fn mcp_without_home(&self) -> Session {
        let mut c = self.command();
        c.env_remove("VEX_HOME").env_remove("HOME");
        Session::start(c)
    }
}

impl Session {
    fn spawn(mut command: Command) -> Session {
        let mut child = command
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Session {
            child,
            stdin,
            stdout,
            next_id: 1,
        }
    }

    fn start(command: Command) -> Session {
        let mut s = Session::spawn(command);
        let init = s.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "vex-tests", "version": "0"}
            }),
        );
        assert_eq!(init["result"]["serverInfo"]["name"], "vex", "{init}");
        s.notify("notifications/initialized");
        s
    }
}

pub struct Session {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Session {
    fn send(&mut self, message: &Value) {
        writeln!(self.stdin, "{message}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn notify(&mut self, method: &str) {
        self.send(&json!({"jsonrpc": "2.0", "method": method}));
    }

    /// A request and its response (notifications in between are skipped).
    pub fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let mut line = String::new();
            assert!(
                self.stdout.read_line(&mut line).unwrap() > 0,
                "vex mcp closed"
            );
            let msg: Value = serde_json::from_str(&line).unwrap();
            if msg["id"] == json!(id) {
                return msg;
            }
        }
    }

    /// `tools/call`: the text and isError.
    pub fn call(&mut self, name: &str, arguments: Value) -> (String, bool) {
        let r = self.request("tools/call", json!({"name": name, "arguments": arguments}));
        let result = &r["result"];
        assert!(result.is_object(), "tools/call {name} failed: {r}");
        (
            result["content"][0]["text"].as_str().unwrap().to_string(),
            result["isError"].as_bool().unwrap_or(false),
        )
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
