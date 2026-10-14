//! The index pipeline: a port of Cortex's `indexer/pipeline.py` `run`
//! (embedding through `embed::Session`, eager unless deferred) and its store writes
//! (`ChunkStore.ingest`, `GraphStore.add_symbols`, `Database.delete_file`,
//! the merkle `Synchronizer`, the REFS rebuild and `rebuild_call_edges`).
//!
//! Row ORDER is reproduced, not just row content: the resolver reads rows in
//! table order, and query results later depend on rowids. Every write
//! happens in Cortex's sequence with Cortex's SQL.

use crate::embed::{EmbedReport, Session};
use crate::walk::walk;
use crate::windows::windows_for;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use vex_core::{Chunk, Parsed, Symbol};
use vex_extract::language::{HEAD_BYTES, language_of};
use vex_store::db::Index;
use vex_store::vec::{self, VecError};

/// Cortex's runtime-method names, excluded from REFS name matches unless
/// the referencing file imports them.
const RUNTIME_METHOD_NAMES: &[&str] = &[
    "map",
    "filter",
    "reduce",
    "find",
    "findIndex",
    "forEach",
    "some",
    "every",
    "flat",
    "flatMap",
    "slice",
    "splice",
    "concat",
    "join",
    "split",
    "trim",
    "replace",
    "replaceAll",
    "match",
    "matchAll",
    "startsWith",
    "endsWith",
    "includes",
    "indexOf",
    "lastIndexOf",
    "padStart",
    "padEnd",
    "repeat",
    "toLowerCase",
    "toUpperCase",
    "push",
    "pop",
    "shift",
    "unshift",
    "reverse",
    "sort",
    "keys",
    "values",
    "entries",
    "has",
    "add",
    "delete",
    "clear",
    "then",
    "catch",
    "finally",
    "resolve",
    "reject",
    "all",
    "race",
    "parse",
    "stringify",
    "toString",
    "valueOf",
    "bind",
    "call",
    "apply",
    "assign",
    "freeze",
    "hasOwnProperty",
    "next",
    "return",
    "throw",
    "describe",
    "it",
    "test",
    "expect",
    "beforeEach",
    "afterEach",
    "beforeAll",
    "afterAll",
    "vi",
    "jest",
    "mock",
    "spyOn",
    "get",
    "set",
    "list",
    "dict",
    "len",
    "open",
    "close",
    "read",
    "write",
    "append",
    "extend",
    "items",
    "update",
    "copy",
    "count",
    "format",
    "strip",
    "lower",
    "upper",
    "encode",
    "decode",
    "send",
    "run",
    "start",
    "stop",
    "clone",
    "document",
    "log",
    "error",
    "warn",
    "info",
    "debug",
    "print",
    "id",
    "type",
    "str",
    "int",
    "float",
];

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct IndexReport {
    pub files_scanned: usize,
    pub added: usize,
    pub modified: usize,
    pub removed: usize,
    pub chunks_ingested: usize,
    pub ref_edges: i64,
    pub call_edges: i64,
    pub index_version: String,
    /// What happened to vectors; absent when embedding is off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embed: Option<EmbedReport>,
}

/// An index run that could not complete.
#[derive(Debug)]
pub enum IndexError {
    Sql(rusqlite::Error),
    /// The index's vector width disagrees with the embedder's.
    Width(String),
}

impl std::fmt::Display for IndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IndexError::Sql(e) => write!(f, "{e}"),
            IndexError::Width(m) => write!(f, "{m}"),
        }
    }
}

impl From<rusqlite::Error> for IndexError {
    fn from(e: rusqlite::Error) -> Self {
        IndexError::Sql(e)
    }
}

impl From<VecError> for IndexError {
    fn from(e: VecError) -> Self {
        match e {
            VecError::Sql(e) => IndexError::Sql(e),
            other => IndexError::Width(other.to_string()),
        }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn read_head(path: &Path) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(HEAD_BYTES as u64)
        .read_to_end(&mut buf)
        .ok()?;
    Some(buf)
}

fn rel_of(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .into_owned()
}

/// Cortex `merkle.workspace_signature`: path, size and mtime_ns of every
/// discovered file, a cheap gate for editor hooks.
pub fn workspace_signature(root: &Path, paths: &[PathBuf]) -> String {
    let mut sorted: Vec<&PathBuf> = paths.iter().collect();
    sorted.sort_by_key(|p| p.to_string_lossy().into_owned());
    let mut h = Sha256::new();
    for p in sorted {
        let Ok(meta) = std::fs::metadata(p) else {
            continue;
        };
        let Ok(rel) = p.strip_prefix(root) else {
            continue;
        };
        #[cfg(unix)]
        let mtime_ns = {
            use std::os::unix::fs::MetadataExt;
            meta.mtime() as i128 * 1_000_000_000 + meta.mtime_nsec() as i128
        };
        #[cfg(not(unix))]
        let mtime_ns = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos() as i128);
        h.update(rel.to_string_lossy().as_bytes());
        h.update(b"\0");
        h.update(meta.len().to_string().as_bytes());
        h.update(b":");
        h.update(mtime_ns.to_string().as_bytes());
        h.update(b"\0");
    }
    format!("{:x}", h.finalize())
}

/// Cortex `Database.delete_file`: the single cascade path.
pub fn delete_file(idx: &Index, path: &str) -> rusqlite::Result<()> {
    let has_vec = idx
        .conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE name = 'chunks_vec'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if has_vec {
        idx.conn.execute(
            "DELETE FROM chunks_vec WHERE rowid IN (SELECT rowid FROM chunks WHERE file_path = ?1)",
            [path],
        )?;
    }
    for sql in [
        "DELETE FROM chunks_fts WHERE file_path = ?1",
        "DELETE FROM chunks WHERE file_path = ?1",
        "DELETE FROM edges WHERE src_file = ?1 OR dst_file = ?1",
        "DELETE FROM symbols WHERE file_path = ?1",
        "DELETE FROM symbol_sites WHERE file_path = ?1",
        "DELETE FROM refs WHERE file_path = ?1",
        "DELETE FROM import_bindings WHERE file_path = ?1",
        "DELETE FROM calls WHERE file_path = ?1",
        "DELETE FROM member_types WHERE file_path = ?1",
        "DELETE FROM files WHERE path = ?1",
        "DELETE FROM vex_file_stats WHERE path = ?1",
    ] {
        idx.conn.execute(sql, [path])?;
    }
    Ok(())
}

/// Cortex `ChunkStore.ingest`: upsert by id, the chunk's vector (if it
/// has one) keyed to its rowid, then the FTS row re-keyed to the rowid.
fn ingest_chunks(
    idx: &Index,
    chunks: &[Chunk],
    vectors: &[Option<Vec<f32>>],
) -> Result<(), IndexError> {
    if let Some(first) = vectors.iter().flatten().next() {
        vec::ensure_table(idx, first.len(), false)?;
    }
    let mut upsert = idx.conn.prepare_cached(
        "INSERT INTO chunks (id, file_path, start_line, end_line, kind, language, content, token_est, symbols, full_chars, parent_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
         ON CONFLICT(id) DO UPDATE SET content=excluded.content, \
         start_line=excluded.start_line, end_line=excluded.end_line, \
         symbols=excluded.symbols, full_chars=excluded.full_chars, \
         parent_id=excluded.parent_id \
         RETURNING rowid",
    )?;
    for (i, c) in chunks.iter().enumerate() {
        let meta_str = |k: &str| {
            c.metadata
                .get(k)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        let full_chars = c
            .metadata
            .get("full_chars")
            .and_then(|v| v.as_i64())
            .unwrap_or(c.content.chars().count() as i64);
        let token_est = crate::windows::tokens(&c.content) as i64;
        let symbols = meta_str("symbols");
        let rowid: i64 = upsert.query_row(
            params![
                c.id,
                c.file_path,
                c.start_line as i64,
                c.end_line as i64,
                c.kind,
                c.language,
                c.content,
                token_est,
                symbols,
                full_chars,
                meta_str("parent_id")
            ],
            |r| r.get(0),
        )?;
        if let Some(Some(v)) = vectors.get(i) {
            vec::put(&idx.conn, rowid, v)?;
        }
        idx.conn
            .execute("DELETE FROM chunks_fts WHERE rowid = ?1", [rowid])?;
        idx.conn.execute(
            "INSERT INTO chunks_fts (rowid, id, content, file_path, symbols) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![rowid, c.id, c.content, c.file_path, symbols],
        )?;
    }
    Ok(())
}

/// Cortex `GraphStore.add_symbols`: every site, and one preferred row per
/// name (the first definition in source order, else the first declaration).
fn add_symbols(idx: &Index, symbols: &[Symbol]) -> rusqlite::Result<()> {
    let mut sorted: Vec<&Symbol> = symbols.iter().collect();
    sorted.sort_by_key(|s| (s.declaration, s.start_line)); // stable, like Python's sort
    let mut seen = HashSet::new();
    let preferred: Vec<&Symbol> = sorted
        .into_iter()
        .filter(|s| seen.insert(s.qualified_name.clone()))
        .collect();
    for s in symbols {
        idx.conn.execute(
            "INSERT OR REPLACE INTO symbol_sites VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                s.qualified_name,
                s.name,
                s.kind,
                s.file_path,
                s.start_line as i64,
                s.end_line as i64,
                s.declaration as i64
            ],
        )?;
    }
    for s in preferred {
        idx.conn.execute(
            "INSERT OR REPLACE INTO symbols VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                s.qualified_name,
                s.name,
                s.kind,
                s.file_path,
                s.start_line as i64,
                s.end_line as i64
            ],
        )?;
    }
    Ok(())
}

fn store_file(
    idx: &Index,
    rel: &str,
    parsed: &Parsed,
    vectors: &[Option<Vec<f32>>],
) -> Result<(), IndexError> {
    if !parsed.chunks.is_empty() {
        ingest_chunks(idx, &parsed.chunks, vectors)?;
    }
    if !parsed.symbols.is_empty() {
        add_symbols(idx, &parsed.symbols)?;
    }
    if !parsed.imports.is_empty() {
        // Cortex iterates a set here; the rows are distinct either way.
        let modules: BTreeSet<&String> = parsed.imports.iter().collect();
        for m in modules {
            idx.conn.execute(
                "INSERT OR REPLACE INTO edges VALUES (?1, ?2, 'IMPORTS', 0.9, ?3, '')",
                params![format!("file:{rel}"), format!("module:{m}"), rel],
            )?;
        }
    }
    for (name, n) in &parsed.refs {
        idx.conn.execute(
            "INSERT OR REPLACE INTO refs (file_path, name, count) VALUES (?1, ?2, ?3)",
            params![rel, name, *n as i64],
        )?;
    }
    for (name, (module, orig)) in &parsed.bindings {
        idx.conn.execute(
            "INSERT OR REPLACE INTO import_bindings (file_path, name, module, orig) VALUES (?1, ?2, ?3, ?4)",
            params![rel, name, module, orig],
        )?;
    }
    for ((caller, name), n) in &parsed.calls {
        idx.conn.execute(
            "INSERT OR REPLACE INTO calls (file_path, caller, name, count) VALUES (?1, ?2, ?3, ?4)",
            params![rel, caller, name, *n as i64],
        )?;
    }
    for (owner, member, kind) in &parsed.members {
        idx.conn.execute(
            "INSERT OR REPLACE INTO member_types (file_path, owner, member, type) VALUES (?1, ?2, ?3, ?4)",
            params![rel, owner, member, kind],
        )?;
    }
    Ok(())
}

/// Cortex `_rebuild_ref_edges`, the same SQL.
fn rebuild_ref_edges(idx: &Index) -> rusqlite::Result<()> {
    let mut names: Vec<&str> = RUNTIME_METHOD_NAMES.to_vec();
    names.sort();
    let placeholders = vec!["?"; names.len()].join(",");
    idx.conn
        .execute("DELETE FROM edges WHERE type = 'REFS'", [])?;
    let sql = format!(
        "INSERT OR REPLACE INTO edges (src, dst, type, confidence, src_file, dst_file) \
         SELECT 'file:' || r.file_path, s.qualified_name, 'REFS', \
                MIN(1.0, r.count / 4.0) / (SELECT COUNT(DISTINCT file_path) FROM symbols WHERE name = r.name), \
                r.file_path, s.file_path \
         FROM refs r JOIN symbols s ON s.name = r.name \
         WHERE s.file_path != r.file_path \
           AND r.name IN (SELECT name FROM symbols GROUP BY name HAVING COUNT(DISTINCT file_path) <= 3) \
           AND NOT (r.name IN ({placeholders}) \
                    AND NOT EXISTS (SELECT 1 FROM import_bindings b WHERE b.file_path = r.file_path AND b.name = r.name))"
    );
    idx.conn.execute(&sql, rusqlite::params_from_iter(names))?;
    Ok(())
}

/// Cortex `workspace_packages`: a monorepo's own package names -> dirs.
fn workspace_packages(root: &Path, files: &BTreeSet<String>) -> BTreeMap<String, String> {
    let mut packages = BTreeMap::new();
    for rel in files {
        if rel.rsplit('/').next() != Some("package.json") {
            continue;
        }
        let dir = rel.rsplit_once('/').map_or("", |(d, _)| d);
        if dir.is_empty() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(root.join(rel)) else {
            continue;
        };
        let Ok(serde_json::Value::Object(data)) = serde_json::from_str(&text) else {
            continue;
        };
        if let Some(name) = data
            .get("name")
            .and_then(|n| n.as_str())
            .filter(|n| !n.is_empty())
        {
            packages.insert(name.to_string(), dir.to_string());
        }
    }
    packages
}

/// Cortex `rebuild_call_edges`: read the rows in table order, resolve, and
/// replay every edge through INSERT OR REPLACE.
/// The rows `rebuild_call_edges` reads, in the order Cortex's SELECTs
/// return them (also what the benchmark scores).
pub fn read_rows(idx: &Index, root: &Path) -> rusqlite::Result<vex_resolve::Rows> {
    let c = &idx.conn;
    let strings = |sql: &str| -> rusqlite::Result<Vec<String>> {
        c.prepare(sql)?.query_map([], |r| r.get(0))?.collect()
    };
    let quads = |sql: &str| -> rusqlite::Result<Vec<(String, String, String, String)>> {
        c.prepare(sql)?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect()
    };
    let files: BTreeSet<String> = strings("SELECT path FROM files")?.into_iter().collect();
    let rows = vex_resolve::Rows {
        packages: workspace_packages(root, &files),
        files,
        symbols: quads("SELECT qualified_name, name, kind, file_path FROM symbols")?,
        bindings: quads("SELECT file_path, name, module, orig FROM import_bindings")?,
        calls: c
            .prepare("SELECT file_path, caller, name, count FROM calls")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<_>>()?,
        abstract_qns: strings("SELECT qualified_name FROM symbol_sites GROUP BY qualified_name HAVING MIN(declaration) = 1")?
            .into_iter()
            .collect(),
        members: quads("SELECT file_path, owner, member, type FROM member_types")?,
    };
    Ok(rows)
}

fn rebuild_call_edges(idx: &Index, root: &Path) -> rusqlite::Result<()> {
    let c = &idx.conn;
    let rows = read_rows(idx, root)?;
    let edges = vex_resolve::resolve_all(&rows);
    c.execute("DELETE FROM edges WHERE type = 'CALLS'", [])?;
    let mut insert = c.prepare("INSERT OR REPLACE INTO edges (src, dst, type, confidence, src_file, dst_file) VALUES (?1, ?2, 'CALLS', ?3, ?4, ?5)")?;
    for e in edges {
        insert.execute(params![e.src, e.dst, e.confidence, e.src_file, e.dst_file])?;
    }
    Ok(())
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

fn new_version() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    sha256_hex(format!("{nanos}:{}", std::process::id()).as_bytes())[..12].to_string()
}

/// How a run finds what changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scan {
    /// Hash every file (Cortex's merkle diff).
    Full,
    /// Hash only files whose size or mtime moved since they were last
    /// hashed, plus new ones; the rest keep their recorded hash. Finds the
    /// same changes as a full scan unless a file's content changed with
    /// its size and mtime unchanged.
    Incremental,
}

/// A file's (size, mtime in ns) for the stats record.
fn stat_of(path: &Path) -> Option<(i64, i64)> {
    let meta = std::fs::metadata(path).ok()?;
    #[cfg(unix)]
    let mtime = {
        use std::os::unix::fs::MetadataExt;
        meta.mtime() * 1_000_000_000 + meta.mtime_nsec()
    };
    #[cfg(not(unix))]
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as i64);
    Some((meta.len() as i64, mtime))
}

/// Cortex `Pipeline.run(embed=False)` with embedding off.
pub fn run(idx: &Index, root: &Path, force: bool) -> rusqlite::Result<IndexReport> {
    run_with(idx, root, force, None, false).map_err(|e| match e {
        IndexError::Sql(e) => e,
        IndexError::Width(m) => unreachable!("no embedder, no width to disagree with: {m}"),
    })
}

/// Cortex `Pipeline.run`: with a session, vectors are computed for every
/// new chunk (its full content, longest first in the embedder's groups,
/// through the cache) before
/// the store phase, unless `defer` leaves them for `vex embed`. A model
/// failure leaves the rest pending; a width mismatch refuses the run before
/// anything is written, unless `force` is the rebuild that fixes it.
pub fn run_with(
    idx: &Index,
    root: &Path,
    force: bool,
    session: Option<&mut Session>,
    defer: bool,
) -> Result<IndexReport, IndexError> {
    run_scan(idx, root, force, session, defer, Scan::Full)
}

/// `run_with`, finding changes by `scan`.
pub fn run_scan(
    idx: &Index,
    root: &Path,
    force: bool,
    mut session: Option<&mut Session>,
    defer: bool,
    scan: Scan,
) -> Result<IndexReport, IndexError> {
    if let Some(s) = session.as_deref()
        && !force
    {
        vec::assert_compatible(idx, s.dim())?;
    }
    let paths = walk(root).files;
    let signature = workspace_signature(root, &paths);
    let snapshot: Vec<(String, String)> = idx
        .conn
        .prepare("SELECT path, hash FROM files")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    if force {
        // The one place a width change is legitimate: an asked-for rebuild.
        if let Some(s) = session.as_deref() {
            vec::ensure_table(idx, s.dim(), true)?;
        }
        for (rel, _) in &snapshot {
            delete_file(idx, rel)?;
        }
    }
    // Synchronizer.diff (after a forced clear the snapshot is empty).
    let old: BTreeMap<String, String> = if force {
        BTreeMap::new()
    } else {
        snapshot.iter().cloned().collect()
    };
    let old_order: Vec<String> = if force {
        Vec::new()
    } else {
        snapshot.iter().map(|(p, _)| p.clone()).collect()
    };
    let recorded: BTreeMap<String, (i64, i64)> = if scan == Scan::Incremental && !force {
        idx.conn
            .prepare("SELECT path, size, mtime_ns FROM vex_file_stats")?
            .query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?
            .collect::<rusqlite::Result<_>>()?
    } else {
        BTreeMap::new()
    };
    let mut new: Vec<(String, String)> = Vec::new();
    let mut stats: Vec<(String, (i64, i64))> = Vec::new();
    let scanning = crate::progress::stage(crate::progress::Stage::Scan, paths.len() as u64);
    for p in &paths {
        scanning.advance(1);
        let rel = rel_of(root, p);
        // Taken before reading: a write after it moves the stats again.
        let stat = stat_of(p);
        if let (Some(st), Some(prev), Some(hash)) = (stat, recorded.get(&rel), old.get(&rel))
            && st == *prev
        {
            new.push((rel.clone(), hash.clone()));
            stats.push((rel, st));
            continue;
        }
        if let Ok(bytes) = std::fs::read(p) {
            new.push((rel.clone(), sha256_hex(&bytes)));
            if let Some(st) = stat {
                stats.push((rel, st));
            }
        }
    }
    drop(scanning);
    let new_map: BTreeMap<&str, &str> = new.iter().map(|(p, h)| (p.as_str(), h.as_str())).collect();
    let added: Vec<String> = new
        .iter()
        .filter(|(p, _)| !old.contains_key(p))
        .map(|(p, _)| p.clone())
        .collect();
    let removed: Vec<String> = old_order
        .iter()
        .filter(|p| !new_map.contains_key(p.as_str()))
        .cloned()
        .collect();
    let modified: Vec<String> = new
        .iter()
        .filter(|(p, h)| old.get(p).is_some_and(|o| o != h))
        .map(|(p, _)| p.clone())
        .collect();

    let tx = idx.conn.unchecked_transaction()?;
    for rel in &removed {
        delete_file(idx, rel)?;
    }
    let to_index: Vec<String> = added.iter().chain(modified.iter()).cloned().collect();
    let mut parsed_files: Vec<(String, Parsed)> = Vec::new();
    let parsing = crate::progress::stage(crate::progress::Stage::Parse, to_index.len() as u64);
    let resolve = crate::parse::source_resolver(root);
    for rel in &to_index {
        parsing.advance(1);
        delete_file(idx, rel)?;
        let path = root.join(rel);
        let bytes = std::fs::read(&path).unwrap_or_default();
        let lang = language_of(rel, Some(&bytes[..bytes.len().min(HEAD_BYTES)]));
        let mut parsed = vex_extract::engine::chunk_bytes_with(rel, lang, &bytes, &resolve);
        let extra: Vec<Chunk> = parsed.chunks.iter().flat_map(windows_for).collect();
        parsed.chunks.extend(extra);
        parsed_files.push((rel.clone(), parsed));
    }
    drop(parsing);
    // Phase 2: embed across files in global batches.
    let mut vectors: Vec<Vec<Option<Vec<f32>>>> = parsed_files
        .iter()
        .map(|(_, p)| vec![None; p.chunks.len()])
        .collect();
    let mut embedded = 0;
    if let Some(s) = session.as_deref_mut()
        && !defer
    {
        let mut slots: Vec<(usize, usize)> = parsed_files
            .iter()
            .enumerate()
            .flat_map(|(f, (_, p))| (0..p.chunks.len()).map(move |c| (f, c)))
            .collect();
        // Longest first: each model batch then holds texts of like length.
        slots.sort_by_key(|&(f, c)| std::cmp::Reverse(parsed_files[f].1.chunks[c].content.len()));
        let embedding = crate::progress::stage(crate::progress::Stage::Embed, slots.len() as u64);
        for batch in slots.chunks(s.group()) {
            let texts: Vec<&str> = batch
                .iter()
                .map(|&(f, c)| parsed_files[f].1.chunks[c].content.as_str())
                .collect();
            let Some(vecs) = s.embed(&texts) else {
                break;
            };
            for (&(f, c), v) in batch.iter().zip(vecs) {
                vectors[f][c] = Some(v);
                embedded += 1;
            }
            embedding.advance(batch.len() as u64);
        }
    }
    let mut chunks_ingested = 0;
    for (i, (rel, parsed)) in parsed_files.iter().enumerate() {
        store_file(idx, rel, parsed, &vectors[i])?;
        chunks_ingested += parsed.chunks.len();
        let hash = new_map
            .get(rel.as_str())
            .map(|h| h.to_string())
            .unwrap_or_default();
        let lang = language_of(rel, read_head(&root.join(rel)).as_deref());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        idx.conn.execute(
            "INSERT OR REPLACE INTO files (path, hash, language, indexed_at) VALUES (?1, ?2, ?3, ?4)",
            params![rel, hash, lang, now],
        )?;
    }
    if !to_index.is_empty() || !removed.is_empty() {
        rebuild_ref_edges(idx)?;
        rebuild_call_edges(idx, root)?;
    }
    idx.conn.execute("DELETE FROM vex_file_stats", [])?;
    {
        let mut put = idx.conn.prepare_cached(
            "INSERT INTO vex_file_stats (path, size, mtime_ns) VALUES (?1, ?2, ?3)",
        )?;
        for (rel, (size, mtime)) in &stats {
            put.execute(params![rel, size, mtime])?;
        }
    }
    idx.conn.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('workspace_signature', ?1)",
        [&signature],
    )?;
    if let Some(head) = git_head(root) {
        idx.conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('git_head', ?1)",
            [&head],
        )?;
    }
    let changed = !to_index.is_empty() || !removed.is_empty() || force;
    let current: Option<String> = idx
        .conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'index_version'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let index_version = match current {
        Some(v) if !changed && !v.is_empty() => v,
        _ => {
            let v = new_version();
            idx.conn.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES ('index_version', ?1)",
                [&v],
            )?;
            v
        }
    };
    let count = |t: &str| -> rusqlite::Result<i64> {
        idx.conn.query_row(
            &format!("SELECT COUNT(*) FROM edges WHERE type = '{t}'"),
            [],
            |r| r.get(0),
        )
    };
    let report = IndexReport {
        files_scanned: paths.len(),
        added: added.len(),
        modified: modified.len(),
        removed: removed.len(),
        chunks_ingested,
        ref_edges: count("REFS")?,
        call_edges: count("CALLS")?,
        index_version,
        embed: None,
    };
    tx.commit()?;
    let Some(s) = session else {
        return Ok(report);
    };
    // Stamp what produced these vectors (Cortex records it every run).
    idx.record_embedding_provenance(s.backend().name(), s.model_name(), s.dim() as i64)?;
    let pending = vec::count_pending(&idx.conn)? as usize;
    Ok(IndexReport {
        embed: Some(EmbedReport {
            backend: s.backend().name().to_string(),
            model: s.model_name().to_string(),
            embedded,
            pending,
            cache_hits: s.hits,
            cache_misses: s.misses,
            notice: s.index_notice(pending),
        }),
        ..report
    })
}
