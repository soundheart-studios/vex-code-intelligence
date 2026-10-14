//! `vex sync`: bring a project's index up to date now. Under the daemon it
//! asks the scheduler for an incremental refresh and returns at once
//! (--wait refreshes before answering); in-process it runs an incremental
//! scan as the index's one writer. --record-head only stamps HEAD, which is
//! all a commit moves. Git hooks call it (see hooks.rs).

use crate::embed::{Session, Settings};
use crate::freshness::{Reindex, Trigger};
use crate::index::{IndexReport, Scan, run_scan};
use crate::project::{ProjectError, WRITE_WAIT, lock_index, registered};
use serde::Serialize;
use std::path::{Path, PathBuf};
use vex_store::db::Index;
use vex_store::registry::index_path;

#[derive(Debug, Clone, Default)]
pub struct Request {
    /// Changed paths, as a hint: the incremental scan finds every change by
    /// file stats regardless.
    pub paths: Vec<String>,
    /// Refresh before answering even when a daemon could do it later.
    pub wait: bool,
    /// Only record HEAD (after a commit).
    pub record_head: bool,
}

#[derive(Debug, Serialize)]
pub struct SyncReport {
    pub schema: u32,
    pub project_id: String,
    pub root: PathBuf,
    /// in_process, dispatched, in_flight, cooldown or recorded_head.
    pub mode: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<IndexReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
}

fn git_head(root: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn sync(
    home: &Path,
    start: &Path,
    req: &Request,
    settings: &Settings,
) -> Result<SyncReport, ProjectError> {
    let (id, root) = registered(home, start)?;
    let index = index_path(home, &id);
    let report = |mode, index, head| SyncReport {
        schema: crate::SCHEMA,
        project_id: id.clone(),
        root: root.clone(),
        mode,
        index,
        head,
    };
    if req.record_head {
        let head = git_head(&root);
        if let Some(h) = &head {
            let _writer = lock_index(&index, WRITE_WAIT)?;
            let idx = Index::open(&index)?;
            idx.conn.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES ('git_head', ?1)",
                [h],
            )?;
        }
        return Ok(report("recorded_head", None, head));
    }
    if !req.wait
        && let Some(r) = crate::freshness::dispatch(&index, &root, Trigger::Edit)
    {
        let mode = match r {
            Reindex::Dispatched => "dispatched",
            Reindex::InFlight => "in_flight",
            Reindex::Cooldown => "cooldown",
            _ => "dispatched",
        };
        return Ok(report(mode, None, None));
    }
    let _writer = lock_index(&index, WRITE_WAIT)?;
    let idx = Index::open(&index)?;
    let mut session = Session::open(settings);
    let r = run_scan(
        &idx,
        &root,
        false,
        session.as_mut(),
        false,
        Scan::Incremental,
    )?;
    Ok(report("in_process", Some(r), None))
}
