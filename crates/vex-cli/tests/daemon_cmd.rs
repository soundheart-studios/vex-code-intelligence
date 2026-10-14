//! `vex daemon status / start / stop / restart`: inspecting and controlling
//! the daemon (pid, version, uptime, watched projects, loaded models).

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use vex_test_support::TestHome;

struct Env {
    home: TestHome,
    root: PathBuf,
}

impl Env {
    fn new() -> Env {
        let home = TestHome::new();
        let root = home.vex_home().join("proj");
        std::fs::create_dir_all(root.join("app")).unwrap();
        std::fs::write(root.join("app/a.py"), "def alpha():\n    return 1\n").unwrap();
        let env = Env { home, root };
        let (code, _, err) = env.vex(&["init"]);
        assert_eq!(code, Some(0), "{err}");
        env.root_canonical();
        env
    }

    fn root_canonical(&self) -> PathBuf {
        self.root.canonicalize().unwrap()
    }

    fn vex(&self, args: &[&str]) -> (Option<i32>, Value, String) {
        self.vex_in(&self.root, args)
    }

    fn vex_in(&self, cwd: &Path, args: &[&str]) -> (Option<i32>, Value, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_vex"))
            .args(args)
            .current_dir(cwd)
            .env("VEX_HOME", self.home.vex_home().join("vexhome"))
            .env("VEX_SOCKET", self.home.socket())
            .env("VEX_EMBED_BACKEND", "hash")
            .env("VEX_DAEMON_AUTOSTART", "when-needed")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        (
            out.status.code(),
            serde_json::from_slice(&out.stdout).unwrap_or(Value::Null),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = self.vex(&["daemon", "stop"]);
    }
}

#[test]
fn daemon_status_reports_pid_version_watched_projects_and_models() {
    let env = Env::new();
    let (code, doc, _) = env.vex(&["--json", "daemon", "status"]);
    assert_eq!(code, Some(0));
    assert_eq!(doc["schema"], 1);
    assert_eq!(doc["running"], false, "{doc}");

    let (code, doc, err) = env.vex(&["--json", "daemon", "start"]);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(doc["running"], true, "{doc}");
    let pid = doc["pid"].as_u64().unwrap();

    // A query makes the daemon watch the project (when-needed: the running
    // daemon serves it).
    let (code, _, _) = env.vex(&["--json", "search", "alpha"]);
    assert_eq!(code, Some(0));
    let (code, doc, _) = env.vex(&["--json", "daemon", "status"]);
    assert_eq!(code, Some(0));
    assert_eq!(doc["running"], true);
    assert_eq!(doc["pid"].as_u64(), Some(pid));
    let (_, version, _) = env.vex(&["--json", "--version"]);
    assert_eq!(doc["version"], version["daemon_handshake"]);
    assert_eq!(doc["handshake_matches"], true);
    assert!(doc["uptime_s"].as_f64().unwrap() >= 0.0);
    assert!(doc["requests"].as_u64().unwrap() >= 1);
    let watched: Vec<String> = serde_json::from_value(doc["watched"].clone()).unwrap();
    assert_eq!(watched, vec![env.root_canonical().display().to_string()]);
    assert!(doc["models"].is_array(), "{doc}");
    assert!(doc["socket"].as_str().unwrap().ends_with(".sock"));

    // Another build or settings sees the daemon without draining it.
    let other = Command::new(env!("CARGO_BIN_EXE_vex"))
        .args(["--json", "daemon", "status"])
        .current_dir(&env.root)
        .env("VEX_HOME", env.home.vex_home().join("vexhome"))
        .env("VEX_SOCKET", env.home.socket())
        .env("VEX_EMBED_BACKEND", "none")
        .output()
        .unwrap();
    let other: Value = serde_json::from_slice(&other.stdout).unwrap();
    assert_eq!(other["running"], true);
    assert_eq!(other["handshake_matches"], false, "{other}");
    let (_, still, _) = env.vex(&["--json", "daemon", "status"]);
    assert_eq!(
        still["pid"].as_u64(),
        Some(pid),
        "asking drained the daemon"
    );

    // Human form.
    let out = Command::new(env!("CARGO_BIN_EXE_vex"))
        .args(["daemon", "status"])
        .env("VEX_HOME", env.home.vex_home().join("vexhome"))
        .env("VEX_SOCKET", env.home.socket())
        .env("VEX_EMBED_BACKEND", "hash")
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains(&format!("pid {pid}")), "{text}");

    // Stop, then status reports not running.
    let (code, doc, err) = env.vex(&["--json", "daemon", "stop"]);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(doc["stopped"], true);
    let (_, doc, _) = env.vex(&["--json", "daemon", "status"]);
    assert_eq!(doc["running"], false, "{doc}");
    assert!(!env.home.socket().exists());
    // Stopping again is not an error.
    let (code, doc, _) = env.vex(&["--json", "daemon", "stop"]);
    assert_eq!(code, Some(0));
    assert_eq!(doc["stopped"], false);
}

#[test]
fn daemon_restart_replaces_the_process_and_start_is_idempotent() {
    let env = Env::new();
    let (_, first, _) = env.vex(&["--json", "daemon", "start"]);
    let (_, again, _) = env.vex(&["--json", "daemon", "start"]);
    assert_eq!(
        first["pid"], again["pid"],
        "a second start spawned another daemon"
    );
    let (code, restarted, err) = env.vex(&["--json", "daemon", "restart"]);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(restarted["running"], true);
    assert_ne!(restarted["pid"], first["pid"]);
}
