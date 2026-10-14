//! Query-time freshness (Cortex `application/freshness.py`): a stale answer
//! always announces itself. Never fails a query: a probe that errors reads
//! as fresh, as in Cortex.
//!
//! The in-process CLI has no reindex coordinator, so a stale verdict says
//! `no_dispatcher`; the daemon supplies one and reports `dispatched`.

use crate::pyjson::Json;
use std::path::Path;
use vex_store::db::Index;

/// How a refresh was (or was not) started for a stale index.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Reindex {
    NotNeeded,
    NoDispatcher,
    Dispatched,
    InFlight,
    Cooldown,
    Disabled,
}

impl Reindex {
    fn as_str(self) -> &'static str {
        match self {
            Reindex::NotNeeded => "not_needed",
            Reindex::NoDispatcher => "no_dispatcher",
            Reindex::Dispatched => "dispatched",
            Reindex::InFlight => "in_flight",
            Reindex::Cooldown => "cooldown",
            Reindex::Disabled => "disabled",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub index_stale: bool,
    pub index_age_s: Option<f64>,
    pub reindex: Reindex,
}

impl Verdict {
    pub fn to_json(&self) -> Json {
        Json::obj(vec![
            ("index_stale", Json::from(self.index_stale)),
            ("index_age_s", Json::from(self.index_age_s)),
            ("reindex", Json::from(self.reindex.as_str())),
        ])
    }
}

/// Cortex's probe: HEAD moved, or the workspace signature differs from the
/// recorded one. An index with no recorded signature is not called stale.
pub fn probe(idx: &Index, root: &Path) -> bool {
    match crate::project::freshness(idx, root) {
        Ok(f) => f.head_changed || f.workspace_changed,
        Err(_) => false,
    }
}

/// Why a refresh is asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// A query found the index stale.
    Query,
    /// Files changed (a watcher event, a git hook's `vex sync`).
    Edit,
    /// Changes may have been missed: hash everything.
    Rescan,
}

/// Starts a refresh (Cortex's injected `dispatch`): given the index's path,
/// the project root and why, it says what it did.
pub type Dispatcher = std::sync::Arc<dyn Fn(&Path, &Path, Trigger) -> Reindex + Send + Sync>;

static DISPATCHER: std::sync::RwLock<Option<Dispatcher>> = std::sync::RwLock::new(None);

/// Install (or remove) the process's refresh dispatcher. The daemon
/// installs its scheduler; the in-process CLI has none.
pub fn set_dispatcher(d: Option<Dispatcher>) {
    *DISPATCHER.write().unwrap_or_else(|p| p.into_inner()) = d;
}

/// Hand a refresh to the installed dispatcher; None when there is none
/// (the in-process CLI).
pub fn dispatch(index: &Path, root: &Path, trigger: Trigger) -> Option<Reindex> {
    let d = DISPATCHER
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .clone()?;
    Some(d(index, root, trigger))
}

/// Told of every project a query checks (the daemon starts watching it).
pub type Observer = std::sync::Arc<dyn Fn(&Path, &Path) + Send + Sync>;

static OBSERVER: std::sync::RwLock<Option<Observer>> = std::sync::RwLock::new(None);

/// Install (or remove) the process's project observer.
pub fn set_observer(o: Option<Observer>) {
    *OBSERVER.write().unwrap_or_else(|p| p.into_inner()) = o;
}

/// The verdict for one query; a stale index is handed to the dispatcher.
pub fn check(idx: &Index, root: &Path) -> Verdict {
    let observer = OBSERVER.read().unwrap_or_else(|p| p.into_inner()).clone();
    if let Some(o) = observer {
        o(&idx.path, root);
    }
    let stale = probe(idx, root);
    let reindex = if !stale {
        Reindex::NotNeeded
    } else {
        let dispatcher = DISPATCHER.read().unwrap_or_else(|p| p.into_inner()).clone();
        match dispatcher {
            Some(d) => d(&idx.path, root, Trigger::Query),
            None => Reindex::NoDispatcher,
        }
    };
    Verdict {
        index_stale: stale,
        index_age_s: None,
        reindex,
    }
}
