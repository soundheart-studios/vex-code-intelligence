//! One reply per command, shared by every adapter.
//!
//! The CLI's `--json` output and the MCP server's tool results are the same
//! text by construction: both turn a command's result into a [`Reply`] here.
//! A reply is one line of compact JSON carrying `"schema": 1`, a status the
//! CLI maps to its exit code, and, for a refusal or a failure to run, the
//! error code and message the MCP server shows as `VEX ERROR (code): ...`.

use crate::project::ProjectError;
use crate::pyjson::{self, Json};
use crate::search::Answer;
use serde::Serialize;
use std::path::{Path, PathBuf};
use vex_store::registry::Registry;

/// How a command ended; the CLI maps it to an exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    /// The request was refused or found nothing to act on (exit 1).
    Refused,
    /// The project has no index (exit 2).
    NoIndex,
    /// Answered from an index behind the working tree (exit 3).
    Stale,
    /// The command could not run (exit 4).
    Runtime,
}

/// A command's reply: the exact `--json` text and its status.
#[derive(Debug, Clone)]
pub struct Reply {
    pub text: String,
    pub status: Status,
    /// `(code, message)` when the reply is an error document.
    pub error: Option<(String, String)>,
}

const STYLE: pyjson::Style = pyjson::Style {
    ensure_ascii: false,
    compact: true,
};

impl Reply {
    fn doc(doc: &Json, status: Status) -> Reply {
        let error = match doc {
            Json::Obj(pairs) if pairs.len() == 3 => match (doc.get("error"), doc.get("message")) {
                (Some(Json::Str(code)), Some(Json::Str(message))) => {
                    Some((code.clone(), message.clone()))
                }
                _ => None,
            },
            _ => None,
        };
        Reply {
            text: pyjson::dumps(doc, STYLE),
            status,
            error,
        }
    }

    /// `{"schema": 1, "error": code, "message": message}`.
    pub fn error(code: &str, message: &str, status: Status) -> Reply {
        Reply::doc(
            &Json::obj(vec![
                ("schema", Json::from(crate::SCHEMA as i64)),
                ("error", Json::from(code)),
                ("message", Json::from(message)),
            ]),
            status,
        )
    }

    /// Neither VEX_HOME nor HOME is set.
    pub fn no_home() -> Reply {
        Reply::error(
            "no_home",
            "neither VEX_HOME nor HOME is set",
            Status::Runtime,
        )
    }

    /// The project could not be opened.
    pub fn project_error(e: &ProjectError) -> Reply {
        let (code, status) = match e {
            ProjectError::NoIndex { .. } => ("no_index", Status::NoIndex),
            ProjectError::BadPath { .. } => ("bad_path", Status::Refused),
            ProjectError::Store(_) => ("store", Status::Runtime),
            ProjectError::Agent(_) => ("agent_config", Status::Refused),
            ProjectError::Embedding(_) => ("embedding_mismatch", Status::Refused),
            ProjectError::ModelVerification(_) => ("model_verification", Status::Refused),
            ProjectError::ModelDownload(_) => ("model_download", Status::Runtime),
        };
        Reply::error(code, &e.to_string(), status)
    }
}

/// A command outcome that has a `--json` document and a status.
pub trait Reportable {
    fn cli_json(&self) -> Json;
    fn status(&self) -> Status;
}

fn answer_status(answer: &Answer, stale: bool) -> Status {
    match answer {
        Answer::Error { .. } => Status::Refused,
        _ if stale => Status::Stale,
        _ => Status::Ok,
    }
}

impl Reportable for crate::search::Outcome {
    fn cli_json(&self) -> Json {
        self.answer.cli_json()
    }
    fn status(&self) -> Status {
        answer_status(&self.answer, self.freshness.index_stale)
    }
}

impl Reportable for crate::expand::Outcome {
    fn cli_json(&self) -> Json {
        crate::expand::Outcome::cli_json(self)
    }
    fn status(&self) -> Status {
        answer_status(&self.answer, self.freshness.index_stale)
    }
}

impl Reportable for crate::graph::Outcome {
    fn cli_json(&self) -> Json {
        crate::graph::Outcome::cli_json(self)
    }
    fn status(&self) -> Status {
        answer_status(&self.answer, self.freshness.index_stale)
    }
}

impl Reportable for crate::grep::Outcome {
    fn cli_json(&self) -> Json {
        crate::grep::Outcome::cli_json(self)
    }
    fn status(&self) -> Status {
        if self.refused() {
            Status::Refused
        } else if self.freshness.index_stale {
            Status::Stale
        } else {
            Status::Ok
        }
    }
}

impl Reportable for crate::map::Outcome {
    fn cli_json(&self) -> Json {
        crate::map::Outcome::cli_json(self)
    }
    fn status(&self) -> Status {
        if self.freshness.index_stale {
            Status::Stale
        } else {
            Status::Ok
        }
    }
}

impl Reportable for crate::changes::Outcome {
    fn cli_json(&self) -> Json {
        crate::changes::Outcome::cli_json(self)
    }
    fn status(&self) -> Status {
        if self.freshness.as_ref().is_some_and(|f| f.index_stale) {
            Status::Stale
        } else {
            Status::Ok
        }
    }
}

/// The reply for a query command's result.
pub fn reply<T: Reportable>(result: &Result<T, ProjectError>) -> Reply {
    match result {
        Ok(out) => Reply::doc(&out.cli_json(), out.status()),
        Err(e) => Reply::project_error(e),
    }
}

/// The reply for a project command's serialisable report (index, status,
/// projects): compact JSON in field order, so `schema` comes first.
pub fn reply_serde<T: Serialize>(result: &Result<T, ProjectError>) -> Reply {
    match result {
        Ok(report) => Reply {
            text: serde_json::to_string(report).expect("reports serialise"),
            status: Status::Ok,
            error: None,
        },
        Err(e) => Reply::project_error(e),
    }
}

/// One registered project, as `vex projects` and `list_projects` show it.
#[derive(Debug, Serialize)]
pub struct ProjectEntry {
    pub project_id: String,
    pub name: String,
    pub root: PathBuf,
    pub remote: Option<String>,
    /// Whether the root still exists on disk.
    pub present: bool,
}

/// Every registered project, by id.
#[derive(Debug, Serialize)]
pub struct ProjectsReport {
    pub schema: u32,
    pub projects: Vec<ProjectEntry>,
}

/// `vex projects` / `list_projects`.
pub fn projects(home: &Path) -> Result<ProjectsReport, ProjectError> {
    let registry = Registry::load(home);
    Ok(ProjectsReport {
        schema: crate::SCHEMA,
        projects: registry
            .projects()
            .map(|(id, p)| ProjectEntry {
                project_id: id.clone(),
                name: p.name.clone(),
                present: p.root.is_dir(),
                root: p.root.clone(),
                remote: p.remote.clone(),
            })
            .collect(),
    })
}

/// Where a tool's `project` argument points: the default directory when
/// empty, else a registered project named by id or display name (a unique
/// match), else a path.
pub fn resolve_project(home: &Path, default: &Path, project: &str) -> Result<PathBuf, Reply> {
    let project = project.trim();
    if project.is_empty() {
        return Ok(default.to_path_buf());
    }
    let registry = Registry::load(home);
    if let Some(p) = registry.get(project) {
        return Ok(p.root.clone());
    }
    let named: Vec<&PathBuf> = registry
        .projects()
        .filter(|(_, p)| p.name == project)
        .map(|(_, p)| &p.root)
        .collect();
    match named.as_slice() {
        [root] => return Ok((*root).clone()),
        [] => {}
        _ => {
            return Err(Reply::error(
                "invalid_request",
                &format!(
                    "project name {} is ambiguous ({} registered roots); pass its id or path",
                    pyjson::repr_str(project),
                    named.len()
                ),
                Status::Refused,
            ));
        }
    }
    let path = if Path::new(project).is_absolute() {
        PathBuf::from(project)
    } else {
        default.join(project)
    };
    if path.exists() {
        Ok(path)
    } else {
        Err(Reply::error(
            "not_found",
            &format!(
                "unknown project {}; call list_projects() for the registered ones",
                pyjson::repr_str(project)
            ),
            Status::Refused,
        ))
    }
}
