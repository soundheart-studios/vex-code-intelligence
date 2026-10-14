//! `vex daemon run`: host the daemon. Its handler runs this binary's own
//! command dispatch (and the MCP tools) for clients, with output captured
//! and paths resolved against the client's directory, so a routed command
//! prints exactly what it would have printed in-process.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use vex_daemon::server::{self, Config, Handler, StartError};

/// True while the start-up catch-up is still checking projects.
static CATCHING_UP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// How long file changes settle before a refresh is asked for.
const WATCH_DEBOUNCE: Duration = Duration::from_millis(300);

/// How long an idle daemon waits for a request before exiting.
pub const DEFAULT_IDLE_EXIT: Duration = Duration::from_secs(1800);

struct CliHandler {
    scheduler: vex_daemon::scheduler::Scheduler,
    watcher: vex_daemon::watch::Watcher,
}

impl Handler for CliHandler {
    fn status(&self) -> Value {
        let models: Vec<Value> = vex_app::embed::loaded_models()
            .into_iter()
            .map(|(dir, loaded, left)| {
                json!({
                    "models": dir,
                    "loaded": loaded,
                    "idle_remaining_s": left.map(|d| d.as_secs()),
                })
            })
            .collect();
        let mut status = self.scheduler.status();
        status["models"] = Value::Array(models);
        status["watched"] = json!(self.watcher.watched());
        status["catching_up"] = Value::Bool(CATCHING_UP.load(std::sync::atomic::Ordering::SeqCst));
        status
    }

    fn call(&self, payload: Value) -> Value {
        let cwd = PathBuf::from(payload["cwd"].as_str().unwrap_or("/"));
        match payload["kind"].as_str() {
            Some("cli") => {
                let argv: Vec<String> = payload["argv"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                let (code, out, err) = crate::out::captured(&cwd, || crate::run_argv(&argv));
                json!({"code": code, "stdout": B64.encode(out), "stderr": B64.encode(err)})
            }
            Some("mcp") => {
                let name = payload["name"].as_str().unwrap_or("");
                let args = payload["args"].as_object().cloned().unwrap_or_default();
                let called = vex_mcp::call(vex_app::home().as_deref(), &cwd, name, &args);
                json!({"text": called.text, "is_error": called.is_error})
            }
            _ => json!({"error": "unknown request kind"}),
        }
    }
}

fn idle_exit() -> Duration {
    std::env::var("VEX_DAEMON_IDLE_EXIT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_IDLE_EXIT)
}

/// Start a daemon in the background and return at once: re-run this binary
/// as `vex daemon run` in its own process group, output to the daemon log.
pub fn detach() -> i32 {
    use std::os::unix::process::CommandExt;
    let Some(home) = vex_app::home() else {
        return crate::exit::RUNTIME;
    };
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(_) => return crate::exit::RUNTIME,
    };
    let logs = home.join("logs");
    let _ = std::fs::create_dir_all(&logs);
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(logs.join("daemon.log"));
    let (out, err) = match log.and_then(|f| Ok((f.try_clone()?, f))) {
        Ok((a, b)) => (std::process::Stdio::from(a), std::process::Stdio::from(b)),
        Err(_) => (std::process::Stdio::null(), std::process::Stdio::null()),
    };
    // Run from VEX_HOME, not from wherever the first client happened to be.
    match std::process::Command::new(exe)
        .args(["daemon", "run"])
        .current_dir(&home)
        .stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(err)
        .process_group(0)
        .spawn()
    {
        Ok(_) => crate::exit::OK,
        Err(_) => crate::exit::RUNTIME,
    }
}

/// Serve until shut down, superseded or idle.
pub fn run() -> i32 {
    let Some(home) = vex_app::home() else {
        eprintln!("vex: neither VEX_HOME nor HOME is set");
        return crate::exit::RUNTIME;
    };
    let mut cfg = Config::for_home(&home, &vex_app::version::handshake());
    cfg.idle_exit = Some(idle_exit());
    // The daemon is the writer: stale queries hand their project to the
    // scheduler, which refreshes it in the background.
    // Its embedding is background work: fewer model sessions, so the
    // machine stays responsive.
    vex_app::embed::set_background(true);
    let scheduler =
        vex_daemon::scheduler::Scheduler::new(vex_app::embed::Settings::from_env(&home));
    scheduler.install();
    // Watch every project a query touches; catch up the ones that changed
    // while no daemon was running, without holding up the first answers.
    let watcher = vex_daemon::watch::Watcher::new(scheduler.clone(), WATCH_DEBOUNCE);
    let observer = watcher.clone();
    vex_app::freshness::set_observer(Some(Arc::new(move |index, root| {
        let _ = observer.watch(index, root);
    })));
    {
        let (home, scheduler) = (home.clone(), scheduler.clone());
        CATCHING_UP.store(true, std::sync::atomic::Ordering::SeqCst);
        std::thread::spawn(move || {
            vex_daemon::watch::catch_up(&home, &scheduler);
            CATCHING_UP.store(false, std::sync::atomic::Ordering::SeqCst);
        });
    }
    match server::run(cfg, Arc::new(CliHandler { scheduler, watcher })) {
        Ok(_) => crate::exit::OK,
        // Another daemon got there first: it serves.
        Err(StartError::AlreadyRunning { .. }) => crate::exit::OK,
        Err(e) => {
            eprintln!("vex: {e}");
            crate::exit::RUNTIME
        }
    }
}

/// The running daemon's status, asked with its own handshake (so asking
/// never makes a daemon of another version drain), or None.
fn ask_status(home: &std::path::Path) -> Option<Value> {
    use vex_daemon::client::{self, Connect};
    use vex_daemon::protocol::{Request, Response};
    let info = server::read_info(home)?;
    let version = info["version"].as_str()?.to_string();
    let socket = vex_daemon::server::default_socket(home);
    let Connect::Ready(mut conn) = client::connect(&socket, &version) else {
        return None;
    };
    match conn.request(&Request::Status) {
        Ok(Response::Status { status }) => Some(status),
        _ => None,
    }
}

fn status_doc(home: &std::path::Path) -> Value {
    match ask_status(home) {
        Some(mut s) => {
            let matches = s["version"].as_str() == Some(vex_app::version::handshake().as_str());
            let mut doc = json!({"schema": vex_app::SCHEMA, "running": true});
            if let (Some(d), Some(extra)) = (doc.as_object_mut(), s.as_object_mut()) {
                d.append(extra);
                d.insert("handshake_matches".into(), Value::Bool(matches));
            }
            doc
        }
        None => json!({"schema": vex_app::SCHEMA, "running": false}),
    }
}

fn print_status(doc: &Value) {
    if doc["running"] != true {
        outln!("daemon: not running");
        return;
    }
    outln!(
        "daemon: pid {} version {}{} up {:.0}s, {} requests",
        doc["pid"],
        doc["version"].as_str().unwrap_or("?"),
        if doc["handshake_matches"] == true {
            ""
        } else {
            " (another build or settings)"
        },
        doc["uptime_s"].as_f64().unwrap_or(0.0),
        doc["requests"]
    );
    for w in doc["watched"].as_array().into_iter().flatten() {
        outln!("watching {}", w.as_str().unwrap_or(""));
    }
    for m in doc["models"].as_array().into_iter().flatten() {
        outln!(
            "model in {}: {}",
            m["models"].as_str().unwrap_or("?"),
            if m["loaded"] == true {
                "loaded"
            } else {
                "not loaded"
            }
        );
    }
}

fn emit(json: bool, doc: &Value, human: impl FnOnce(&Value)) -> i32 {
    if json {
        outln!("{doc}");
    } else {
        human(doc);
    }
    crate::exit::OK
}

fn no_home() -> i32 {
    errln!("vex: neither VEX_HOME nor HOME is set");
    crate::exit::RUNTIME
}

/// `vex daemon status`.
pub fn status(json: bool) -> i32 {
    let Some(home) = vex_app::home() else {
        return no_home();
    };
    emit(json, &status_doc(&home), print_status)
}

/// Wait for `cond` up to `limit`.
fn wait_for(limit: Duration, cond: impl Fn() -> bool) -> bool {
    let deadline = std::time::Instant::now() + limit;
    while std::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    cond()
}

fn start_daemon(home: &std::path::Path) -> bool {
    if ask_status(home).is_some() {
        return true;
    }
    let exe = std::env::var_os("VEX_DAEMON_EXE")
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok());
    let Some(exe) = exe else { return false };
    let spawned = std::process::Command::new(exe)
        .args(["daemon", "run", "--detach"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    spawned && wait_for(Duration::from_secs(5), || ask_status(home).is_some())
}

fn stop_daemon(home: &std::path::Path) -> bool {
    use vex_daemon::client::{self, Connect};
    use vex_daemon::protocol::Request;
    let Some(info) = server::read_info(home) else {
        return false;
    };
    let socket = vex_daemon::server::default_socket(home);
    let version = info["version"].as_str().unwrap_or("").to_string();
    let Connect::Ready(mut conn) = client::connect(&socket, &version) else {
        return false;
    };
    let _ = conn.request(&Request::Shutdown);
    wait_for(Duration::from_secs(10), || {
        !socket.exists() && server::read_info(home).is_none()
    });
    true
}

/// `vex daemon start`.
pub fn start(json: bool) -> i32 {
    let Some(home) = vex_app::home() else {
        return no_home();
    };
    if !start_daemon(&home) {
        errln!(
            "vex: the daemon did not start (see {})",
            home.join("logs/daemon.log").display()
        );
        return crate::exit::RUNTIME;
    }
    emit(json, &status_doc(&home), print_status)
}

/// `vex daemon stop`.
pub fn stop(json: bool) -> i32 {
    let Some(home) = vex_app::home() else {
        return no_home();
    };
    let stopped = stop_daemon(&home);
    emit(
        json,
        &json!({"schema": vex_app::SCHEMA, "stopped": stopped}),
        |_| {
            outln!(
                "{}",
                if stopped {
                    "daemon stopped"
                } else {
                    "daemon: not running"
                }
            );
        },
    )
}

/// `vex daemon restart`.
pub fn restart(json: bool) -> i32 {
    let Some(home) = vex_app::home() else {
        return no_home();
    };
    stop_daemon(&home);
    if !start_daemon(&home) {
        errln!(
            "vex: the daemon did not start (see {})",
            home.join("logs/daemon.log").display()
        );
        return crate::exit::RUNTIME;
    }
    emit(json, &status_doc(&home), print_status)
}
