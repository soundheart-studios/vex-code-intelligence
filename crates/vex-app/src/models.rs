//! `vex models list / pull / unload`: the one model vex hosts (the bge-small
//! embedder; there is no selector model), its pinned weights and its
//! idle-unload timer. A one-shot CLI process never keeps the model
//! resident, so `loaded` is false and `unload` has nothing to drop until
//! the daemon (P7) holds it.

use crate::embed::{Backend, Settings};
use crate::project::ProjectError;
use serde::Serialize;
use std::path::{Path, PathBuf};
use vex_embed::weights::{self, WeightsError};
use vex_embed::{BGE_DIM, BGE_MODEL};

#[derive(Debug, Serialize)]
pub struct ModelInfo {
    pub name: &'static str,
    pub role: &'static str,
    pub backend: &'static str,
    pub dim: usize,
    pub repo: &'static str,
    pub revision: &'static str,
    pub path: PathBuf,
    /// Every pinned file is present.
    pub downloaded: bool,
    /// Every pinned file hashes to its pin.
    pub verified: bool,
    pub bytes: u64,
    /// The configured backend uses this model.
    pub active: bool,
    pub loaded: bool,
    pub idle_unload_s: u64,
    pub idle_remaining_s: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct ListReport {
    pub schema: u32,
    /// bge, hash or none (VEX_EMBED_BACKEND).
    pub embed_backend: &'static str,
    pub models: Vec<ModelInfo>,
}

fn backend_name(b: Backend) -> &'static str {
    match b {
        Backend::Bge => "bge",
        Backend::Hash => "hash",
        Backend::None => "none",
    }
}

/// `vex models list`.
pub fn list(settings: &Settings) -> ListReport {
    let dir = weights::bge_dir(&settings.models);
    let present: Vec<&weights::ModelFile> = weights::BGE_FILES
        .iter()
        .filter(|f| dir.join(f.name).is_file())
        .collect();
    let downloaded = present.len() == weights::BGE_FILES.len();
    let bytes = present
        .iter()
        .filter_map(|f| std::fs::metadata(dir.join(f.name)).ok())
        .map(|m| m.len())
        .sum();
    ListReport {
        schema: crate::SCHEMA,
        embed_backend: backend_name(settings.backend),
        models: vec![ModelInfo {
            name: BGE_MODEL,
            role: "embedder",
            backend: "ort",
            dim: BGE_DIM,
            repo: weights::BGE_REPO,
            revision: weights::BGE_REVISION,
            verified: downloaded && weights::verify_files(&dir, weights::BGE_FILES).is_ok(),
            path: dir,
            downloaded,
            bytes,
            active: settings.backend == Backend::Bge,
            loaded: false,
            idle_unload_s: settings.idle.as_secs(),
            idle_remaining_s: None,
        }],
    }
}

#[derive(Debug, Serialize)]
pub struct PulledFile {
    pub name: &'static str,
    pub sha256: &'static str,
    pub bytes: u64,
    /// `downloaded`, or `verified` when the file was already in place.
    pub action: &'static str,
}

#[derive(Debug, Serialize)]
pub struct PullReport {
    pub schema: u32,
    pub model: &'static str,
    pub path: PathBuf,
    pub files: Vec<PulledFile>,
}

fn weights_error(e: WeightsError) -> ProjectError {
    match e {
        WeightsError::Checksum { .. } => ProjectError::ModelVerification(e.to_string()),
        _ => ProjectError::ModelDownload(e.to_string()),
    }
}

/// `vex models pull`: fetch and verify every pinned file now. A file whose
/// bytes do not hash to its pin is refused and never stored.
pub fn pull(settings: &Settings) -> Result<PullReport, ProjectError> {
    let dir = weights::bge_dir(&settings.models);
    let before: Vec<bool> = weights::BGE_FILES
        .iter()
        .map(|f| weights::verify_files(&dir, std::slice::from_ref(f)).is_ok())
        .collect();
    weights::ensure_bge(&settings.models, settings.model_source.as_deref())
        .map_err(weights_error)?;
    Ok(PullReport {
        schema: crate::SCHEMA,
        model: BGE_MODEL,
        files: weights::BGE_FILES
            .iter()
            .zip(before)
            .map(|(f, had)| PulledFile {
                name: f.name,
                sha256: f.sha256,
                bytes: f.size,
                action: if had { "verified" } else { "downloaded" },
            })
            .collect(),
        path: dir,
    })
}

/// Download whatever the bge weights still lack, reporting progress; the
/// bytes fetched (0 when every file was already in place).
pub fn prefetch(settings: &Settings) -> Result<u64, ProjectError> {
    let missing = weights::bge_missing_bytes(&settings.models);
    if missing == 0 {
        return Ok(0);
    }
    let downloading = crate::progress::stage(crate::progress::Stage::Download, missing);
    weights::ensure_bge_with(&settings.models, settings.model_source.as_deref(), &|n| {
        downloading.advance(n)
    })
    .map_err(weights_error)?;
    Ok(missing)
}

#[derive(Debug, Serialize)]
pub struct UnloadReport {
    pub schema: u32,
    pub model: &'static str,
    pub unloaded: bool,
    pub reason: String,
}

/// `vex models unload`.
pub fn unload(_home: &Path) -> UnloadReport {
    UnloadReport {
        schema: crate::SCHEMA,
        model: BGE_MODEL,
        unloaded: false,
        reason: "not loaded: a one-shot vex process never keeps the model resident".into(),
    }
}
