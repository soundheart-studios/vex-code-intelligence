//! The daemon's lifecycle: one daemon per VEX_HOME however many start at
//! once, a socket left by a killed daemon detected and replaced, a client
//! of another version making the daemon drain and exit so a matching one
//! can take over, and an owner-only socket.
//!
//! Real processes: the `daemon_child` helper (ignored unless run by name)
//! re-runs this test binary as a daemon, so two starts genuinely race for
//! the lock and a daemon can be SIGKILLed.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use vex_daemon::client::{self, Connect};
use vex_daemon::protocol::{Request, Response};
use vex_daemon::server::{self, Config, Exit, Handler, StartError};
use vex_test_support::TestHome;

const VERSION: &str = "1.0.0-test";

struct Echo;
impl Handler for Echo {
    fn call(&self, payload: Value) -> Value {
        json!({"echo": payload})
    }
}

fn config(home: &TestHome) -> Config {
    Config {
        home: home.vex_home().to_path_buf(),
        socket: home.socket().to_path_buf(),
        version: VERSION.to_string(),
        idle_exit: None,
    }
}

fn wait_until(limit: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    cond()
}

fn daemon_process(home: &Path, socket: &Path) -> Child {
    Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "daemon_child",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("VEX_TEST_DAEMON_HOME", home)
        .env("VEX_TEST_DAEMON_SOCKET", socket)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

/// Runs a daemon until it exits; the exit code says how it ended.
#[test]
#[ignore = "helper: run as a child process by the lifecycle tests"]
fn daemon_child() {
    let (Ok(home), Ok(socket)) = (
        std::env::var("VEX_TEST_DAEMON_HOME"),
        std::env::var("VEX_TEST_DAEMON_SOCKET"),
    ) else {
        return;
    };
    let cfg = Config {
        home: PathBuf::from(home),
        socket: PathBuf::from(socket),
        version: VERSION.to_string(),
        idle_exit: None,
    };
    match server::run(cfg, Arc::new(Echo)) {
        Ok(Exit::Shutdown) => std::process::exit(0),
        Ok(Exit::Restart) => std::process::exit(3),
        Ok(Exit::Idle) => std::process::exit(4),
        Err(StartError::AlreadyRunning { .. }) => std::process::exit(10),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(11)
        }
    }
}

fn hello(socket: &Path) -> Result<Response, String> {
    let mut conn = match client::connect(socket, VERSION) {
        Connect::Ready(c) => c,
        other => return Err(format!("{other:?}")),
    };
    conn.request(&Request::Ping).map_err(|e| e.to_string())
}

#[test]
fn two_concurrent_starts_yield_exactly_one_daemon() {
    let home = TestHome::new();
    let mut a = daemon_process(home.vex_home(), home.socket());
    let mut b = daemon_process(home.vex_home(), home.socket());
    // Exactly one of them gives up as AlreadyRunning; the other serves.
    let mut loser = None;
    assert!(
        wait_until(Duration::from_secs(20), || {
            [&mut a, &mut b]
                .iter_mut()
                .any(|c| matches!(c.try_wait(), Ok(Some(_))))
        }),
        "neither start gave way"
    );
    for (name, child) in [("a", &mut a), ("b", &mut b)] {
        if let Ok(Some(status)) = child.try_wait() {
            assert_eq!(
                status.code(),
                Some(10),
                "{name} exited, but not as AlreadyRunning"
            );
            loser = Some(name);
        }
    }
    assert!(loser.is_some());
    assert!(matches!(hello(home.socket()), Ok(Response::Pong)));
    let info: Value = serde_json::from_str(
        &std::fs::read_to_string(home.vex_home().join("daemon.json")).unwrap(),
    )
    .unwrap();
    let winner = if loser == Some("a") { &b } else { &a };
    assert_eq!(info["pid"], winner.id());
    assert_eq!(info["version"], VERSION);
    assert!(info["started_at"].as_f64().unwrap() > 0.0);

    let mut conn = match client::connect(home.socket(), VERSION) {
        Connect::Ready(c) => c,
        other => panic!("{other:?}"),
    };
    assert!(matches!(
        conn.request(&Request::Shutdown).unwrap(),
        Response::ShuttingDown
    ));
    for child in [&mut a, &mut b] {
        let _ = child.wait();
    }
    assert!(!home.socket().exists(), "shutdown left the socket behind");
    assert!(!home.vex_home().join("daemon.json").exists());
}

#[test]
fn a_stale_socket_from_a_killed_daemon_is_detected_and_replaced() {
    let home = TestHome::new();
    let mut first = daemon_process(home.vex_home(), home.socket());
    assert!(wait_until(Duration::from_secs(20), || hello(home.socket()).is_ok()));
    first.kill().unwrap();
    first.wait().unwrap();
    // SIGKILL runs no cleanup: the socket file and daemon.json remain.
    assert!(home.socket().exists());
    assert!(home.vex_home().join("daemon.json").exists());
    assert!(matches!(
        client::connect(home.socket(), VERSION),
        Connect::NotRunning
    ));

    let mut second = daemon_process(home.vex_home(), home.socket());
    assert!(
        wait_until(Duration::from_secs(20), || hello(home.socket()).is_ok()),
        "the replacement never served"
    );
    let info: Value = serde_json::from_str(
        &std::fs::read_to_string(home.vex_home().join("daemon.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(info["pid"], second.id());
    let _ = second.kill();
    let _ = second.wait();
}

#[test]
fn a_client_of_another_version_triggers_drain_and_restart() {
    let home = TestHome::new();
    let cfg = config(&home);
    let socket = cfg.socket.clone();
    let daemon = std::thread::spawn(move || server::run(cfg, Arc::new(Echo)));
    assert!(wait_until(Duration::from_secs(10), || hello(&socket).is_ok()));

    // A connection that never sends anything after its handshake: the
    // drain must close it rather than wait on it.
    let idle = match client::connect(&socket, VERSION) {
        Connect::Ready(c) => c,
        other => panic!("{other:?}"),
    };
    // A matching client is served.
    let mut conn = match client::connect(&socket, VERSION) {
        Connect::Ready(c) => c,
        other => panic!("{other:?}"),
    };
    match conn
        .request(&Request::Call {
            payload: json!({"q": 1}),
        })
        .unwrap()
    {
        Response::Reply { payload } => assert_eq!(payload, json!({"echo": {"q": 1}})),
        other => panic!("{other:?}"),
    }

    match client::connect(&socket, "2.0.0-newer") {
        Connect::Mismatch { daemon_version } => assert_eq!(daemon_version, VERSION),
        other => panic!("expected a mismatch, got {other:?}"),
    }
    // The old daemon drains (the open connection still gets its answer)
    // and exits, asking to be restarted.
    match conn.request(&Request::Ping) {
        Ok(Response::Pong) | Err(_) => {}
        Ok(other) => panic!("{other:?}"),
    }
    // Draining closes idle connections instead of waiting on them: `conn`
    // is still open here.
    let draining = Instant::now();
    let exit = daemon.join().unwrap().unwrap();
    assert!(
        draining.elapsed() < Duration::from_secs(5),
        "the drain waited {:?} on an idle connection",
        draining.elapsed()
    );
    drop((conn, idle));
    assert!(matches!(exit, Exit::Restart));
    assert!(!socket.exists());
    assert!(!home.vex_home().join("daemon.json").exists());

    // The lock is free: a daemon of the new version can start at once.
    let mut cfg = config(&home);
    cfg.version = "2.0.0-newer".into();
    let socket2 = cfg.socket.clone();
    let daemon = std::thread::spawn(move || server::run(cfg, Arc::new(Echo)));
    assert!(wait_until(Duration::from_secs(10), || matches!(
        client::connect(&socket2, "2.0.0-newer"),
        Connect::Ready(_)
    )));
    if let Connect::Ready(mut c) = client::connect(&socket2, "2.0.0-newer") {
        c.request(&Request::Shutdown).unwrap();
    }
    assert!(matches!(daemon.join().unwrap().unwrap(), Exit::Shutdown));
}

#[cfg(unix)]
#[test]
fn the_socket_file_is_mode_0600() {
    use std::os::unix::fs::PermissionsExt;
    let home = TestHome::new();
    let cfg = config(&home);
    let socket = cfg.socket.clone();
    let daemon = std::thread::spawn(move || server::run(cfg, Arc::new(Echo)));
    assert!(wait_until(Duration::from_secs(10), || hello(&socket).is_ok()));
    let mode = std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "socket mode {mode:o}");
    let info = home.vex_home().join("daemon.json");
    let mode = std::fs::metadata(&info).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "daemon.json mode {mode:o}");
    if let Connect::Ready(mut c) = client::connect(&socket, VERSION) {
        c.request(&Request::Shutdown).unwrap();
    }
    daemon.join().unwrap().unwrap();
}

#[test]
fn frames_are_length_prefixed_json_and_bounded() {
    use vex_daemon::protocol::{MAX_FRAME, read_frame, write_frame};
    let mut buf = Vec::new();
    write_frame(
        &mut buf,
        &Request::Hello {
            version: "v".into(),
            schema: 1,
        },
    )
    .unwrap();
    let len = u32::from_be_bytes(buf[..4].try_into().unwrap()) as usize;
    assert_eq!(len, buf.len() - 4);
    let v: Value = serde_json::from_slice(&buf[4..]).unwrap();
    assert_eq!(v, json!({"type": "hello", "version": "v", "schema": 1}));
    let back: Request = read_frame(&mut &buf[..]).unwrap();
    assert!(matches!(back, Request::Hello { schema: 1, .. }));
    // An oversized length is refused for its size, before any body is
    // read (here an endless one).
    let header = ((MAX_FRAME + 1) as u32).to_be_bytes();
    let mut endless = std::io::Read::chain(&header[..], std::io::repeat(b' '));
    let err = read_frame::<_, Request>(&mut endless).unwrap_err();
    assert!(err.to_string().contains("limit"), "{err}");
}
