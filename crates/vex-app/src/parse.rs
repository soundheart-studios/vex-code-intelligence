//! `vex parse FILE`: the exact Parsed of one file, in the canonical golden
//! format (what `xtask parity` compares and Cortex's export writes).

use serde::Serialize;
use std::path::{Path, PathBuf};
use vex_core::Parsed;
use vex_extract::language::{HEAD_BYTES, language_of};

#[derive(Debug)]
pub enum ParseError {
    /// The file is not under the root, so it has no relative path.
    OutsideRoot {
        file: PathBuf,
        root: PathBuf,
    },
    Unreadable {
        file: PathBuf,
        error: String,
    },
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::OutsideRoot { file, root } => {
                write!(
                    f,
                    "{} is not under {}; pass --root",
                    file.display(),
                    root.display()
                )
            }
            ParseError::Unreadable { file, error } => {
                write!(f, "cannot read {}: {error}", file.display())
            }
        }
    }
}

/// One file's extraction with the facts a caller needs to present it.
pub struct ParseOutput {
    pub rel: String,
    pub language: &'static str,
    pub parsed: Parsed,
}

#[derive(Debug, Serialize)]
pub struct Summary {
    pub schema: u32,
    pub file: String,
    pub language: String,
    pub chunks: usize,
    pub symbols: usize,
    pub calls: usize,
    pub imports: usize,
}

impl ParseOutput {
    /// The canonical golden-format bytes (no `schema` field: byte-identity
    /// with Cortex's export is this command's contract).
    pub fn canonical_json(&self) -> Vec<u8> {
        vex_core::canonical::parsed_bytes(Some(&self.parsed))
    }

    pub fn summary(&self) -> Summary {
        Summary {
            schema: crate::SCHEMA,
            file: self.rel.clone(),
            language: self.language.to_string(),
            chunks: self.parsed.chunks.len(),
            symbols: self.parsed.symbols.len(),
            calls: self.parsed.calls.values().sum::<u64>() as usize,
            imports: self.parsed.imports.len(),
        }
    }
}

/// The filesystem answer to "where does `source <written>` point" for a
/// script in `dir` (relative to `root`): Cortex's `(path.parent /
/// raw).resolve().relative_to(root.resolve())`, kept only when it exists.
pub fn source_resolver(root: &Path) -> impl Fn(&str, &str) -> Option<String> + use<> {
    let canon_root = std::fs::canonicalize(root).ok();
    let given = root.to_path_buf();
    move |dir, written| {
        let root = canon_root.as_ref()?;
        // An absolute `written` replaces the base, as with Python's `/`.
        let resolved = std::fs::canonicalize(given.join(dir).join(written)).ok()?;
        let rel = resolved
            .strip_prefix(root)
            .ok()?
            .to_string_lossy()
            .into_owned();
        Some(if rel.is_empty() { ".".to_string() } else { rel })
    }
}

/// Extract `file`, naming it by its path relative to `root` (Cortex's
/// `chunk_file(root, path)`).
pub fn parse(root: &Path, file: &Path) -> Result<ParseOutput, ParseError> {
    let abs = |p: &Path| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    let (root_abs, file_abs) = (abs(root), abs(file));
    let rel = file_abs
        .strip_prefix(&root_abs)
        .map_err(|_| ParseError::OutsideRoot {
            file: file_abs.clone(),
            root: root_abs.clone(),
        })?
        .to_string_lossy()
        .into_owned();
    let bytes = std::fs::read(&file_abs).map_err(|e| ParseError::Unreadable {
        file: file_abs.clone(),
        error: e.to_string(),
    })?;
    let language = language_of(&rel, Some(&bytes[..bytes.len().min(HEAD_BYTES)]));
    let resolve = source_resolver(&root_abs);
    let parsed = vex_extract::engine::chunk_bytes_with(&rel, language, &bytes, &resolve);
    Ok(ParseOutput {
        rel,
        language,
        parsed,
    })
}
