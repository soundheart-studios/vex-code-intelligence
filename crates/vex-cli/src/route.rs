//! Sending a command to the daemon: connect (spawning one per the autostart
//! policy), hand it the arguments and the directory, print what it returns.
//! Anything short of an answer means the command runs in-process instead.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use vex_daemon::client::{self, Conn, Connect};
use vex_daemon::protocol::{Request, Response};

/// How long a client waits for a daemon it spawned to answer.
const SPAWN_WAIT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Autostart {
    /// Spawn a daemon when none is running.
    Always,
    /// Use a running daemon; otherwise run in-process.
    WhenNeeded,
    /// Always run in-process.
    Never,
}

pub fn policy() -> Autostart {
    match std::env::var("VEX_DAEMON_AUTOSTART")
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "never" | "off" | "0" => Autostart::Never,
        "when-needed" | "when_needed" => Autostart::WhenNeeded,
        _ => Autostart::Always,
    }
}

fn spawn(home: &Path) -> bool {
    let exe = std::env::var_os("VEX_DAEMON_EXE")
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok());
    let Some(exe) = exe else { return false };
    std::process::Command::new(exe)
        .args(["daemon", "run", "--detach"])
        .env("VEX_HOME", home)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A ready connection to a daemon answering as this binary would, or None.
pub fn connect(policy: Autostart) -> Option<Conn> {
    if policy == Autostart::Never {
        return None;
    }
    let home = vex_app::home()?;
    let socket = vex_daemon::server::default_socket(&home);
    let version = vex_app::version::handshake();
    match client::connect(&socket, &version) {
        Connect::Ready(c) => return Some(c),
        Connect::Failed(_) => return None,
        Connect::NotRunning | Connect::Mismatch { .. } if policy == Autostart::Always => {}
        _ => return None,
    }
    // Wait out a draining daemon of another version, then start ours.
    let deadline = Instant::now() + SPAWN_WAIT;
    let mut spawned = false;
    while Instant::now() < deadline {
        match client::connect(&socket, &version) {
            Connect::Ready(c) => return Some(c),
            Connect::NotRunning if !spawned => {
                if !spawn(&home) {
                    return None;
                }
                spawned = true;
            }
            _ => {}
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

/// Ask the daemon; None means it did not answer.
pub fn call(conn: &mut Conn, payload: Value) -> Option<Value> {
    match conn.request(&Request::Call { payload }) {
        Ok(Response::Reply { payload }) => Some(payload),
        _ => None,
    }
}

/// Run this command through the daemon: its exit code, or None to run it
/// in-process.
pub fn command(argv: &[String]) -> Option<i32> {
    let mut conn = connect(policy())?;
    let cwd = std::env::current_dir().ok()?;
    let reply = call(&mut conn, json!({"kind": "cli", "argv": argv, "cwd": cwd}))?;
    let decode = |k: &str| B64.decode(reply[k].as_str()?).ok();
    let (out, err) = (decode("stdout")?, decode("stderr")?);
    let code = reply["code"].as_i64()? as i32;
    crate::out::write_out(&out);
    crate::out::write_err(&err);
    Some(code)
}

/// Hand `root` to the daemon to refresh, which fills its pending vectors
/// in the background (`vex sync` without `--wait`). False when no daemon
/// took it.
pub fn background_sync(root: &Path) -> bool {
    let Some(mut conn) = connect(policy()) else {
        return false;
    };
    let Ok(cwd) = std::env::current_dir() else {
        return false;
    };
    let argv = ["vex", "--json", "sync", &root.display().to_string()].map(String::from);
    call(&mut conn, json!({"kind": "cli", "argv": argv, "cwd": cwd}))
        .and_then(|reply| reply["code"].as_i64())
        == Some(0)
}
