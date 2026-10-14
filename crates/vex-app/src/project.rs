//! Projects: which root a command means, its registry entry and its index.
//!
//! The project is the git toplevel containing the given (or current)
//! directory, worktree-aware, or the directory itself when there is no git.
//! Roots are canonicalised so `/var/...` and `/private/var/...` (macOS)
//! name one project.

use crate::embed::{Plan, Session};
pub use crate::index::Scan;
use crate::index::{IndexError, IndexReport, run_with, workspace_signature};
use crate::walk::walk;
use rusqlite::OptionalExtension;
use serde::Serialize;
use std::path::{Path, PathBuf};
use vex_store::db::Index;
use vex_store::registry::{Attached, Registry, index_path, project_dir};

#[derive(Debug)]
pub enum ProjectError {
    /// No index for this project yet (exit 2).
    NoIndex {
        root: PathBuf,
    },
    BadPath {
        path: PathBuf,
        error: String,
    },
    Store(String),
    /// An agent harness config could not be written (exit 1).
    Agent(String),
    /// The index's vectors are of another width than the embedder's (exit 1).
    Embedding(String),
    /// Downloaded weights failed their sha256 pin (exit 1).
    ModelVerification(String),
    /// The weights could not be downloaded (exit 4).
    ModelDownload(String),
}

impl std::fmt::Display for ProjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProjectError::NoIndex { root } => {
                write!(
                    f,
                    "{} has no vex index yet; run `vex init` there first",
                    root.display()
                )
            }
            ProjectError::BadPath { path, error } => write!(f, "{}: {error}", path.display()),
            ProjectError::Store(e) => write!(f, "index error: {e}"),
            ProjectError::Agent(e) => write!(f, "agent setup: {e}"),
            ProjectError::Embedding(e)
            | ProjectError::ModelVerification(e)
            | ProjectError::ModelDownload(e) => write!(f, "{e}"),
        }
    }
}

impl From<rusqlite::Error> for ProjectError {
    fn from(e: rusqlite::Error) -> Self {
        ProjectError::Store(e.to_string())
    }
}

impl From<IndexError> for ProjectError {
    fn from(e: IndexError) -> Self {
        match e {
            IndexError::Sql(e) => e.into(),
            IndexError::Width(m) => ProjectError::Embedding(m),
        }
    }
}

/// How long a writer waits for another (the daemon's background refresh,
/// or a second CLI) before giving up.
pub const WRITE_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

/// Take the single-writer lock beside an index (write.lock), waiting up to
/// `wait` for the writer that holds it.
pub fn lock_index(
    index: &Path,
    wait: std::time::Duration,
) -> Result<vex_store::lock::WriteLock, ProjectError> {
    use vex_store::lock::{LockError, WriteLock};
    let path = index.with_file_name("write.lock");
    let deadline = std::time::Instant::now() + wait;
    loop {
        match WriteLock::try_acquire(&path) {
            Ok(lock) => return Ok(lock),
            Err(LockError::Busy(_)) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(e) => return Err(ProjectError::Store(e.to_string())),
        }
    }
}

/// Run the pipeline under an embedding plan, as the index's one writer.
fn run_planned(
    idx: &Index,
    root: &Path,
    force: bool,
    plan: &Plan,
    model_failure: Option<String>,
) -> Result<IndexReport, ProjectError> {
    let _writer = lock_index(&idx.path, WRITE_WAIT)?;
    let mut session = Session::open(&plan.settings);
    // A model that already failed to download is not fetched again.
    if let (Some(s), Some(why)) = (session.as_mut(), model_failure) {
        s.failure = Some(why);
    }
    Ok(run_with(idx, root, force, session.as_mut(), plan.defer)?)
}

/// A background refresh (the daemon's scheduler): bring the index at
/// `index` up to date with `root`, then fill any vectors still pending, as
/// the index's one writer.
pub fn refresh(
    index: &Path,
    root: &Path,
    settings: &crate::embed::Settings,
    scan: crate::index::Scan,
) -> Result<IndexReport, ProjectError> {
    let _writer = lock_index(index, WRITE_WAIT)?;
    let idx = Index::open(index)?;
    let mut session = Session::open(settings);
    let report = crate::index::run_scan(&idx, root, false, session.as_mut(), false, scan)?;
    if let Some(s) = session.as_mut() {
        crate::embed::drain(&idx, s)?;
    }
    Ok(report)
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The project root for `start`: its git toplevel, else itself.
pub fn project_root(start: &Path) -> Result<PathBuf, ProjectError> {
    let canon = std::fs::canonicalize(start).map_err(|e| ProjectError::BadPath {
        path: start.to_path_buf(),
        error: e.to_string(),
    })?;
    let dir = if canon.is_dir() {
        canon.clone()
    } else {
        canon
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or(canon.clone())
    };
    match git(&dir, &["rev-parse", "--show-toplevel"]) {
        Some(top) => std::fs::canonicalize(&top).map_err(|e| ProjectError::BadPath {
            path: PathBuf::from(top),
            error: e.to_string(),
        }),
        None => Ok(dir),
    }
}

/// The repository's remote URL, if it has one (origin first).
pub fn remote_url(root: &Path) -> Option<String> {
    git(root, &["config", "--get", "remote.origin.url"]).or_else(|| {
        let first = git(root, &["remote"])?.lines().next()?.to_string();
        git(root, &["config", "--get", &format!("remote.{first}.url")])
    })
}

#[derive(Debug, Serialize)]
pub struct InitReport {
    pub schema: u32,
    pub project_id: String,
    pub root: PathBuf,
    /// `new`, `existing` or `reattached`.
    pub registration: &'static str,
    pub index: IndexReport,
    /// One entry per `--agent` harness configured.
    pub agents: Vec<crate::agent::AgentReport>,
}

fn register(home: &Path, root: &Path) -> Result<(String, &'static str), ProjectError> {
    let mut registry = Registry::load(home);
    let attached = registry.attach(root, remote_url(root).as_deref());
    registry
        .save()
        .map_err(|e| ProjectError::Store(e.to_string()))?;
    let how = match attached {
        Attached::New(_) => "new",
        Attached::Existing(_) => "existing",
        Attached::Reattached { .. } => "reattached",
    };
    Ok((attached.id().to_string(), how))
}

/// `vex init PATH`: register the project and build its index.
/// `vex init [PATH] [--agent NAME]...`: register, index, then wire each
/// requested agent harness to `vex mcp`.
pub fn init(
    home: &Path,
    start: &Path,
    agents: &[crate::agent::Agent],
    plan: &Plan,
) -> Result<InitReport, ProjectError> {
    Ok(init_prefetched(home, start, agents, plan, !plan.defer)?.0)
}

/// `init`, fetching the model first when `prefetch`; also why the fetch
/// failed, if it did.
fn init_prefetched(
    home: &Path,
    start: &Path,
    agents: &[crate::agent::Agent],
    plan: &Plan,
    prefetch: bool,
) -> Result<(InitReport, Option<String>), ProjectError> {
    vex_store::home::ensure(home).map_err(|e| ProjectError::Store(e.to_string()))?;
    let root = project_root(start)?;
    let (id, registration) = register(home, &root)?;
    let idx = Index::open(&index_path(home, &id))?;
    // First use fetches the model up front, where its progress can show,
    // rather than inside the first embedding call.
    let model_failure = (plan.settings.backend == crate::embed::Backend::Bge && prefetch)
        .then(|| crate::models::prefetch(&plan.settings).err())
        .flatten()
        .map(|e| e.to_string());
    let index = run_planned(&idx, &root, false, plan, model_failure.clone())?;
    let agents = agents
        .iter()
        .map(|a| crate::agent::install(&root, *a).map_err(|e| ProjectError::Agent(e.to_string())))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((
        InitReport {
            schema: crate::SCHEMA,
            project_id: id,
            root,
            registration,
            index,
            agents,
        },
        model_failure,
    ))
}

/// When the vectors of a long embed get computed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmbedChoice {
    /// Before init returns.
    Now,
    /// By the daemon, while keyword search already works.
    Background,
    /// When the person runs `vex embed`.
    Later,
}

/// What a timed sample says about the rest of an embed.
#[derive(Clone, Debug)]
pub struct EmbedEstimate {
    /// Chunks still without a vector.
    pub pending: usize,
    /// Chunks the sample embedded.
    pub sampled: usize,
    /// How long the pending chunks should take.
    pub estimate: std::time::Duration,
}

/// `init` for a person at a terminal: index structure first, time the
/// model on a sample of the longest chunks, and when the rest would take
/// at least `ask_over` let `decide` say when it embeds (otherwise it
/// embeds now). Also the choice made, None when nothing was asked. With
/// embedding deferred or off, or the model unavailable, it is `init`.
pub fn init_measured(
    home: &Path,
    start: &Path,
    agents: &[crate::agent::Agent],
    plan: &Plan,
    ask_over: std::time::Duration,
    decide: impl FnOnce(&EmbedEstimate) -> EmbedChoice,
) -> Result<(InitReport, Option<EmbedChoice>), ProjectError> {
    if plan.defer || plan.settings.backend == crate::embed::Backend::None {
        return Ok((init(home, start, agents, plan)?, None));
    }
    let deferred = Plan {
        settings: plan.settings.clone(),
        defer: true,
    };
    let (mut report, model_failure) = init_prefetched(home, start, agents, &deferred, true)?;
    let idx = Index::open(&index_path(home, &report.project_id))?;
    let _writer = lock_index(&idx.path, WRITE_WAIT)?;
    let Some(mut session) = Session::open(&plan.settings) else {
        return Ok((report, None));
    };
    session.failure = model_failure;
    let before = vex_store::vec::count_pending(&idx.conn)? as usize;
    let mut choice = None;
    if session.failure.is_none() && before > 0 {
        // Half a group: every session busy, done in seconds.
        let n = (session.group() / 2).max(1);
        let sample = crate::embed::sample(&idx, &mut session, n)?;
        let left = vex_store::vec::count_pending(&idx.conn)? as usize;
        if left > 0 && session.failure.is_none() {
            let rest = vex_store::vec::pending_cost(&idx.conn, crate::embed::COST_CAP)?;
            let estimate = if sample.cost > 0 {
                sample.took.mul_f64(rest as f64 / sample.cost as f64)
            } else {
                std::time::Duration::ZERO
            };
            let when = if estimate >= ask_over {
                let c = decide(&EmbedEstimate {
                    pending: left,
                    sampled: sample.embedded,
                    estimate,
                });
                choice = Some(c);
                c
            } else {
                EmbedChoice::Now
            };
            if when == EmbedChoice::Now {
                crate::embed::drain(&idx, &mut session)?;
            }
        }
    }
    let pending = vex_store::vec::count_pending(&idx.conn)? as usize;
    if let Some(e) = report.index.embed.as_mut() {
        e.embedded += before - pending;
        e.pending = pending;
        e.cache_hits += session.hits;
        e.cache_misses += session.misses;
        e.notice = session.index_notice(pending);
    }
    Ok((report, choice))
}

/// Every registered project with an index and an existing root, as
/// (index path, root).
pub fn registered_projects(home: &Path) -> Vec<(PathBuf, PathBuf)> {
    Registry::load(home)
        .projects()
        .filter(|(_, p)| p.root.is_dir())
        .map(|(id, p)| (index_path(home, id), p.root.clone()))
        .filter(|(index, _)| index.exists())
        .collect()
}

/// Whether the index at `index` is behind `root` (Cortex's probe).
pub fn is_stale(index: &Path, root: &Path) -> bool {
    Index::open(index).is_ok_and(|idx| crate::freshness::probe(&idx, root))
}

/// The registered project containing `start`: its id and root, or NoIndex.
pub fn lookup(home: &Path, start: &Path) -> Result<(String, PathBuf), ProjectError> {
    registered(home, start)
}

/// The registered project containing `start`, or NoIndex.
pub(crate) fn registered(home: &Path, start: &Path) -> Result<(String, PathBuf), ProjectError> {
    let root = project_root(start)?;
    let registry = Registry::load(home);
    let id = registry
        .find_by_root(&root)
        .map(String::from)
        .ok_or(ProjectError::NoIndex { root: root.clone() })?;
    if !index_path(home, &id).exists() {
        return Err(ProjectError::NoIndex { root });
    }
    Ok((id, root))
}

#[derive(Debug, Serialize)]
pub struct IndexCommandReport {
    pub schema: u32,
    pub project_id: String,
    pub root: PathBuf,
    pub index: IndexReport,
}

/// `vex index [PATH] [--force]`: refresh (or rebuild) a registered index.
pub fn index(
    home: &Path,
    start: &Path,
    force: bool,
    plan: &Plan,
) -> Result<IndexCommandReport, ProjectError> {
    let (id, root) = registered(home, start)?;
    let idx = Index::open(&index_path(home, &id))?;
    let index = run_planned(&idx, &root, force, plan, None)?;
    Ok(IndexCommandReport {
        schema: crate::SCHEMA,
        project_id: id,
        root,
        index,
    })
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Freshness {
    /// The working tree differs from what was indexed.
    pub workspace_changed: bool,
    /// HEAD moved since the index was built.
    pub head_changed: bool,
    pub stale: bool,
}

#[derive(Debug, Serialize)]
pub struct StatusReport {
    pub schema: u32,
    pub project_id: String,
    pub root: PathBuf,
    pub index_path: PathBuf,
    pub files: i64,
    pub chunks: i64,
    pub symbols: i64,
    pub edges: std::collections::BTreeMap<String, i64>,
    /// Chunks with a vector / all chunks (0 until vectors exist).
    pub embedded_chunks: i64,
    pub indexed_head: Option<String>,
    pub index_version: Option<String>,
    pub freshness: Freshness,
}

/// Cortex `freshness_state`: workspace signature and HEAD against what the
/// last successful run recorded.
pub fn freshness(idx: &Index, root: &Path) -> Result<Freshness, ProjectError> {
    let meta = |k: &str| -> rusqlite::Result<Option<String>> {
        idx.conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [k], |r| r.get(0))
            .optional()
    };
    let stored_signature = meta("workspace_signature")?;
    let stored_head = meta("git_head")?;
    let signature = workspace_signature(root, &walk(root).files);
    let head = git(root, &["rev-parse", "HEAD"]);
    let workspace_changed = stored_signature.as_deref().is_some_and(|s| s != signature);
    let head_changed = matches!((&head, &stored_head), (Some(h), Some(s)) if h != s);
    Ok(Freshness {
        workspace_changed,
        head_changed,
        stale: workspace_changed || head_changed || stored_signature.is_none(),
    })
}

/// `vex status [PATH]`.
pub fn status(home: &Path, start: &Path) -> Result<StatusReport, ProjectError> {
    let (id, root) = registered(home, start)?;
    let path = index_path(home, &id);
    let idx = Index::open(&path)?;
    let count = |sql: &str| -> rusqlite::Result<i64> { idx.conn.query_row(sql, [], |r| r.get(0)) };
    let mut edges = std::collections::BTreeMap::new();
    {
        let mut stmt = idx
            .conn
            .prepare("SELECT type, COUNT(*) FROM edges GROUP BY type")?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
            let (t, n) = row?;
            edges.insert(t, n);
        }
    }
    let has_vec = idx
        .conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE name = 'chunks_vec'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    let meta = |k: &str| -> rusqlite::Result<Option<String>> {
        idx.conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [k], |r| r.get(0))
            .optional()
    };
    Ok(StatusReport {
        schema: crate::SCHEMA,
        project_id: id,
        index_path: path.clone(),
        files: count("SELECT COUNT(*) FROM files")?,
        chunks: count("SELECT COUNT(*) FROM chunks")?,
        symbols: count("SELECT COUNT(*) FROM symbols")?,
        edges,
        embedded_chunks: if has_vec {
            count("SELECT COUNT(*) FROM chunks_vec")?
        } else {
            0
        },
        indexed_head: meta("git_head")?,
        index_version: meta("index_version")?,
        freshness: freshness(&idx, &root)?,
        root,
    })
}

#[derive(Debug, Serialize)]
pub struct ForgetReport {
    pub schema: u32,
    pub project_id: String,
    pub root: PathBuf,
    pub removed_index: PathBuf,
}

/// `vex forget [PATH]`: drop the registry entry and the index directory.
pub fn forget(home: &Path, start: &Path) -> Result<ForgetReport, ProjectError> {
    let root = project_root(start)?;
    let mut registry = Registry::load(home);
    let id = registry
        .find_by_root(&root)
        .map(String::from)
        .ok_or(ProjectError::NoIndex { root: root.clone() })?;
    registry.forget(&id);
    registry
        .save()
        .map_err(|e| ProjectError::Store(e.to_string()))?;
    let dir = project_dir(home, &id);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(ForgetReport {
        schema: crate::SCHEMA,
        project_id: id,
        root,
        removed_index: dir,
    })
}
