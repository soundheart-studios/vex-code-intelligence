//! The daemon's scheduler (Cortex `runtime/jobs.py`): it is the freshness
//! dispatcher, so a query that finds its index stale hands the project
//! here. One job per project at a time; a stale query while one is queued
//! or running attaches to it. A job refreshes the index and fills pending
//! vectors as the project's one writer, in the background, while queries
//! keep answering from the index as it was.

use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use vex_app::embed::Settings;
use vex_app::freshness::Reindex;
use vex_app::project::Scan;

pub use vex_app::freshness::Trigger;

/// A project's queued or running job and what must follow it.
struct Job {
    started: Instant,
    /// Scan again when this job ends: None, or the kind needed.
    rerun: Option<Scan>,
}

/// Cortex's REINDEX_COOLDOWN_S: the floor between query-triggered refreshes
/// of one project, so a repository mid-rebase cannot cause a refresh storm
/// (and a query that probed just before a refresh landed does not start
/// another). Edits seen by the watcher are not held back by it.
pub const QUERY_COOLDOWN: Duration = Duration::from_secs(20);

struct Inner {
    settings: Settings,
    /// When each project last had a query-triggered refresh.
    last_query: Mutex<HashMap<PathBuf, Instant>>,
    /// Projects (by index path) with a job queued or running.
    active: Mutex<HashMap<PathBuf, Job>>,
    idle: Condvar,
    refreshes: AtomicU64,
    failures: AtomicU64,
    incremental: AtomicU64,
    full: AtomicU64,
}

fn stronger(a: Option<Scan>, b: Scan) -> Scan {
    match (a, b) {
        (Some(Scan::Full), _) | (_, Scan::Full) => Scan::Full,
        _ => Scan::Incremental,
    }
}

#[derive(Clone)]
pub struct Scheduler {
    inner: Arc<Inner>,
}

impl Scheduler {
    pub fn new(settings: Settings) -> Scheduler {
        Scheduler {
            inner: Arc::new(Inner {
                settings,
                last_query: Mutex::new(HashMap::new()),
                active: Mutex::new(HashMap::new()),
                idle: Condvar::new(),
                refreshes: AtomicU64::new(0),
                failures: AtomicU64::new(0),
                incremental: AtomicU64::new(0),
                full: AtomicU64::new(0),
            }),
        }
    }

    /// Become this process's freshness dispatcher.
    pub fn install(&self) {
        let me = self.clone();
        vex_app::freshness::set_dispatcher(Some(Arc::new(move |index, root, trigger| {
            me.request(index, root, trigger)
        })));
    }

    /// A stale query's refresh: start one unless a job is already queued
    /// or running for this index (then attach to it).
    pub fn dispatch(&self, index: &Path, root: &Path) -> Reindex {
        self.request(index, root, Trigger::Query)
    }

    /// Ask for a refresh of the index at `index`.
    pub fn request(&self, index: &Path, root: &Path, trigger: Trigger) -> Reindex {
        let scan = if trigger == Trigger::Rescan {
            Scan::Full
        } else {
            Scan::Incremental
        };
        {
            let mut active = self.inner.active.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(job) = active.get_mut(index) {
                if trigger != Trigger::Query {
                    job.rerun = Some(stronger(job.rerun, scan));
                }
                return Reindex::InFlight;
            }
            if trigger == Trigger::Query {
                let mut last = self
                    .inner
                    .last_query
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                if last
                    .get(index)
                    .is_some_and(|t| t.elapsed() < QUERY_COOLDOWN)
                {
                    return Reindex::Cooldown;
                }
                last.insert(index.to_path_buf(), Instant::now());
            }
            active.insert(
                index.to_path_buf(),
                Job {
                    started: Instant::now(),
                    rerun: None,
                },
            );
        }
        let inner = Arc::clone(&self.inner);
        let key = index.to_path_buf();
        let (index, root) = (index.to_path_buf(), root.to_path_buf());
        let spawned = std::thread::Builder::new()
            .name("vex-refresh".into())
            .spawn(move || {
                let mut scan = scan;
                loop {
                    let counter = match scan {
                        Scan::Full => &inner.full,
                        Scan::Incremental => &inner.incremental,
                    };
                    counter.fetch_add(1, Ordering::SeqCst);
                    match vex_app::project::refresh(&index, &root, &inner.settings, scan) {
                        Ok(_) => inner.refreshes.fetch_add(1, Ordering::SeqCst),
                        Err(_) => inner.failures.fetch_add(1, Ordering::SeqCst),
                    };
                    let mut active = inner.active.lock().unwrap_or_else(|p| p.into_inner());
                    match active.get_mut(&index).and_then(|j| j.rerun.take()) {
                        Some(next) => scan = next,
                        None => {
                            active.remove(&index);
                            inner.idle.notify_all();
                            return;
                        }
                    }
                }
            });
        if spawned.is_err() {
            let mut active = self.inner.active.lock().unwrap_or_else(|p| p.into_inner());
            active.remove(&key);
            return Reindex::NoDispatcher;
        }
        Reindex::Dispatched
    }

    /// Scans run so far: (incremental, full).
    pub fn scans(&self) -> (u64, u64) {
        (
            self.inner.incremental.load(Ordering::SeqCst),
            self.inner.full.load(Ordering::SeqCst),
        )
    }

    /// Wait until no job is queued or running; false on timeout.
    pub fn wait_idle(&self, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        let mut active = self.inner.active.lock().unwrap_or_else(|p| p.into_inner());
        while !active.is_empty() {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            active = self
                .inner
                .idle
                .wait_timeout(active, deadline - now)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
        true
    }

    /// Background refreshes completed so far.
    pub fn refreshes(&self) -> u64 {
        self.inner.refreshes.load(Ordering::SeqCst)
    }

    /// What the scheduler is doing, for the daemon's status.
    pub fn status(&self) -> Value {
        let active = self.inner.active.lock().unwrap_or_else(|p| p.into_inner());
        let mut running: Vec<Value> = active
            .iter()
            .map(|(p, j)| json!({"index": p, "running_s": j.started.elapsed().as_secs_f64()}))
            .collect();
        running.sort_by_key(|v| v["index"].as_str().unwrap_or("").to_string());
        json!({
            "refreshing": running,
            "refreshes": self.inner.refreshes.load(Ordering::SeqCst),
            "refresh_failures": self.inner.failures.load(Ordering::SeqCst),
            "incremental_scans": self.inner.incremental.load(Ordering::SeqCst),
            "full_scans": self.inner.full.load(Ordering::SeqCst),
        })
    }
}
