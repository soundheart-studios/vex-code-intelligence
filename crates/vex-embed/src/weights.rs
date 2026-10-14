//! The pinned bge-small weights: fetched on first need, sha256-verified,
//! and stored under VEX_HOME/models. Bytes reach their final name only
//! after their hash matches, so a partial or corrupted download is never
//! loaded.

use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

/// fastembed 0.8.0's source for `BAAI/bge-small-en-v1.5`, pinned to the
/// revision Cortex's recorded vectors came from.
pub const BGE_REPO: &str = "Qdrant/bge-small-en-v1.5-onnx-Q";
pub const BGE_REVISION: &str = "aa8f8b060edb00e03bfdd08813a2949946c8ba55";

#[derive(Debug, Clone, Copy)]
pub struct ModelFile {
    pub name: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

pub const BGE_FILES: &[ModelFile] = &[
    ModelFile {
        name: "model_optimized.onnx",
        sha256: "51f1bd0addd6e859e42c2c8021a5e5461385bb676a649f4b269aa445449f2431",
        size: 66_465_124,
    },
    ModelFile {
        name: "tokenizer.json",
        sha256: "d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66",
        size: 711_396,
    },
    ModelFile {
        name: "config.json",
        sha256: "13582bcf2effc85b7bf3d3f5532e686bc1c9ce86bb009d10f0ec33cbe92299dd",
        size: 706,
    },
    ModelFile {
        name: "tokenizer_config.json",
        sha256: "0b29c7bfc889e53b36d9dd3e686dd4300f6525110eaa98c76a5dafceb2029f53",
        size: 1_242,
    },
    ModelFile {
        name: "special_tokens_map.json",
        sha256: "5d5b662e421ea9fac075174bb0688ee0d9431699900b90662acd44b2a350503a",
        size: 695,
    },
];

#[derive(Debug)]
pub enum WeightsError {
    Missing {
        file: String,
    },
    Checksum {
        file: String,
        expected: String,
        actual: String,
    },
    Fetch {
        file: String,
        reason: String,
    },
    Io {
        file: String,
        reason: String,
    },
}

impl std::fmt::Display for WeightsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WeightsError::Missing { file } => write!(f, "model file {file} is not downloaded"),
            WeightsError::Checksum {
                file,
                expected,
                actual,
            } => write!(
                f,
                "model file {file} failed verification: sha256 {actual}, expected {expected}"
            ),
            WeightsError::Fetch { file, reason } => {
                write!(f, "could not download model file {file}: {reason}")
            }
            WeightsError::Io { file, reason } => write!(f, "model file {file}: {reason}"),
        }
    }
}

impl std::error::Error for WeightsError {}

/// Where the bge weights live under a models directory.
pub fn bge_dir(models: &Path) -> PathBuf {
    models.join(crate::BGE_MODEL)
}

/// The download URL of a pinned file; `base` replaces the hub (a mirror,
/// or a local server in tests).
pub fn url(base: Option<&str>, name: &str) -> String {
    let base = base.unwrap_or("https://huggingface.co");
    format!(
        "{}/{BGE_REPO}/resolve/{BGE_REVISION}/{name}",
        base.trim_end_matches('/')
    )
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn check(dir: &Path, f: &ModelFile) -> Result<(), WeightsError> {
    let path = dir.join(f.name);
    if !path.is_file() {
        return Err(WeightsError::Missing {
            file: f.name.into(),
        });
    }
    let actual = sha256_file(&path).map_err(|e| WeightsError::Io {
        file: f.name.into(),
        reason: e.to_string(),
    })?;
    if actual != f.sha256 {
        return Err(WeightsError::Checksum {
            file: f.name.into(),
            expected: f.sha256.into(),
            actual,
        });
    }
    Ok(())
}

/// Every file present with its pinned hash; the first problem otherwise.
pub fn verify_files(dir: &Path, files: &[ModelFile]) -> Result<(), WeightsError> {
    files.iter().try_for_each(|f| check(dir, f))
}

/// Make every file present and verified, fetching (by file name) the ones
/// missing or failing their hash. A fetched file that does not hash to its
/// pin is refused and never written under its name.
pub fn ensure_files(
    dir: &Path,
    files: &[ModelFile],
    fetch: &dyn Fn(&str) -> Result<Vec<u8>, String>,
) -> Result<(), WeightsError> {
    let io = |file: &str, e: std::io::Error| WeightsError::Io {
        file: file.into(),
        reason: e.to_string(),
    };
    std::fs::create_dir_all(dir).map_err(|e| io(files.first().map_or("", |f| f.name), e))?;
    for f in files {
        if check(dir, f).is_ok() {
            continue;
        }
        let bytes = fetch(f.name).map_err(|reason| WeightsError::Fetch {
            file: f.name.into(),
            reason,
        })?;
        let actual = hex(&Sha256::digest(&bytes));
        if actual != f.sha256 {
            return Err(WeightsError::Checksum {
                file: f.name.into(),
                expected: f.sha256.into(),
                actual,
            });
        }
        let tmp = dir.join(format!(".{}.{}.part", f.name, std::process::id()));
        std::fs::write(&tmp, &bytes).map_err(|e| io(f.name, e))?;
        if let Err(e) = std::fs::rename(&tmp, dir.join(f.name)) {
            let _ = std::fs::remove_file(&tmp);
            return Err(io(f.name, e));
        }
    }
    Ok(())
}

/// Download one pinned file over HTTPS (rustls), refusing a body larger
/// than the pin allows.
pub fn http_fetch(base: Option<&str>, name: &str) -> Result<Vec<u8>, String> {
    http_fetch_with(base, name, &|_| {})
}

/// `http_fetch`, calling `on_bytes` with each piece's length as it arrives.
pub fn http_fetch_with(
    base: Option<&str>,
    name: &str,
    on_bytes: &dyn Fn(u64),
) -> Result<Vec<u8>, String> {
    let limit = BGE_FILES
        .iter()
        .find(|f| f.name == name)
        .map_or(1 << 20, |f| f.size + 1);
    let mut response = ureq::get(&url(base, name))
        .call()
        .map_err(|e| e.to_string())?;
    let mut reader = response.body_mut().with_config().limit(limit).reader();
    let mut body = Vec::new();
    let mut buf = vec![0u8; 64 << 10];
    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(body);
        }
        body.extend_from_slice(&buf[..n]);
        on_bytes(n as u64);
    }
}

/// Bytes still to download before every bge file is in place: the pinned
/// size of each file missing or failing its hash.
pub fn bge_missing_bytes(models: &Path) -> u64 {
    let dir = bge_dir(models);
    BGE_FILES
        .iter()
        .filter(|f| check(&dir, f).is_err())
        .map(|f| f.size)
        .sum()
}

/// The bge weights present and verified under `models`, downloading them
/// from `base` (the hub when None) on first need.
pub fn ensure_bge(models: &Path, base: Option<&str>) -> Result<PathBuf, WeightsError> {
    let dir = bge_dir(models);
    ensure_files(&dir, BGE_FILES, &|name: &str| http_fetch(base, name))?;
    Ok(dir)
}

/// `ensure_bge`, calling `on_bytes` as download bytes arrive.
pub fn ensure_bge_with(
    models: &Path,
    base: Option<&str>,
    on_bytes: &dyn Fn(u64),
) -> Result<PathBuf, WeightsError> {
    let dir = bge_dir(models);
    ensure_files(&dir, BGE_FILES, &|name: &str| {
        http_fetch_with(base, name, on_bytes)
    })?;
    Ok(dir)
}
