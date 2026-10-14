//! File watching (notify + notify-debouncer-full): every watched project's
//! tree is watched recursively; debounced changes ask the scheduler for an
//! incremental refresh, and a watcher that lost events (an overflow flagged
//! Rescan, or a watcher error) asks for a full hash rescan. Where a native
//! watcher cannot be had (inotify limits), a polling watcher takes over.
//! At daemon start `catch_up` refreshes every project that changed while
//! no daemon was watching.

use crate::scheduler::{Scheduler, Trigger};
use notify::event::Flag;
use notify::{Event, RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{DebounceEventResult, Debouncer, RecommendedCache, new_debouncer};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A changed path worth a refresh: anything but git's internals (except
/// HEAD and refs, which a checkout, commit or reset moves) and anything
/// inside a directory the walker never indexes.
pub fn relevant(root: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(root) else {
        return true;
    };
    let mut parts = rel.components().map(|c| c.as_os_str().to_string_lossy());
    match parts.next().as_deref() {
        Some(".git") => matches!(
            parts.next().as_deref(),
            Some("HEAD") | Some("refs") | Some("packed-refs")
        ),
        // A build writing into target/ or an agent into .claude/ changes
        // nothing the index holds. (The last part may be a file.)
        Some(first) => {
            let mut dirs: Vec<String> = std::iter::once(first.to_string())
                .chain(parts.map(|p| p.into_owned()))
                .collect();
            dirs.pop();
            !dirs.iter().any(|d| vex_app::walk::never_indexed_dir(d))
        }
        None => true,
    }
}

/// The live watcher, held only to keep it running.
#[allow(dead_code)]
enum Backend {
    Native(Debouncer<RecommendedWatcher, RecommendedCache>),
    Poll(Debouncer<notify::PollWatcher, notify_debouncer_full::NoCache>),
}

struct Watched {
    index: PathBuf,
    _watcher: Backend,
}

struct Inner {
    scheduler: Scheduler,
    debounce: Duration,
    roots: Mutex<HashMap<PathBuf, Watched>>,
}

/// Watches projects for the scheduler. Dropping it stops every watch.
#[derive(Clone)]
pub struct Watcher {
    inner: Arc<Inner>,
}

impl Watcher {
    pub fn new(scheduler: Scheduler, debounce: Duration) -> Watcher {
        Watcher {
            inner: Arc::new(Inner {
                scheduler,
                debounce,
                roots: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// The roots being watched, sorted.
    pub fn watched(&self) -> Vec<PathBuf> {
        let roots = self.inner.roots.lock().unwrap_or_else(|p| p.into_inner());
        let mut v: Vec<PathBuf> = roots.keys().cloned().collect();
        v.sort();
        v
    }

    /// Start watching a project (no-op if it already is).
    pub fn watch(&self, index: &Path, root: &Path) -> notify::Result<()> {
        let mut roots = self.inner.roots.lock().unwrap_or_else(|p| p.into_inner());
        if roots.contains_key(root) {
            return Ok(());
        }
        let backend = self.native(root).or_else(|_| self.polling(root))?;
        roots.insert(
            root.to_path_buf(),
            Watched {
                index: index.to_path_buf(),
                _watcher: backend,
            },
        );
        Ok(())
    }

    fn handler(&self, root: &Path) -> impl FnMut(DebounceEventResult) + Send + 'static {
        let me = Arc::downgrade(&self.inner);
        let root = root.to_path_buf();
        move |result: DebounceEventResult| {
            let Some(inner) = me.upgrade() else { return };
            Watcher { inner }.on_result(&root, result);
        }
    }

    /// One debounced batch for the project at `root`: events, or the
    /// watcher's errors (it failed or dropped events: trust nothing).
    pub fn on_result(&self, root: &Path, result: DebounceEventResult) {
        match result {
            Ok(events) => {
                for e in &events {
                    self.handle(root, &e.event);
                }
            }
            Err(_) => self.request(root, Trigger::Rescan),
        }
    }

    fn native(&self, root: &Path) -> notify::Result<Backend> {
        let mut d = new_debouncer(self.inner.debounce, None, self.handler(root))?;
        d.watch(root, RecursiveMode::Recursive)?;
        Ok(Backend::Native(d))
    }

    fn polling(&self, root: &Path) -> notify::Result<Backend> {
        let config = notify::Config::default().with_poll_interval(Duration::from_secs(2));
        let mut d = notify_debouncer_full::new_debouncer_opt::<_, notify::PollWatcher, _>(
            self.inner.debounce,
            None,
            self.handler(root),
            notify_debouncer_full::NoCache,
            config,
        )?;
        d.watch(root, RecursiveMode::Recursive)?;
        Ok(Backend::Poll(d))
    }

    fn request(&self, root: &Path, trigger: Trigger) {
        let index = {
            let roots = self.inner.roots.lock().unwrap_or_else(|p| p.into_inner());
            roots.get(root).map(|w| w.index.clone())
        };
        if let Some(index) = index {
            self.inner.scheduler.request(&index, root, trigger);
        }
    }

    /// One (debounced) watcher event for the project at `root`.
    pub fn handle(&self, root: &Path, event: &Event) {
        if event.flag() == Some(Flag::Rescan) {
            self.request(root, Trigger::Rescan);
        } else if event.paths.iter().any(|p| relevant(root, p)) {
            self.request(root, Trigger::Edit);
        }
    }
}

/// Refresh every registered project that changed while nothing watched it
/// (its recorded workspace signature or HEAD moved). Returns how many were
/// dispatched.
pub fn catch_up(home: &Path, scheduler: &Scheduler) -> usize {
    let mut n = 0;
    for (index, root) in vex_app::project::registered_projects(home) {
        if vex_app::project::is_stale(&index, &root)
            && scheduler.request(&index, &root, Trigger::Edit)
                == vex_app::freshness::Reindex::Dispatched
        {
            n += 1;
        }
    }
    n
}
