//! The daemon server: one per VEX_HOME, on an owner-only unix socket.
//!
//! Single instance comes from an exclusive lock on `daemon.lock`: the
//! kernel drops it when the holder dies, so a SIGKILLed daemon never wedges
//! the next one, and holding it proves any socket file still on disk is a
//! leftover to replace. Requests are served on tokio; the handler (the CLI
//! or MCP adapter's dispatch) runs on the blocking pool, so the daemon
//! itself holds no command logic.

use crate::protocol::{Request, Response, SCHEMA, read_frame_async, write_frame_async};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;

/// The held daemon.lock, released (by the kernel, too) when dropped.
struct InstanceLock(#[allow(dead_code)] std::fs::File);

enum Lock {
    Held(InstanceLock),
    Busy,
}

fn take_lock(path: &Path) -> std::io::Result<Lock> {
    let file = std::fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Lock::Held(InstanceLock(file))),
        Err(std::fs::TryLockError::WouldBlock) => Ok(Lock::Busy),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

/// Where a daemon lives and what it is.
#[derive(Debug, Clone)]
pub struct Config {
    pub home: PathBuf,
    pub socket: PathBuf,
    pub version: String,
    /// Exit after this long with no connection open.
    pub idle_exit: Option<Duration>,
}

/// macOS caps a unix socket path at 104 bytes including the terminator.
const MAX_SOCKET_PATH: usize = 100;

/// The daemon socket for a VEX_HOME: VEX_SOCKET if set, else
/// VEX_HOME/vex.sock, else (a home too deep for the OS limit) a /tmp name
/// derived from the home, the same for every vex version.
pub fn default_socket(home: &Path) -> PathBuf {
    if let Some(s) = std::env::var_os("VEX_SOCKET").filter(|s| !s.is_empty()) {
        return PathBuf::from(s);
    }
    let socket = home.join("vex.sock");
    if socket.as_os_str().len() <= MAX_SOCKET_PATH {
        return socket;
    }
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(home.as_os_str().as_encoded_bytes());
    let hex: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    PathBuf::from(format!("/tmp/vex-{hex}.sock"))
}

impl Config {
    pub fn for_home(home: &Path, version: &str) -> Config {
        Config {
            home: home.to_path_buf(),
            socket: default_socket(home),
            version: version.to_string(),
            idle_exit: None,
        }
    }
}

/// What a request to the daemon does; supplied by the hosting adapter.
pub trait Handler: Send + Sync + 'static {
    fn call(&self, payload: Value) -> Value;
    /// Extra fields for `status` (watched projects, loaded models...).
    fn status(&self) -> Value {
        json!({})
    }
}

/// Why the daemon stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// Asked to stop.
    Shutdown,
    /// A client of another version arrived; start that version instead.
    Restart,
    /// Nothing connected for `idle_exit`.
    Idle,
}

#[derive(Debug)]
pub enum StartError {
    /// Another daemon holds daemon.lock.
    AlreadyRunning {
        pid: Option<u32>,
    },
    Io(std::io::Error),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartError::AlreadyRunning { pid: Some(p) } => {
                write!(f, "a vex daemon is already running (pid {p})")
            }
            StartError::AlreadyRunning { pid: None } => {
                write!(f, "a vex daemon is already running")
            }
            StartError::Io(e) => write!(f, "daemon could not start: {e}"),
        }
    }
}

impl std::error::Error for StartError {}

impl From<std::io::Error> for StartError {
    fn from(e: std::io::Error) -> Self {
        StartError::Io(e)
    }
}

pub fn lock_path(home: &Path) -> PathBuf {
    home.join("daemon.lock")
}

pub fn info_path(home: &Path) -> PathBuf {
    home.join("daemon.json")
}

/// The running daemon's daemon.json, if any.
pub fn read_info(home: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(info_path(home)).ok()?).ok()
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[cfg(unix)]
fn owner_only(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

/// daemon.json, written atomically and owner-only.
fn write_info(cfg: &Config, started_at: f64) -> std::io::Result<()> {
    let info = json!({
        "pid": std::process::id(),
        "version": cfg.version,
        "schema": SCHEMA,
        "started_at": started_at,
        "socket": cfg.socket,
    });
    let path = info_path(&cfg.home);
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(
        &tmp,
        serde_json::to_vec(&info).map_err(std::io::Error::other)?,
    )?;
    owner_only(&tmp)?;
    std::fs::rename(&tmp, &path)
}

struct Shared {
    cfg: Config,
    handler: Arc<dyn Handler>,
    stop: watch::Sender<Option<Exit>>,
    open: AtomicUsize,
    requests: std::sync::atomic::AtomicU64,
    started: Instant,
    started_at: f64,
}

impl Shared {
    fn request_stop(&self, why: Exit) {
        self.stop.send_if_modified(|current| {
            if current.is_none() {
                *current = Some(why);
                true
            } else {
                false
            }
        });
    }
}

/// Run a daemon until it is shut down, superseded or idle. Blocks.
pub fn run(cfg: Config, handler: Arc<dyn Handler>) -> Result<Exit, StartError> {
    std::fs::create_dir_all(&cfg.home)?;
    let lock = match take_lock(&lock_path(&cfg.home))? {
        Lock::Held(l) => l,
        Lock::Busy => {
            let pid = read_info(&cfg.home)
                .and_then(|i| i["pid"].as_u64())
                .map(|p| p as u32);
            return Err(StartError::AlreadyRunning { pid });
        }
    };
    // We hold the lock, so no live daemon owns a socket still on disk.
    match std::fs::remove_file(&cfg.socket) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(StartError::Io(e)),
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("vex-daemon")
        .build()?;
    let exit = runtime.block_on(serve(cfg, handler));
    // Blocking-pool handlers still running are abandoned, not awaited.
    runtime.shutdown_timeout(Duration::from_secs(5));
    drop(lock);
    exit
}

async fn serve(cfg: Config, handler: Arc<dyn Handler>) -> Result<Exit, StartError> {
    let listener = UnixListener::bind(&cfg.socket)?;
    owner_only(&cfg.socket)?;
    let started_at = now_secs();
    write_info(&cfg, started_at)?;
    let (stop, mut stopped) = watch::channel(None);
    let shared = Arc::new(Shared {
        cfg,
        handler,
        stop,
        open: AtomicUsize::new(0),
        requests: std::sync::atomic::AtomicU64::new(0),
        started: Instant::now(),
        started_at,
    });
    let mut connections = tokio::task::JoinSet::new();
    let mut last_activity = Instant::now();
    let tick = Duration::from_millis(200);
    let exit = loop {
        tokio::select! {
            accepted = listener.accept() => {
                if let Ok((stream, _)) = accepted {
                    last_activity = Instant::now();
                    shared.open.fetch_add(1, Ordering::SeqCst);
                    let s = Arc::clone(&shared);
                    connections.spawn(async move {
                        connection(stream, &s).await;
                        s.open.fetch_sub(1, Ordering::SeqCst);
                    });
                }
            }
            _ = stopped.changed() => {
                if let Some(why) = *stopped.borrow() {
                    break why;
                }
            }
            _ = tokio::time::sleep(tick) => {
                while connections.try_join_next().is_some() {}
                if shared.open.load(Ordering::SeqCst) > 0 {
                    last_activity = Instant::now();
                } else if let Some(idle) = shared.cfg.idle_exit
                    && last_activity.elapsed() >= idle
                {
                    shared.request_stop(Exit::Idle);
                }
            }
        }
    };
    // Drain: no new connections; open ones finish the request in hand.
    drop(listener);
    let _ = std::fs::remove_file(&shared.cfg.socket);
    let drained = async { while connections.join_next().await.is_some() {} };
    let _ = tokio::time::timeout(Duration::from_secs(30), drained).await;
    let _ = std::fs::remove_file(info_path(&shared.cfg.home));
    Ok(exit)
}

async fn connection(mut stream: UnixStream, shared: &Shared) {
    let mut stopped = shared.stop.subscribe();
    let hello = tokio::time::timeout(
        Duration::from_secs(5),
        read_frame_async::<_, Request>(&mut stream),
    )
    .await;
    match hello {
        Ok(Ok(Request::Hello { version, schema }))
            if version == shared.cfg.version && schema == SCHEMA =>
        {
            let welcome = Response::Welcome {
                version: shared.cfg.version.clone(),
                schema: SCHEMA,
                pid: std::process::id(),
            };
            if write_frame_async(&mut stream, &welcome).await.is_err() {
                return;
            }
        }
        Ok(Ok(Request::Hello { .. })) => {
            let answer = Response::Mismatch {
                daemon_version: shared.cfg.version.clone(),
                daemon_schema: SCHEMA,
            };
            let _ = write_frame_async(&mut stream, &answer).await;
            shared.request_stop(Exit::Restart);
            return;
        }
        Ok(Ok(_)) => {
            let answer = Response::Error {
                message: "the first frame must be hello".into(),
            };
            let _ = write_frame_async(&mut stream, &answer).await;
            return;
        }
        _ => return,
    }
    loop {
        if stopped.borrow().is_some() {
            return;
        }
        let request = tokio::select! {
            r = read_frame_async::<_, Request>(&mut stream) => r,
            _ = stopped.changed() => return,
        };
        let Ok(request) = request else { return };
        let response = match request {
            Request::Ping => Response::Pong,
            Request::Status => {
                let mut status = json!({
                    "pid": std::process::id(),
                    "version": shared.cfg.version,
                    "schema": SCHEMA,
                    "started_at": shared.started_at,
                    "uptime_s": shared.started.elapsed().as_secs_f64(),
                    "socket": shared.cfg.socket,
                    "connections": shared.open.load(Ordering::SeqCst),
                    "requests": shared.requests.load(Ordering::SeqCst),
                });
                if let (Some(obj), Value::Object(extra)) =
                    (status.as_object_mut(), shared.handler.status())
                {
                    obj.extend(extra);
                }
                Response::Status { status }
            }
            Request::Shutdown => {
                let _ = write_frame_async(&mut stream, &Response::ShuttingDown).await;
                shared.request_stop(Exit::Shutdown);
                return;
            }
            Request::Call { payload } => {
                shared.requests.fetch_add(1, Ordering::SeqCst);
                let handler = Arc::clone(&shared.handler);
                match tokio::task::spawn_blocking(move || handler.call(payload)).await {
                    Ok(payload) => Response::Reply { payload },
                    Err(e) => Response::Error {
                        message: format!("the request failed: {e}"),
                    },
                }
            }
            Request::Hello { .. } => Response::Error {
                message: "hello is only the first frame".into(),
            },
        };
        if write_frame_async(&mut stream, &response).await.is_err() {
            return;
        }
    }
}
