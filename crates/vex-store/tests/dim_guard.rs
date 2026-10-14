//! The vector store's width guard (Cortex's fix): a query or writer whose
//! width differs from the index fails loudly, naming both widths and the
//! rebuild command, and only an explicit rebuild changes the width.

use vex_store::cache::{self, EmbeddingCache};
use vex_store::db::Index;
use vex_store::vec;
use vex_test_support::TestHome;

fn index_with_chunks(h: &TestHome) -> Index {
    let idx = Index::open(&h.vex_home().join("dim.db")).unwrap();
    for (i, text) in ["alpha beta", "gamma delta", "epsilon"].iter().enumerate() {
        idx.conn
            .execute(
                "INSERT INTO chunks (id, file_path, start_line, end_line, content) VALUES (?1, 'a.py', 1, 2, ?2)",
                rusqlite::params![format!("c{i}"), text],
            )
            .unwrap();
        idx.conn
            .execute(
                "INSERT INTO chunks_fts (rowid, id, content, file_path, symbols) VALUES (last_insert_rowid(), ?1, ?2, 'a.py', '')",
                rusqlite::params![format!("c{i}"), text],
            )
            .unwrap();
    }
    idx
}

fn unit(width: usize, hot: usize) -> Vec<f32> {
    let mut v = vec![0f32; width];
    v[hot] = 1.0;
    v
}

#[test]
fn sqlite_vec_is_linked_at_cortexs_version() {
    let h = TestHome::new();
    let idx = Index::open(&h.vex_home().join("v.db")).unwrap();
    assert_eq!(vec::version(&idx.conn).unwrap(), "v0.1.9");
    // Built as the official wheels are, so distances round as Cortex's do.
    let want = match std::env::consts::ARCH {
        "aarch64" => "neon",
        "x86_64" => "avx",
        _ => "",
    };
    assert_eq!(vec::build_flags(&idx.conn).unwrap(), want);
}

#[test]
fn chunks_vec_is_created_lazily_at_the_first_width() {
    let h = TestHome::new();
    let idx = index_with_chunks(&h);
    assert_eq!(vec::dim(&idx.conn).unwrap(), None);
    assert_eq!(vec::count_pending(&idx.conn).unwrap(), 3);
    assert!(vec::search(&idx, &unit(256, 0), 5).unwrap().is_empty());
    vec::set_embeddings(&idx, &[(1, unit(256, 1)), (2, unit(256, 2))]).unwrap();
    assert_eq!(vec::dim(&idx.conn).unwrap(), Some(256));
    assert_eq!(vec::count_pending(&idx.conn).unwrap(), 1);
    assert_eq!(
        vec::pending(&idx.conn, 10).unwrap(),
        vec![(3, "epsilon".to_string())]
    );
    let hits = vec::search(&idx, &unit(256, 2), 5).unwrap();
    assert_eq!(hits.iter().map(|h| h.0).collect::<Vec<_>>(), vec![2, 1]);
    assert_eq!(hits[0].1, 0.0);
    assert_eq!(vec::search(&idx, &unit(256, 2), 1).unwrap().len(), 1);
}

#[test]
fn a_query_of_another_width_fails_naming_both_dims() {
    let h = TestHome::new();
    let idx = index_with_chunks(&h);
    idx.record_embedding_provenance("hash", "hash-256", 256)
        .unwrap();
    vec::set_embeddings(&idx, &[(1, unit(256, 1))]).unwrap();
    let err = vec::search(&idx, &unit(384, 1), 5).unwrap_err();
    let msg = err.to_string();
    for want in ["256", "384", "hash-256", "vex index --force"] {
        assert!(msg.contains(want), "{want:?} missing from: {msg}");
    }
    assert!(matches!(
        err,
        vec::VecError::Width {
            stored: 256,
            produced: 384,
            rebuild_refused: false,
            ..
        }
    ));
    let err = vec::assert_compatible(&idx, 384).unwrap_err().to_string();
    assert!(err.contains("256") && err.contains("384"), "{err}");
    vec::assert_compatible(&idx, 256).unwrap();
}

#[test]
fn a_writer_of_another_width_is_refused_and_deletes_nothing() {
    let h = TestHome::new();
    let idx = index_with_chunks(&h);
    idx.record_embedding_provenance("hash", "hash-256", 256)
        .unwrap();
    vec::set_embeddings(&idx, &[(1, unit(256, 1))]).unwrap();
    let err = vec::set_embeddings(&idx, &[(2, unit(384, 1))]).unwrap_err();
    assert!(matches!(
        err,
        vec::VecError::Width {
            stored: 256,
            produced: 384,
            rebuild_refused: true,
            ..
        }
    ));
    let msg = err.to_string();
    for want in [
        "256",
        "384",
        "hash-256",
        "backend hash",
        "vex index --force",
    ] {
        assert!(msg.contains(want), "{want:?} missing from: {msg}");
    }
    let chunks: i64 = idx
        .conn
        .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(chunks, 3, "a refused writer deleted chunks");
    assert_eq!(vec::dim(&idx.conn).unwrap(), Some(256));
}

#[test]
fn storing_a_chunks_vector_again_replaces_it() {
    let h = TestHome::new();
    let idx = index_with_chunks(&h);
    vec::ensure_table(&idx, 4, false).unwrap();
    vec::put(&idx.conn, 1, &unit(4, 0)).unwrap();
    vec::put(&idx.conn, 1, &unit(4, 3)).unwrap();
    let hits = vec::search(&idx, &unit(4, 3), 5).unwrap();
    assert_eq!(hits, vec![(1, 0.0)]);
}

#[test]
fn an_explicit_rebuild_changes_the_width_and_clears_chunks() {
    let h = TestHome::new();
    let idx = index_with_chunks(&h);
    vec::set_embeddings(&idx, &[(1, unit(256, 1))]).unwrap();
    vec::ensure_table(&idx, 384, true).unwrap();
    assert_eq!(vec::dim(&idx.conn).unwrap(), Some(384));
    let count = |t: &str| -> i64 {
        idx.conn
            .query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(
        (count("chunks"), count("chunks_fts"), count("chunks_vec")),
        (0, 0, 0)
    );
    vec::ensure_table(&idx, 384, false).unwrap();
}

#[test]
fn the_global_cache_keys_by_model_and_text_and_checks_width() {
    let h = TestHome::new();
    let c = EmbeddingCache::open(&h.vex_home().join("cache/embeddings.db")).unwrap();
    // Cortex's key: sha256("model:text") as hex.
    assert_eq!(
        cache::key("hash-256", "abc"),
        "ac98ddf80b3b0071410fc15a247ec76f9c4feb426fefaef41089284fe29e07c9"
    );
    let k = cache::key("hash-256", "abc");
    assert_eq!(k.len(), 64);
    assert_ne!(k, cache::key("bge-small-en-v1.5", "abc"));
    assert!(c.get(&k, 2).unwrap().is_none());
    c.put_many(&[(k.clone(), &[0.5f32, -1.0][..])]).unwrap();
    assert_eq!(c.get(&k, 2).unwrap(), Some(vec![0.5, -1.0]));
    assert_eq!(
        c.get(&k, 3).unwrap(),
        None,
        "a row of another width is a miss"
    );
    assert_eq!(c.len().unwrap(), 1);
}
