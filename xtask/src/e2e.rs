//! `cargo xtask e2e`: a scripted end-to-end agent session on a real
//! multi-language repository (the itsdangerous, ky and chi benchmark
//! packages side by side, committed as one git repository):
//!
//! 1. `vex init --agent claude` and `vex hook install`;
//! 2. a `vex mcp` session whose search / expand / impact calls are served
//!    by the daemon (proved by the daemon's own request counter);
//! 3. an edit, picked up by the daemon's watcher, answered fresh by the
//!    next MCP search without any `vex index` or hook;
//! 4. a branch checkout, synced by the post-checkout hook.
//!
//! Hash embeddings keep it deterministic and offline. Everything runs in a
//! private VEX_HOME; the daemon is stopped at the end.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

const PACKAGES: &[&str] = &["itsdangerous", "ky", "chi"];

struct Session {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next: u64,
}

impl Session {
    fn send(&mut self, v: &Value) -> Result<(), String> {
        writeln!(self.stdin, "{v}").map_err(|e| e.to_string())?;
        self.stdin.flush().map_err(|e| e.to_string())
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next;
        self.next += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        loop {
            let mut line = String::new();
            if self
                .stdout
                .read_line(&mut line)
                .map_err(|e| e.to_string())?
                == 0
            {
                return Err("vex mcp closed its output".into());
            }
            let v: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
            if v["id"] == id {
                return Ok(v);
            }
        }
    }

    /// A tool's text, or Err when the call failed.
    fn tool(&mut self, name: &str, args: Value) -> Result<String, String> {
        let v = self.request("tools/call", json!({"name": name, "arguments": args}))?;
        let text = v["result"]["content"][0]["text"]
            .as_str()
            .ok_or(format!("{name}: no text in {v}"))?
            .to_string();
        if v["result"]["isError"] == true {
            return Err(format!("{name}: {text}"));
        }
        Ok(text)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Run {
    vex: PathBuf,
    home: PathBuf,
    repo: PathBuf,
}

impl Run {
    fn command(&self, program: &Path) -> Command {
        let mut c = Command::new(program);
        c.current_dir(&self.repo)
            .env("VEX_HOME", &self.home)
            .env("VEX_EMBED_BACKEND", "hash")
            .env("VEX_DAEMON_AUTOSTART", "always")
            .env("VEX_DAEMON_IDLE_EXIT_SECS", "300")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("VEX_SOCKET");
        c
    }

    fn vex(&self, args: &[&str]) -> Result<Value, String> {
        let out = self
            .command(&self.vex)
            .args(args)
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(format!(
                "vex {args:?} exited {:?}: {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        Ok(serde_json::from_slice(&out.stdout).unwrap_or(Value::Null))
    }

    fn git(&self, args: &[&str]) -> Result<(), String> {
        let out = self
            .command(Path::new("git"))
            .args([
                "-c",
                "user.name=e2e",
                "-c",
                "user.email=e2e@localhost",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            ))
        }
    }

    fn mcp(&self) -> Result<Session, String> {
        let mut child = self
            .command(&self.vex)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = BufReader::new(child.stdout.take().ok_or("no stdout")?);
        let mut s = Session {
            child,
            stdin,
            stdout,
            next: 1,
        };
        let init = s.request(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "vex-e2e", "version": "0"}}),
        )?;
        if init["result"]["serverInfo"]["name"] != "vex" {
            return Err(format!("unexpected initialize answer: {init}"));
        }
        s.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))?;
        Ok(s)
    }

    fn requests(&self) -> Result<u64, String> {
        let s = self.vex(&["--json", "daemon", "status"])?;
        if s["running"] != true {
            return Err(format!("the daemon is not running: {s}"));
        }
        s["requests"]
            .as_u64()
            .ok_or(format!("no request count in {s}"))
    }
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let dest = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            if entry.file_name() != ".git" {
                copy_tree(&entry.path(), &dest)?;
            }
        } else {
            std::fs::write(&dest, std::fs::read(entry.path())?)?;
        }
    }
    Ok(())
}

fn eventually(
    limit: Duration,
    mut cond: impl FnMut() -> Result<bool, String>,
) -> Result<bool, String> {
    let start = Instant::now();
    loop {
        if cond()? {
            return Ok(true);
        }
        if start.elapsed() > limit {
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// The paths of the hits a search answer returned.
fn hit_files(text: &str) -> Vec<String> {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|v| v["results"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|r| r["file"].as_str().map(String::from))
        .collect()
}

fn step(name: &str, result: Result<String, String>) -> Result<(), String> {
    match result {
        Ok(detail) => {
            println!("ok   {name}: {detail}");
            Ok(())
        }
        Err(e) => {
            println!("FAIL {name}: {e}");
            Err(e)
        }
    }
}

fn session(run: &Run) -> Result<(), String> {
    step("init with the claude agent", {
        let r = run.vex(&["--json", "init", "--agent", "claude"])?;
        let files = r["index"]["files_scanned"].as_u64().unwrap_or(0);
        if files < 50 || !run.repo.join(".mcp.json").exists() {
            Err(format!("init did not index and wire the agent: {r}"))
        } else {
            Ok(format!("{files} files, .mcp.json written"))
        }
    })?;
    step("hook install", {
        let r = run.vex(&["--json", "hook", "install"])?;
        let n = r["hooks"].as_array().map_or(0, Vec::len);
        if n < 4 {
            Err(format!("{r}"))
        } else {
            Ok(format!("{n} hooks"))
        }
    })?;

    let mut mcp = run.mcp()?;
    // The first call may spawn the daemon; count from a known running one.
    mcp.tool("list_projects", json!({}))?;
    let before = run.requests()?;
    step("mcp search via the daemon", {
        let text = mcp.tool("search", json!({"query": "sign a value with a timestamp"}))?;
        let files = hit_files(&text);
        if files.iter().any(|f| f.contains("itsdangerous")) {
            Ok(format!("top hit {}", files[0]))
        } else {
            Err(format!("no itsdangerous hit: {text}"))
        }
    })?;
    step("mcp expand via the daemon", {
        let text = mcp.tool("expand", json!({"target": "TimestampSigner"}))?;
        if text.contains("class TimestampSigner") {
            Ok("source returned".into())
        } else {
            Err(text)
        }
    })?;
    step("mcp impact via the daemon", {
        let text = mcp.tool(
            "impact",
            json!({"symbol": "want_bytes", "direction": "upstream"}),
        )?;
        if text.contains("want_bytes") {
            Ok("callers returned".into())
        } else {
            Err(text)
        }
    })?;
    step("the daemon served them", {
        let after = run.requests()?;
        if after >= before + 3 {
            Ok(format!("{} requests", after - before))
        } else {
            Err(format!(
                "only {} of 3 calls reached the daemon",
                after - before
            ))
        }
    })?;

    // An edit, with no vex index and no git operation: the watcher.
    std::fs::write(
        run.repo.join("ky/source/e2e_probe.ts"),
        "export function freshlyWrittenProbe(): number {\n  return 42;\n}\n",
    )
    .map_err(|e| e.to_string())?;
    step("an edit is picked up by the watcher", {
        let seen = eventually(Duration::from_secs(30), || {
            let text = mcp.tool("search", json!({"query": "freshlyWrittenProbe"}))?;
            Ok(hit_files(&text).iter().any(|f| f.ends_with("e2e_probe.ts")))
        })?;
        if seen {
            Ok("fresh result without vex index".into())
        } else {
            Err("never seen".into())
        }
    })?;

    // A branch checkout, synced by the post-checkout hook.
    run.git(&["checkout", "-q", "-b", "e2e-branch"])?;
    std::fs::write(
        run.repo.join("chi/e2e_branch.go"),
        "package chi\n\nfunc OnlyOnTheBranch() int { return 7 }\n",
    )
    .map_err(|e| e.to_string())?;
    run.git(&["add", "-A"])?;
    run.git(&["commit", "-q", "-m", "branch work"])?;
    run.git(&["checkout", "-q", "main"])?;
    step("a checkout is synced", {
        let gone = eventually(Duration::from_secs(30), || {
            let text = mcp.tool("search", json!({"query": "OnlyOnTheBranch"}))?;
            Ok(!hit_files(&text)
                .iter()
                .any(|f| f.ends_with("e2e_branch.go")))
        })?;
        if gone {
            Ok("main no longer shows the branch's file".into())
        } else {
            Err("stale after checkout".into())
        }
    })?;
    Ok(())
}

pub fn main(root: &Path) -> i32 {
    let corpora = std::env::var_os("VEX_CORTEX_CORPORA")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Documents/cortex-corpora"))
        });
    let Some(packages) = corpora
        .map(|c| c.join("graph/packages"))
        .filter(|p| p.is_dir())
    else {
        eprintln!("e2e: the benchmark packages are not available (VEX_CORTEX_CORPORA)");
        return 1;
    };
    let status = Command::new(env!("CARGO"))
        .current_dir(root)
        .args(["build", "--quiet", "-p", "vex-cli", "--bin", "vex"])
        .status();
    if !matches!(status, Ok(s) if s.success()) {
        eprintln!("e2e: building vex failed");
        return 1;
    }
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target"));
    let work = std::env::temp_dir().join(format!(
        "vex-e2e-{}-{}",
        std::env::var("PW_RUN_ID").unwrap_or_else(|_| "local".into()),
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&work);
    let run = Run {
        vex: target.join("debug/vex"),
        home: work.join("vexhome"),
        repo: work.join("repo"),
    };
    let setup = (|| -> Result<(), String> {
        for p in PACKAGES {
            copy_tree(&packages.join(p), &run.repo.join(p)).map_err(|e| format!("{p}: {e}"))?;
        }
        run.git(&["init", "-q", "-b", "main"])?;
        run.git(&["add", "-A"])?;
        run.git(&["commit", "-q", "-m", "three packages"])
    })();
    let outcome = setup.and_then(|_| session(&run));
    let _ = run.vex(&["daemon", "stop"]);
    let _ = std::fs::remove_dir_all(&work);
    match outcome {
        Ok(()) => {
            println!("e2e: passed");
            0
        }
        Err(_) => {
            eprintln!("e2e: failed");
            1
        }
    }
}
