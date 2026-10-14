//! bge-small-en-v1.5 hosted in-process: fastembed 0.8.0's
//! `TextEmbedding("BAAI/bge-small-en-v1.5")` reproduced over statically
//! linked ONNX Runtime. Tokenizer set up as fastembed's `load_tokenizer`
//! does, inputs as its `onnx_embed` builds them, CLS pooling and L2
//! normalisation as its `_post_process_onnx_output`. Unlike fastembed, a
//! large call runs as length-sorted batches over several sessions
//! (`batching`); padding is masked, so the vectors are the same.

use crate::batching::{default_workers, fan_out, plan, threads_per_worker};
use crate::idle::IdleSlot;
use crate::weights;
use crate::{EmbedError, Embedder};
use ort::session::Session;
use ort::value::Tensor;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokenizers::{AddedToken, Encoding, Tokenizer, TruncationParams};

pub const BGE_MODEL: &str = "bge-small-en-v1.5";
pub const BGE_DIM: usize = 384;
pub const CONTEXT_TOKENS: usize = 512;
/// A batch's padded size (rows × its longest text) at most: 16 texts at
/// the full window, which bounds each session's activations.
const BATCH_TOKENS: usize = 16 * CONTEXT_TOKENS;
/// Rows per batch at most, however short the texts.
const BATCH_ROWS: usize = 64;

struct Inputs {
    attention_mask: bool,
    token_type_ids: bool,
    pad_id: u32,
}

struct Loaded {
    sessions: Vec<Session>,
    tokenizer: Tokenizer,
    inputs: Inputs,
}

pub struct BgeEmbedder {
    models: PathBuf,
    source: Option<String>,
    workers: usize,
    slot: IdleSlot<Loaded>,
}

fn cores() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

impl BgeEmbedder {
    /// Nothing is read or downloaded until the first embed.
    pub fn new(models: &Path, idle: Duration) -> Self {
        Self::with_source(models, idle, None)
    }

    /// Weights download from `source` instead of the hub.
    pub fn with_source(models: &Path, idle: Duration, source: Option<String>) -> Self {
        BgeEmbedder {
            models: models.to_path_buf(),
            source,
            workers: default_workers(cores()),
            slot: IdleSlot::new(idle),
        }
    }

    /// Run `workers` sessions (at least one) instead of one per two cores;
    /// each still gets two cores' worth of threads.
    pub fn with_workers(mut self, workers: usize) -> Self {
        self.workers = workers.max(1);
        self
    }

    pub fn workers(&self) -> usize {
        self.workers
    }

    pub fn is_loaded(&self) -> bool {
        self.slot.is_loaded()
    }

    pub fn idle_remaining(&self) -> Option<Duration> {
        self.slot.idle_remaining()
    }

    pub fn unload(&self) {
        self.slot.unload();
    }
}

fn json(dir: &Path, name: &str) -> Result<serde_json::Value, EmbedError> {
    let raw = std::fs::read_to_string(dir.join(name))
        .map_err(|e| EmbedError::Model(format!("{name}: {e}")))?;
    serde_json::from_str(&raw).map_err(|e| EmbedError::Model(format!("{name}: {e}")))
}

fn model_err(e: impl std::fmt::Display) -> EmbedError {
    EmbedError::Model(e.to_string())
}

/// fastembed `load_tokenizer`: truncation at min(model_max_length,
/// max_length) and every special token re-added. fastembed pads each batch
/// to its longest text with the config's pad id; `run_batch` does that
/// itself, after `plan` has grouped texts of like length, so the tokenizer
/// here does not pad.
fn tokenizer(dir: &Path) -> Result<(Tokenizer, u32), EmbedError> {
    let config = json(dir, "config.json")?;
    let tok_config = json(dir, "tokenizer_config.json")?;
    let specials = json(dir, "special_tokens_map.json")?;
    let max_context = match (
        tok_config["model_max_length"].as_u64(),
        tok_config["max_length"].as_u64(),
    ) {
        (Some(a), Some(b)) => a.min(b),
        (Some(a), None) | (None, Some(a)) => a,
        (None, None) => return Err(model_err("tokenizer_config.json has no max length")),
    };
    let mut tok = Tokenizer::from_file(dir.join("tokenizer.json")).map_err(model_err)?;
    tok.with_truncation(Some(TruncationParams {
        max_length: max_context as usize,
        ..Default::default()
    }))
    .map_err(model_err)?;
    let pad_id = match tok.get_padding() {
        Some(p) => p.pad_id,
        None => config["pad_token_id"].as_u64().unwrap_or(0) as u32,
    };
    tok.with_padding(None);
    let mut added = Vec::new();
    for token in specials.as_object().into_iter().flat_map(|m| m.values()) {
        if let Some(s) = token.as_str() {
            added.push(AddedToken::from(s.to_string(), true));
        } else if let Some(content) = token["content"].as_str() {
            let flag = |k: &str| token[k].as_bool().unwrap_or(false);
            added.push(
                AddedToken::from(content.to_string(), true)
                    .single_word(flag("single_word"))
                    .lstrip(flag("lstrip"))
                    .rstrip(flag("rstrip"))
                    .normalized(token["normalized"].as_bool().unwrap_or(true)),
            );
        }
    }
    tok.add_special_tokens(&added);
    Ok((tok, pad_id))
}

fn load(models: &Path, source: Option<&str>, workers: usize) -> Result<Loaded, EmbedError> {
    let dir = weights::ensure_bge(models, source)?;
    let (tokenizer, pad_id) = tokenizer(&dir)?;
    // Each session gets the share it would have at the default count, so
    // fewer sessions (the daemon's) leave cores free.
    let threads = threads_per_worker(cores(), default_workers(cores()));
    let sessions = (0..workers)
        .map(|_| {
            Session::builder()
                .map_err(model_err)?
                .with_intra_threads(threads)
                .map_err(model_err)?
                .commit_from_file(dir.join("model_optimized.onnx"))
                .map_err(model_err)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let names: Vec<String> = sessions[0]
        .inputs()
        .iter()
        .map(|i| i.name().to_string())
        .collect();
    Ok(Loaded {
        inputs: Inputs {
            attention_mask: names.iter().any(|n| n == "attention_mask"),
            token_type_ids: names.iter().any(|n| n == "token_type_ids"),
            pad_id,
        },
        sessions,
        tokenizer,
    })
}

/// One batch through one session: each row padded to the batch's longest
/// with the pad id and a zero mask (fastembed's batch-longest padding).
fn run_batch(
    session: &mut Session,
    inputs: &Inputs,
    encodings: &[&Encoding],
) -> Result<Vec<Vec<f32>>, EmbedError> {
    let rows = encodings.len();
    let cols = encodings
        .iter()
        .map(|e| e.get_ids().len())
        .max()
        .unwrap_or(0);
    let mut ids = Vec::with_capacity(rows * cols);
    let mut mask = Vec::with_capacity(rows * cols);
    for e in encodings {
        let pad = cols - e.get_ids().len();
        ids.extend(e.get_ids().iter().map(|&x| x as i64));
        ids.extend(std::iter::repeat_n(inputs.pad_id as i64, pad));
        mask.extend(e.get_attention_mask().iter().map(|&x| x as i64));
        mask.extend(std::iter::repeat_n(0, pad));
    }
    let shape = [rows as i64, cols as i64];
    let mut feed = vec![(
        "input_ids",
        Tensor::from_array((shape, ids)).map_err(model_err)?,
    )];
    if inputs.attention_mask {
        feed.push((
            "attention_mask",
            Tensor::from_array((shape, mask)).map_err(model_err)?,
        ));
    }
    if inputs.token_type_ids {
        feed.push((
            "token_type_ids",
            Tensor::from_array((shape, vec![0i64; rows * cols])).map_err(model_err)?,
        ));
    }
    let outputs = session.run(feed).map_err(model_err)?;
    let (shape, data) = outputs[0].try_extract_tensor::<f32>().map_err(model_err)?;
    let dims: Vec<usize> = shape.iter().map(|&d| d as usize).collect();
    let (stride, width) = match dims.as_slice() {
        // (batch, seq, dim): the CLS token's row.
        [_, seq, dim] => (seq * dim, *dim),
        [_, dim] => (*dim, *dim),
        other => return Err(model_err(format!("unsupported output shape {other:?}"))),
    };
    Ok((0..rows)
        .map(|r| {
            let v = &data[r * stride..r * stride + width];
            let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            let norm = if norm == 0.0 { 1e-12 } else { norm };
            v.iter().map(|x| x / norm).collect()
        })
        .collect())
}

impl Embedder for BgeEmbedder {
    fn model_name(&self) -> &str {
        BGE_MODEL
    }
    fn dim(&self) -> usize {
        BGE_DIM
    }
    fn context_tokens(&self) -> Option<usize> {
        Some(CONTEXT_TOKENS)
    }
    /// Two full batches of short texts per session.
    fn preferred_batch(&self) -> usize {
        2 * BATCH_ROWS * self.workers
    }
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        self.slot.with(
            || load(&self.models, self.source.as_deref(), self.workers),
            |m| {
                let encodings = m
                    .tokenizer
                    .encode_batch(texts.to_vec(), true)
                    .map_err(model_err)?;
                let lens: Vec<usize> = encodings.iter().map(|e| e.get_ids().len()).collect();
                let batches = plan(&lens, BATCH_TOKENS, BATCH_ROWS);
                let inputs = &m.inputs;
                fan_out(&mut m.sessions, &batches, texts.len(), |session, batch| {
                    let rows: Vec<&Encoding> = batch.iter().map(|&i| &encodings[i]).collect();
                    run_batch(session, inputs, &rows)
                })
            },
        )?
    }
}
