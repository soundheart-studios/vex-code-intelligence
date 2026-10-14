//! Embedding: which backend a command uses, the cached batch embedder the
//! pipeline and search share, and `vex embed` (Cortex `Pipeline.drain_pending`).
//!
//! Vectors are cached globally (VEX_HOME/cache/embeddings.db) by model and
//! content, so identical content is embedded once per machine. When the
//! model cannot load, vex degrades to keyword-only and says so; unlike
//! Cortex it never falls back to hash vectors, which rank by something the
//! query does not share.

use crate::project::{ProjectError, registered};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use vex_embed::{BgeEmbedder, Embedder, HashEmbedder};
use vex_store::cache::{self, EmbeddingCache};
use vex_store::db::Index;
use vex_store::registry::index_path;
use vex_store::vec;

/// Cortex's embed batch size. vex hands the embedder its own
/// `preferred_batch` instead (`Session::group`); tests replaying Cortex's
/// drain still use this.
pub const BATCH: usize = 64;
/// A chunk's cost to embed, in characters: the model sees no more than its
/// 512-token window, roughly this much code.
pub const COST_CAP: i64 = 2048;

static BACKGROUND: AtomicBool = AtomicBool::new(false);

/// Mark this process as background work (the daemon): unless
/// VEX_EMBED_WORKERS says otherwise, it runs half the model sessions a
/// foreground command would, so the machine stays responsive.
pub fn set_background(background: bool) {
    BACKGROUND.store(background, Ordering::Relaxed);
}

/// The background session count: half the default, at least one.
pub fn background_workers() -> usize {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    (vex_embed::batching::default_workers(cores) / 2).max(1)
}

/// How long an idle bge model stays loaded.
pub const DEFAULT_IDLE: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// bge-small-en-v1.5 in-process (the default).
    Bge,
    /// Cortex's deterministic hash vectors: offline, not semantic.
    Hash,
    /// Keyword-only by choice: no vectors, no notice.
    None,
}

impl Backend {
    pub fn parse(s: &str) -> Option<Backend> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "bge" | "auto" => Some(Backend::Bge),
            "hash" => Some(Backend::Hash),
            "none" | "off" => Some(Backend::None),
            _ => None,
        }
    }

    /// The provenance stamp's backend name.
    pub fn name(self) -> &'static str {
        match self {
            Backend::Bge => "ort",
            Backend::Hash => "hash",
            Backend::None => "none",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub backend: Backend,
    /// VEX_HOME/models.
    pub models: PathBuf,
    /// VEX_HOME/cache/embeddings.db.
    pub cache: PathBuf,
    /// Where weights download from, instead of the hub.
    pub model_source: Option<String>,
    pub idle: Duration,
    /// Model sessions run side by side; 0 picks one per two cores (half
    /// that in the background, see `set_background`).
    pub workers: usize,
}

impl Settings {
    /// From the environment: VEX_EMBED_BACKEND (bge | hash | none),
    /// VEX_MODEL_BASE_URL, VEX_EMBED_IDLE_SECS and VEX_EMBED_WORKERS.
    pub fn from_env(home: &Path) -> Settings {
        let backend = std::env::var("VEX_EMBED_BACKEND")
            .ok()
            .and_then(|b| Backend::parse(&b))
            .unwrap_or(Backend::Bge);
        let idle = std::env::var("VEX_EMBED_IDLE_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_IDLE);
        Settings {
            backend,
            models: home.join("models"),
            cache: home.join("cache").join("embeddings.db"),
            model_source: std::env::var("VEX_MODEL_BASE_URL")
                .ok()
                .filter(|s| !s.is_empty()),
            idle,
            workers: std::env::var("VEX_EMBED_WORKERS")
                .ok()
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or_else(|| {
                    if BACKGROUND.load(Ordering::Relaxed) {
                        background_workers()
                    } else {
                        0
                    }
                }),
        }
    }
}

/// How an indexing command embeds: the settings, and whether to leave every
/// vector for `vex embed` (`--defer-embed`).
#[derive(Debug, Clone)]
pub struct Plan {
    pub settings: Settings,
    pub defer: bool,
}

impl Plan {
    pub fn from_env(home: &Path, defer: bool) -> Plan {
        Plan {
            settings: Settings::from_env(home),
            defer,
        }
    }
}

/// What an index run did with vectors.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct EmbedReport {
    pub backend: String,
    pub model: String,
    /// Vectors written by this run.
    pub embedded: usize,
    /// Chunks in the index still without a vector.
    pub pending: usize,
    pub cache_hits: usize,
    pub cache_misses: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
}

type BgeKey = (PathBuf, Option<String>, Duration, usize);

fn bge_registry() -> &'static Mutex<HashMap<BgeKey, Arc<BgeEmbedder>>> {
    static SHARED: OnceLock<Mutex<HashMap<BgeKey, Arc<BgeEmbedder>>>> = OnceLock::new();
    SHARED.get_or_init(Default::default)
}

/// The process's bge embedder for these settings: one per models dir,
/// source, idle timer and worker count, so a long-lived process (the daemon) keeps the
/// model loaded across requests until its idle timer drops it.
fn shared_bge(settings: &Settings) -> Arc<dyn Embedder> {
    let key = (
        settings.models.clone(),
        settings.model_source.clone(),
        settings.idle,
        settings.workers,
    );
    let mut map = bge_registry().lock().unwrap_or_else(|p| p.into_inner());
    map.entry(key)
        .or_insert_with(|| {
            let bge = BgeEmbedder::with_source(
                &settings.models,
                settings.idle,
                settings.model_source.clone(),
            );
            Arc::new(match settings.workers {
                0 => bge,
                n => bge.with_workers(n),
            })
        })
        .clone()
}

/// Every bge embedder this process holds: (models dir, loaded, time left
/// before its idle unload).
pub fn loaded_models() -> Vec<(PathBuf, bool, Option<Duration>)> {
    let map = bge_registry().lock().unwrap_or_else(|p| p.into_inner());
    map.iter()
        .map(|((dir, ..), e)| (dir.clone(), e.is_loaded(), e.idle_remaining()))
        .collect()
}

/// Drop every loaded model now; how many were loaded.
pub fn unload_models() -> usize {
    let map = bge_registry().lock().unwrap_or_else(|p| p.into_inner());
    map.values()
        .filter(|e| {
            let was = e.is_loaded();
            e.unload();
            was
        })
        .count()
}

/// One command's embedder plus the global cache. After the first failure
/// it embeds nothing more and remembers why.
pub struct Session {
    embedder: Arc<dyn Embedder>,
    backend: Backend,
    cache: Option<EmbeddingCache>,
    pub hits: usize,
    pub misses: usize,
    pub failure: Option<String>,
}

impl Session {
    /// None when embedding is off. Nothing loads until the first embed.
    pub fn open(settings: &Settings) -> Option<Session> {
        let embedder: Arc<dyn Embedder> = match settings.backend {
            Backend::None => return None,
            Backend::Hash => Arc::new(HashEmbedder),
            Backend::Bge => shared_bge(settings),
        };
        Some(Session {
            embedder,
            backend: settings.backend,
            // A cache that cannot open costs re-embedding, never correctness.
            cache: EmbeddingCache::open(&settings.cache).ok(),
            hits: 0,
            misses: 0,
            failure: None,
        })
    }

    /// A session over a given embedder (tests count or fail its calls).
    pub fn with_embedder(
        embedder: Box<dyn Embedder>,
        backend: Backend,
        cache: Option<&Path>,
    ) -> Session {
        Session {
            embedder: Arc::from(embedder),
            backend,
            cache: cache.and_then(|p| EmbeddingCache::open(p).ok()),
            hits: 0,
            misses: 0,
            failure: None,
        }
    }

    pub fn backend(&self) -> Backend {
        self.backend
    }

    pub fn model_name(&self) -> &str {
        self.embedder.model_name()
    }

    pub fn dim(&self) -> usize {
        self.embedder.dim()
    }

    /// How many texts to hand `embed` at once (the embedder's preference).
    pub fn group(&self) -> usize {
        self.embedder.preferred_batch().max(1)
    }

    /// One vector per text, cache first and every miss in one model call
    /// (Cortex `Embedder.embed`). None once the model has failed.
    pub fn embed(&mut self, texts: &[&str]) -> Option<Vec<Vec<f32>>> {
        if self.failure.is_some() {
            return None;
        }
        let model = self.embedder.model_name().to_string();
        let dim = self.embedder.dim();
        let keys: Vec<String> = texts.iter().map(|t| cache::key(&model, t)).collect();
        let mut out: Vec<Option<Vec<f32>>> = keys
            .iter()
            .map(|k| {
                self.cache
                    .as_ref()
                    .and_then(|c| c.get(k, dim).ok().flatten())
            })
            .collect();
        let todo: Vec<usize> = (0..texts.len()).filter(|&i| out[i].is_none()).collect();
        self.hits += texts.len() - todo.len();
        self.misses += todo.len();
        if !todo.is_empty() {
            let batch: Vec<&str> = todo.iter().map(|&i| texts[i]).collect();
            let computed = match self.embedder.embed(&batch) {
                Ok(v) => v,
                Err(e) => {
                    self.failure = Some(e.to_string());
                    return None;
                }
            };
            if let Some(c) = &self.cache {
                let rows: Vec<(String, &[f32])> = todo
                    .iter()
                    .zip(&computed)
                    .map(|(&i, v)| (keys[i].clone(), v.as_slice()))
                    .collect();
                let _ = c.put_many(&rows);
            }
            for (&i, v) in todo.iter().zip(computed) {
                out[i] = Some(v);
            }
        }
        Some(out.into_iter().map(|v| v.unwrap_or_default()).collect())
    }

    /// The keyword-only notice for an index that could not embed `pending`
    /// chunks, None when the model is healthy.
    pub fn index_notice(&self, pending: usize) -> Option<String> {
        self.failure.as_ref().map(|why| {
            format!(
                "keyword-only: {pending} chunks have no vectors because the embedding model is \
                 unavailable ({why}). Search still works by text match; run `vex embed` once the \
                 model can load (`vex models pull` fetches it)."
            )
        })
    }

    /// The keyword-only notice for a search, None when the model is healthy.
    pub fn search_notice(&self) -> Option<String> {
        self.failure.as_ref().map(|why| {
            format!(
                "keyword-only: the embedding model is unavailable ({why}), so results are ranked \
                 by text match alone. Run `vex models pull` when online, then `vex embed`."
            )
        })
    }
}

/// `vex embed`'s result.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FillReport {
    pub schema: u32,
    pub project_id: String,
    pub root: PathBuf,
    pub embedded: usize,
    pub remaining: usize,
    pub cache_hits: usize,
    pub cache_misses: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
}

fn width(e: vec::VecError) -> ProjectError {
    match e {
        vec::VecError::Sql(e) => e.into(),
        other => ProjectError::Embedding(other.to_string()),
    }
}

/// Embed every chunk still without a vector in an open index, in batches,
/// stopping at the first model failure (Cortex `drain_pending`). Longest
/// content first, so each batch holds texts of like length.
pub fn drain(idx: &Index, session: &mut Session) -> Result<usize, ProjectError> {
    vec::assert_compatible(idx, session.dim()).map_err(width)?;
    let mut done = 0;
    let pending = vec::count_pending(&idx.conn).unwrap_or(0) as u64;
    let embedding =
        (pending > 0).then(|| crate::progress::stage(crate::progress::Stage::Embed, pending));
    loop {
        let batch = vec::pending_longest(&idx.conn, session.group())?;
        if batch.is_empty() {
            break;
        }
        let texts: Vec<&str> = batch.iter().map(|(_, c)| c.as_str()).collect();
        let Some(vecs) = session.embed(&texts) else {
            break;
        };
        let rows: Vec<(i64, Vec<f32>)> = batch.iter().map(|(r, _)| *r).zip(vecs).collect();
        vec::set_embeddings(idx, &rows).map_err(width)?;
        done += rows.len();
        if let Some(e) = &embedding {
            e.advance(rows.len() as u64);
        }
    }
    Ok(done)
}

/// What timing the model on a sample found.
#[derive(Debug, Clone, Copy)]
pub struct Sample {
    pub embedded: usize,
    pub took: Duration,
    /// The sample's chunks' lengths, each capped at `COST_CAP`.
    pub cost: i64,
}

/// Embed up to `n` pending chunks, longest first, and time it: a
/// pessimistic sample, since what is left is shorter.
pub fn sample(idx: &Index, session: &mut Session, n: usize) -> Result<Sample, ProjectError> {
    vec::assert_compatible(idx, session.dim()).map_err(width)?;
    let batch = vec::pending_longest(&idx.conn, n)?;
    let measuring = crate::progress::stage(crate::progress::Stage::Measure, batch.len() as u64);
    let started = std::time::Instant::now();
    let texts: Vec<&str> = batch.iter().map(|(_, c)| c.as_str()).collect();
    let Some(vecs) = session.embed(&texts) else {
        return Ok(Sample {
            embedded: 0,
            took: started.elapsed(),
            cost: 0,
        });
    };
    let rows: Vec<(i64, Vec<f32>)> = batch.iter().map(|(r, _)| *r).zip(vecs).collect();
    vec::set_embeddings(idx, &rows).map_err(width)?;
    let took = started.elapsed();
    measuring.advance(rows.len() as u64);
    Ok(Sample {
        embedded: rows.len(),
        took,
        cost: texts.iter().map(|t| (t.len() as i64).min(COST_CAP)).sum(),
    })
}

/// `vex embed [PATH]`: fill the vectors a deferred or offline index lacks.
pub fn fill(home: &Path, start: &Path, settings: &Settings) -> Result<FillReport, ProjectError> {
    let (id, root) = registered(home, start)?;
    let idx = Index::open(&index_path(home, &id))?;
    let _writer = crate::project::lock_index(&idx.path, crate::project::WRITE_WAIT)?;
    let Some(mut session) = Session::open(settings) else {
        let remaining = vec::count_pending(&idx.conn)? as usize;
        return Ok(FillReport {
            schema: crate::SCHEMA,
            project_id: id,
            root,
            embedded: 0,
            remaining,
            cache_hits: 0,
            cache_misses: 0,
            notice: Some(
                "embedding is off (VEX_EMBED_BACKEND=none); the index stays keyword-only"
                    .to_string(),
            ),
        });
    };
    let embedded = drain(&idx, &mut session)?;
    let remaining = vec::count_pending(&idx.conn)? as usize;
    Ok(FillReport {
        schema: crate::SCHEMA,
        project_id: id,
        root,
        embedded,
        remaining,
        cache_hits: session.hits,
        cache_misses: session.misses,
        notice: session.index_notice(remaining),
    })
}
