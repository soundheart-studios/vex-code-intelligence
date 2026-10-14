//! The embedding pipeline: vectors are cached globally by model and content
//! (two projects with identical content embed once), `--defer-embed` leaves
//! every chunk pending until `vex embed` fills them, and with the model
//! unavailable indexing and search still work keyword-only and say so.

use std::path::{Path, PathBuf};
use std::time::Duration;
use vex_app::embed::{Backend, Plan, Settings};
use vex_app::search::{Answer, Request};
use vex_test_support::TestHome;

const FILES: &[(&str, &str)] = &[
    (
        "app/config.py",
        "import json\n\n\ndef parse_config(path):\n    with open(path) as f:\n        return json.load(f)\n\n\nclass Loader:\n    def load(self, name):\n        return parse_config(name)\n",
    ),
    (
        "app/auth.py",
        "def check_token(token):\n    return token.startswith('vex_')\n\n\ndef login(user, token):\n    if not check_token(token):\n        raise ValueError('bad token')\n    return user\n",
    ),
];

fn project(home: &TestHome, name: &str) -> PathBuf {
    let dir = home.vex_home().join("work").join(name);
    for (rel, text) in FILES {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    dir
}

fn settings(home: &Path, backend: Backend) -> Settings {
    Settings {
        backend,
        models: home.join("models"),
        cache: home.join("cache").join("embeddings.db"),
        model_source: None,
        idle: Duration::from_secs(60),
        workers: 1,
    }
}

fn plan(settings: Settings, defer: bool) -> Plan {
    Plan { settings, defer }
}

#[test]
fn two_projects_with_identical_content_share_cache_entries() {
    let h = TestHome::new();
    let home = h.vex_home();
    let (a, b) = (project(&h, "a"), project(&h, "b"));
    let p = plan(settings(home, Backend::Hash), false);

    let ra = vex_app::project::init(home, &a, &[], &p).unwrap();
    let ea = ra.index.embed.clone().expect("embed report");
    let chunks = ra.index.chunks_ingested;
    assert!(chunks >= 4, "fixture too small: {chunks}");
    assert_eq!((ea.cache_hits, ea.cache_misses), (0, chunks));
    assert_eq!((ea.embedded, ea.pending), (chunks, 0));
    assert_eq!(ea.model, "hash-256");
    assert_eq!(ea.notice, None);

    let rb = vex_app::project::init(home, &b, &[], &p).unwrap();
    let eb = rb.index.embed.clone().unwrap();
    assert_eq!(rb.index.chunks_ingested, chunks);
    assert_eq!(
        (eb.cache_hits, eb.cache_misses),
        (chunks, 0),
        "identical content was embedded again"
    );
    for dir in [&a, &b] {
        let s = vex_app::project::status(home, dir).unwrap();
        assert_eq!(s.embedded_chunks, chunks as i64);
    }
    let cache = vex_store::cache::EmbeddingCache::open(&home.join("cache/embeddings.db")).unwrap();
    assert_eq!(cache.len().unwrap(), chunks as i64);
}

#[test]
fn defer_embed_then_embed_reaches_full_coverage() {
    let h = TestHome::new();
    let home = h.vex_home();
    let dir = project(&h, "d");
    let s = settings(home, Backend::Hash);

    let r = vex_app::project::init(home, &dir, &[], &plan(s.clone(), true)).unwrap();
    let chunks = r.index.chunks_ingested;
    let e = r.index.embed.unwrap();
    assert_eq!((e.embedded, e.pending), (0, chunks));
    assert_eq!(
        vex_app::project::status(home, &dir)
            .unwrap()
            .embedded_chunks,
        0
    );

    let filled = vex_app::embed::fill(home, &dir, &s).unwrap();
    assert_eq!((filled.embedded, filled.remaining), (chunks, 0));
    assert_eq!(filled.notice, None);
    assert_eq!(
        vex_app::project::status(home, &dir)
            .unwrap()
            .embedded_chunks,
        chunks as i64
    );
    let again = vex_app::embed::fill(home, &dir, &s).unwrap();
    assert_eq!((again.embedded, again.remaining), (0, 0));

    // A deferred-then-filled index holds the same vectors as an eager one.
    let eager_dir = project(&h, "e");
    vex_app::project::init(home, &eager_dir, &[], &plan(s.clone(), false)).unwrap();
    let vectors = |dir: &Path| -> Vec<(String, Vec<u8>)> {
        let (id, _) = vex_app::project::lookup(home, dir).unwrap();
        let idx = vex_store::db::Index::open(&vex_store::registry::index_path(home, &id)).unwrap();
        let mut stmt = idx
            .conn
            .prepare("SELECT c.id, v.embedding FROM chunks c JOIN chunks_vec v ON v.rowid = c.rowid ORDER BY c.id")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    let (deferred, eager) = (vectors(&dir), vectors(&eager_dir));
    assert_eq!(deferred.len(), chunks);
    assert_eq!(
        deferred.iter().map(|r| &r.1).collect::<Vec<_>>(),
        eager.iter().map(|r| &r.1).collect::<Vec<_>>()
    );
}

#[test]
fn with_the_model_unavailable_search_returns_keyword_results_with_a_notice() {
    let h = TestHome::new();
    let home = h.vex_home();
    let dir = project(&h, "offline");
    let mut s = settings(home, Backend::Bge);
    // Nothing listens on the discard port: the download fails at once.
    s.model_source = Some("http://127.0.0.1:9".into());

    let r = vex_app::project::init(home, &dir, &[], &plan(s.clone(), false)).unwrap();
    let e = r.index.embed.unwrap();
    assert_eq!((e.embedded, e.pending), (0, r.index.chunks_ingested));
    let notice = e.notice.expect("an offline index says so");
    assert!(notice.contains("keyword"), "{notice}");
    assert!(
        notice.contains("vex models pull") || notice.contains("vex embed"),
        "{notice}"
    );

    let out = vex_app::search::run_with(home, &dir, &Request::single("parse_config"), &s).unwrap();
    let notice = out.notice.clone().expect("search says it is keyword-only");
    assert!(notice.contains("keyword"), "{notice}");
    let Answer::Payload(payload) = &out.answer else {
        panic!("no keyword results: {:?}", out.answer)
    };
    let text = vex_app::pyjson::dumps(payload, vex_app::pyjson::DEFAULT);
    assert!(text.contains("parse_config"), "{text}");
    assert!(
        text.contains("\"notice\""),
        "the payload carries the notice: {text}"
    );

    // `vex embed` offline fills nothing, keeps everything pending and says why.
    let filled = vex_app::embed::fill(home, &dir, &s).unwrap();
    assert_eq!(filled.embedded, 0);
    assert_eq!(filled.remaining, r.index.chunks_ingested);
    assert!(filled.notice.is_some());
}

#[test]
fn keyword_only_by_choice_says_nothing_and_writes_no_vectors() {
    let h = TestHome::new();
    let home = h.vex_home();
    let dir = project(&h, "none");
    let s = settings(home, Backend::None);
    let r = vex_app::project::init(home, &dir, &[], &plan(s.clone(), false)).unwrap();
    assert!(r.index.embed.is_none());
    let out = vex_app::search::run_with(home, &dir, &Request::single("parse_config"), &s).unwrap();
    assert_eq!(out.notice, None);
    assert!(!home.join("cache/embeddings.db").exists());
}

#[test]
fn an_index_of_another_width_is_refused_until_rebuilt() {
    let h = TestHome::new();
    let home = h.vex_home();
    let dir = project(&h, "width");
    let hash = plan(settings(home, Backend::Hash), false);
    vex_app::project::init(home, &dir, &[], &hash).unwrap();
    // Pretend the stored vectors are 384-wide (a bge-built index).
    let (id, _) = vex_app::project::lookup(home, &dir).unwrap();
    let idx = vex_store::db::Index::open(&vex_store::registry::index_path(home, &id)).unwrap();
    vex_store::vec::ensure_table(&idx, 384, true).unwrap();
    drop(idx);
    std::fs::write(dir.join("app/new.py"), "def fresh():\n    return 1\n").unwrap();
    let err = vex_app::project::index(home, &dir, false, &hash).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("384") && msg.contains("256") && msg.contains("vex index --force"),
        "{msg}"
    );
    let rebuilt = vex_app::project::index(home, &dir, true, &hash).unwrap();
    assert_eq!(rebuilt.index.embed.unwrap().pending, 0);
}

#[test]
fn a_failed_model_is_not_retried_for_every_batch() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Broken(Arc<AtomicUsize>);
    impl vex_embed::Embedder for Broken {
        fn model_name(&self) -> &str {
            "broken"
        }
        fn dim(&self) -> usize {
            4
        }
        fn context_tokens(&self) -> Option<usize> {
            None
        }
        fn embed(&self, _: &[&str]) -> Result<Vec<Vec<f32>>, vex_embed::EmbedError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(vex_embed::EmbedError::Model("offline".into()))
        }
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let mut s =
        vex_app::embed::Session::with_embedder(Box::new(Broken(calls.clone())), Backend::Bge, None);
    assert!(s.embed(&["a"]).is_none());
    assert!(s.embed(&["b"]).is_none());
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the failed model was tried again"
    );
    assert!(s.search_notice().unwrap().contains("offline"));
}

#[test]
fn the_backend_is_parsed_from_its_names() {
    for (name, want) in [
        ("", Some(Backend::Bge)),
        ("bge", Some(Backend::Bge)),
        (" AUTO ", Some(Backend::Bge)),
        ("hash", Some(Backend::Hash)),
        ("none", Some(Backend::None)),
        ("off", Some(Backend::None)),
        ("fastembed", None),
    ] {
        assert_eq!(Backend::parse(name), want, "{name:?}");
    }
}

#[test]
fn an_index_records_what_produced_its_vectors() {
    let h = TestHome::new();
    let home = h.vex_home();
    let dir = project(&h, "prov");
    vex_app::project::init(home, &dir, &[], &plan(settings(home, Backend::Hash), false)).unwrap();
    let (id, _) = vex_app::project::lookup(home, &dir).unwrap();
    let idx = vex_store::db::Index::open(&vex_store::registry::index_path(home, &id)).unwrap();
    let p = idx.embedding_provenance().unwrap();
    assert_eq!(
        (
            p.backend.as_deref(),
            p.model.as_deref(),
            p.dim,
            p.expected_dim
        ),
        (Some("hash"), Some("hash-256"), Some(256), Some(256))
    );
}

#[test]
fn embed_refuses_an_index_of_another_width_even_with_nothing_pending() {
    let h = TestHome::new();
    let home = h.vex_home();
    let dir = project(&h, "drainwidth");
    vex_app::project::init(home, &dir, &[], &plan(settings(home, Backend::Hash), false)).unwrap();
    let (id, _) = vex_app::project::lookup(home, &dir).unwrap();
    let idx = vex_store::db::Index::open(&vex_store::registry::index_path(home, &id)).unwrap();
    assert_eq!(vex_store::vec::count_pending(&idx.conn).unwrap(), 0);
    struct Wide;
    impl vex_embed::Embedder for Wide {
        fn model_name(&self) -> &str {
            "wide"
        }
        fn dim(&self) -> usize {
            384
        }
        fn context_tokens(&self) -> Option<usize> {
            None
        }
        fn embed(&self, t: &[&str]) -> Result<Vec<Vec<f32>>, vex_embed::EmbedError> {
            Ok(t.iter().map(|_| vec![0.0; 384]).collect())
        }
    }
    let mut s = vex_app::embed::Session::with_embedder(Box::new(Wide), Backend::Bge, None);
    let err = vex_app::embed::drain(&idx, &mut s).unwrap_err().to_string();
    assert!(err.contains("256") && err.contains("384"), "{err}");
}

#[test]
fn a_forced_refresh_during_search_embeds_what_it_indexes() {
    let h = TestHome::new();
    let home = h.vex_home();
    let dir = project(&h, "fresh");
    let s = settings(home, Backend::Hash);
    vex_app::project::init(home, &dir, &[], &plan(s.clone(), false)).unwrap();
    std::fs::write(
        dir.join("app/extra.py"),
        "def refreshed_helper():\n    return 'new'\n",
    )
    .unwrap();
    let mut req = Request::single("refreshed_helper");
    req.force_fresh = true;
    let out = vex_app::search::run_with(home, &dir, &req, &s).unwrap();
    assert!(out.answer.wire().contains("refreshed_helper"));
    let status = vex_app::project::status(home, &dir).unwrap();
    assert_eq!(
        status.embedded_chunks, status.chunks,
        "the refresh left chunks without vectors"
    );
}
