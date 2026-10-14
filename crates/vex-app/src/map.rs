//! `vex map`: the PageRank-ranked orientation map (Cortex's
//! application/maps.py, without its savings ledger).

use crate::freshness;
use crate::pyjson::Json;
use std::path::Path;
use vex_search::repomap;
use vex_store::db::Index;

pub use repomap::DEFAULT_MAP_TOKEN_BUDGET;

/// A map and the files it summarizes.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// Cortex's map tool output, verbatim.
    pub text: String,
    pub included_files: Vec<String>,
    pub freshness: freshness::Verdict,
}

impl Outcome {
    /// The CLI's `--json` document, with a `freshness` block when stale.
    pub fn cli_json(&self) -> Json {
        let mut doc = Json::obj(vec![
            ("schema", Json::from(crate::SCHEMA as i64)),
            ("map", Json::from(self.text.as_str())),
            (
                "included_files",
                Json::Arr(
                    self.included_files
                        .iter()
                        .map(|f| Json::from(f.as_str()))
                        .collect(),
                ),
            ),
        ]);
        if self.freshness.index_stale {
            doc.set("freshness", self.freshness.to_json());
        }
        doc
    }
}

/// The map text and included files for an open index.
pub fn build(
    idx: &Index,
    budget_tokens: i64,
    focus: &[String],
) -> rusqlite::Result<(String, Vec<String>)> {
    repomap::repo_map(&idx.conn, budget_tokens, focus)
}

/// `vex map` in the registered project containing `start`.
pub fn run(
    home: &Path,
    start: &Path,
    budget_tokens: i64,
    focus: &[String],
) -> Result<Outcome, crate::project::ProjectError> {
    let (id, root) = crate::project::registered(home, start)?;
    let idx = Index::open(&vex_store::registry::index_path(home, &id))?;
    let freshness = freshness::check(&idx, &root);
    let (text, included_files) = build(&idx, budget_tokens, focus)?;
    Ok(Outcome {
        text,
        included_files,
        freshness,
    })
}
