-- vex index schema: Cortex's src/cortex/store/schema.sql at the golden
-- revision, copied VERBATIM up to (not including) its session-memory section.
-- Dropped by ADR 1 (code intelligence only): memory, memory_fts,
-- memory_symbols, ledger, embedding_cache (the cache is global in vex).
-- Existing tables must not change while parity matters; vex-only tables are
-- prefixed vex_.

-- Cortex single-file schema. Everything lives in one cortex.db:
-- one transaction boundary, no cross-file consistency drift.
-- chunks_vec (vec0 virtual table) is created lazily once embedding dim is known.

CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- File registry: source of truth for the Merkle sync + cascade deletes.
CREATE TABLE IF NOT EXISTS files (
    path       TEXT PRIMARY KEY,
    hash       TEXT NOT NULL,
    language   TEXT NOT NULL DEFAULT '',
    indexed_at REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS chunks (
    id         TEXT PRIMARY KEY,
    file_path  TEXT NOT NULL,
    start_line INTEGER NOT NULL,
    end_line   INTEGER NOT NULL,
    kind       TEXT NOT NULL DEFAULT 'block',   -- function | class | module | block
    language   TEXT NOT NULL DEFAULT '',
    content    TEXT NOT NULL,
    token_est  INTEGER NOT NULL DEFAULT 0,
    -- Space-joined symbol names defined in this chunk. Computed at index
    -- time (indexer/astchunk.py, indexer/chunker.py) and previously written
    -- ONLY into chunks_fts, so retrieval results carried no symbol identity
    -- and could not be capped per symbol. Persisted here so every consumer
    -- gets it without a per-hit lookup. Migration for existing databases
    -- lives in store/db.py::_migrate.
    symbols    TEXT NOT NULL DEFAULT '',
    -- The symbol's real length in characters, before content was capped at
    -- store/chunks.py::_MAX_CONTENT_CHARS. Without it a capped chunk is
    -- indistinguishable at query time from a symbol that is genuinely that
    -- long, so search returned a body cut mid-statement and said nothing.
    -- 0 means "not recorded" (rows written before this column existed).
    full_chars INTEGER NOT NULL DEFAULT 0,
    -- Set when this row is a WINDOW: a narrower view of the chunk named
    -- here, added because one vector cannot represent a symbol larger than
    -- the embedding window (indexer/windows.py). '' means an ordinary chunk.
    -- Explicit rather than inferred from overlapping spans: no two chunks
    -- overlap today, but nested symbols legitimately would, and a dedup rule
    -- built on span containment would start discarding real results the day
    -- that changed.
    parent_id  TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_chunks_file ON chunks(file_path);

CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
    id UNINDEXED, content, file_path, symbols
);

-- Symbol-level graph (codebase-memory taxonomy, reimplemented).
CREATE TABLE IF NOT EXISTS symbols (
    qualified_name TEXT PRIMARY KEY,             -- <project>.<path_parts>.<name>
    name           TEXT NOT NULL,
    kind           TEXT NOT NULL,                -- function | method | class | interface | module
    file_path      TEXT NOT NULL,
    start_line     INTEGER NOT NULL,
    end_line       INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name);
CREATE INDEX IF NOT EXISTS idx_symbols_file ON symbols(file_path);

-- Every definition site of a qualified name (ADR 19). `symbols` keeps one
-- preferred row per name -- the first definition, else the first declaration
-- -- so edges, call resolution and memory links that key on the name are
-- unchanged, while overloads, multi-clause functions and declaration /
-- definition pairs each keep their own span here. Qualified names embed the
-- file path, so every site of one name lives in one file.
CREATE TABLE IF NOT EXISTS symbol_sites (
    qualified_name TEXT NOT NULL,
    name           TEXT NOT NULL,
    kind           TEXT NOT NULL,
    file_path      TEXT NOT NULL,
    start_line     INTEGER NOT NULL,
    end_line       INTEGER NOT NULL,
    declaration    INTEGER NOT NULL DEFAULT 0,  -- 1: no body (e.g. @interface method)
    PRIMARY KEY (qualified_name, file_path, start_line)
);
CREATE INDEX IF NOT EXISTS idx_symbol_sites_file ON symbol_sites(file_path);

-- Call-site references per file (aider's "refs" side of the tags model).
-- REFS edges are rebuilt from this table after every index run, so
-- incremental indexing stays consistent in both directions.
CREATE TABLE IF NOT EXISTS refs (
    file_path TEXT NOT NULL,
    name      TEXT NOT NULL,
    count     INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (file_path, name)
);
CREATE INDEX IF NOT EXISTS idx_refs_name ON refs(name);

-- Named import bindings per file (M3): `from src.auth import hash_password
-- as hp` -> (file, 'hp', 'src.auth', 'hash_password'). Resolution to files
-- happens at edge-rebuild time so it sees the whole current index.
CREATE TABLE IF NOT EXISTS import_bindings (
    file_path TEXT NOT NULL,
    name      TEXT NOT NULL,      -- local binding name (alias if aliased)
    module    TEXT NOT NULL,      -- module string as written
    orig      TEXT NOT NULL,      -- original symbol name in the module
    PRIMARY KEY (file_path, name)
);

-- Call sites attributed to their enclosing symbol (M3).
CREATE TABLE IF NOT EXISTS calls (
    file_path TEXT NOT NULL,
    caller    TEXT NOT NULL,      -- qualified_name of enclosing def, or file:<path>
    name      TEXT NOT NULL,      -- callee name as written
    count     INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (file_path, caller, name)
);
CREATE INDEX IF NOT EXISTS idx_calls_name ON calls(name);

-- Declared field types per type (ADR 20): a call through a field declared in
-- another file (a C++ header for its .cc) resolves through the field's type.
-- owner is the type's qualified name without the file prefix; type is one
-- plain type name, `std` for any standard-library type.
CREATE TABLE IF NOT EXISTS member_types (
    file_path TEXT NOT NULL,
    owner     TEXT NOT NULL,
    member    TEXT NOT NULL,
    type      TEXT NOT NULL,
    PRIMARY KEY (file_path, owner, member)
);
CREATE INDEX IF NOT EXISTS idx_member_types_owner ON member_types(owner, member);

CREATE TABLE IF NOT EXISTS edges (
    src        TEXT NOT NULL,                    -- qualified_name (or file: path)
    dst        TEXT NOT NULL,
    type       TEXT NOT NULL,                    -- CALLS | IMPORTS | DEFINES | EXTENDS | IMPLEMENTS | USES_TYPE
    confidence REAL NOT NULL DEFAULT 1.0,
    src_file   TEXT NOT NULL DEFAULT '',
    dst_file   TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (src, dst, type)
);
CREATE INDEX IF NOT EXISTS idx_edges_dst ON edges(dst);

-- vex: the schema version this database was created or migrated to.
CREATE TABLE IF NOT EXISTS vex_schema (
    version INTEGER NOT NULL
);

-- vex: each indexed file's size and mtime when it was last hashed, so an
-- incremental scan hashes only files whose stats moved (git's index idea).
-- A full scan ignores it and rewrites it.
CREATE TABLE IF NOT EXISTS vex_file_stats (
    path     TEXT PRIMARY KEY,
    size     INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL
);
