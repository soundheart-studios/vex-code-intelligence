//! Chunk ids, the content cap and qualified names, against values produced
//! by Cortex's own Python (tests/fixtures/oracle/ids.json).

use serde_json::{Value, json};
use std::collections::BTreeMap;
use vex_core::ids::{anonymous, chunk_id, qual_prefix, qualify, residual_chunk_id};
use vex_core::{CAP_CHUNK_CHARS, Chunk};

fn oracle() -> Value {
    serde_json::from_str(include_str!("fixtures/oracle/ids.json")).unwrap()
}

#[test]
fn chunk_ids_match_cortex() {
    let o = oracle();
    assert_eq!(chunk_id("src/a.py", 0, "def f():\n    pass"), o["ascii"]);
    assert_eq!(
        chunk_id("src/ü.py", 3, "s = 'naïve — ✓'\nprint(s)"),
        o["nonascii"]
    );
    assert_eq!(chunk_id("a.go", 10, "\n\nfunc f() {}\n"), o["empty_lines"]);
}

#[test]
fn residual_ids_use_their_own_formula() {
    let o = oracle();
    let content = "static const int t[] = {1, 2};";
    assert_eq!(residual_chunk_id("x/y.c", 81, content), o["residual"]);
    assert_ne!(
        residual_chunk_id("x/y.c", 81, content),
        chunk_id("x/y.c", 81, content)
    );
}

#[test]
fn the_id_hashes_the_uncapped_content() {
    let big = "x".repeat(6000);
    let id = chunk_id("big.py", 0, &big);
    assert_eq!(id, oracle()["uncapped_big"]);
    let c = Chunk::new(id.clone(), "big.py", 1, 1, big);
    assert_eq!(c.id, id, "capping must not touch the id");
    assert_eq!(c.content.chars().count(), CAP_CHUNK_CHARS);
}

#[test]
fn the_cap_counts_code_points_not_bytes() {
    let o = oracle();
    let c = Chunk::new("i", "f", 1, 1, "é".repeat(5001));
    assert_eq!(
        c.content.chars().count(),
        o["cap_uni"][0].as_u64().unwrap() as usize
    );
    assert_eq!(c.content.len(), o["cap_uni"][1].as_u64().unwrap() as usize);
    assert_eq!(json!(c.metadata), o["cap_uni"][2]);
    use sha2_check::sha256_hex;
    assert_eq!(sha256_hex(&c.content), o["cap_uni"][3]);
}

#[test]
fn over_the_cap_records_full_chars() {
    let c = Chunk::new("i", "f", 1, 1, "x".repeat(6000));
    assert_eq!(
        json!([c.content.chars().count(), c.metadata]),
        oracle()["cap_big"]
    );
}

#[test]
fn exactly_at_the_cap_records_nothing() {
    let c = Chunk::new("i", "f", 1, 1, "y".repeat(5000));
    assert_eq!(
        json!([c.content.chars().count(), c.metadata]),
        oracle()["cap_exact"]
    );
}

#[test]
fn an_existing_full_chars_is_kept_like_setdefault() {
    let mut meta = BTreeMap::new();
    meta.insert("full_chars".to_string(), json!(7));
    let c = Chunk::with_metadata("i", "f", 1, 1, "x".repeat(6000), "block", "", meta);
    assert_eq!(
        json!([c.content.chars().count(), c.metadata]),
        oracle()["cap_setdefault"]
    );
}

#[test]
fn chunk_defaults_match_cortex() {
    let c = Chunk::new("i", "f", 1, 2, "a");
    assert_eq!(
        (c.kind.as_str(), c.language.as_str(), c.metadata.len()),
        ("block", "", 0)
    );
}

#[test]
fn qualified_names_join_path_owners_and_name() {
    let prefix = qual_prefix("src/lib.rs");
    assert_eq!(prefix, "src.lib.rs");
    assert_eq!(
        qualify(&prefix, "Prerelease.is_empty"),
        "src.lib.rs.Prerelease.is_empty"
    );
    assert_eq!(
        qualify(&qual_prefix("db/db_impl.cc"), "leveldb.DBImpl.Run"),
        "db.db_impl.cc.leveldb.DBImpl.Run"
    );
}

#[test]
fn anonymous_names_are_one_based_row_and_byte_column() {
    assert_eq!(anonymous(0, 0), "(anonymous@1:1)");
    assert_eq!(anonymous(4, 10), "(anonymous@5:11)");
}

mod sha2_check {
    pub fn sha256_hex(s: &str) -> String {
        use std::fmt::Write;
        // Hash through the public canonical path rather than re-importing
        // sha2 here: content_sha256 of a one-chunk Parsed.
        let mut p = vex_core::Parsed::default();
        p.chunks.push(vex_core::Chunk::new("i", "f", 1, 1, s));
        let v = vex_core::canonical::to_canonical(Some(&p));
        let mut out = String::new();
        write!(
            out,
            "{}",
            v["chunks"][0]["content_sha256"].as_str().unwrap()
        )
        .unwrap();
        out
    }
}
