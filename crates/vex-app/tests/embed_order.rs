//! Indexing and `vex embed` hand the model its chunks longest first, in
//! groups of the size the embedder asks for: equal-length neighbours pad
//! least, and a parallel embedder gets enough work per call to keep every
//! session busy.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use vex_app::embed::{Backend, Plan, Session, Settings};
use vex_test_support::TestHome;

type Calls = Arc<Mutex<Vec<Vec<String>>>>;

struct Recording(Calls);

impl vex_embed::Embedder for Recording {
    fn model_name(&self) -> &str {
        "recording"
    }
    fn dim(&self) -> usize {
        256
    }
    fn context_tokens(&self) -> Option<usize> {
        None
    }
    fn preferred_batch(&self) -> usize {
        3
    }
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, vex_embed::EmbedError> {
        self.0
            .lock()
            .unwrap()
            .push(texts.iter().map(|t| t.to_string()).collect());
        Ok(texts.iter().map(|_| vec![0.5; 256]).collect())
    }
}

fn project(home: &TestHome) -> PathBuf {
    let dir = home.vex_home().join("work").join("p");
    for (i, n) in [1, 40, 3, 25, 8, 60, 2].iter().enumerate() {
        let body: String = (0..*n).map(|k| format!("    x{k} = {k}\n")).collect();
        let p = dir.join(format!("m{i}.py"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, format!("def f{i}():\n{body}    return 0\n")).unwrap();
    }
    dir
}

fn settings(home: &Path) -> Settings {
    Settings {
        backend: Backend::Hash,
        models: home.join("models"),
        cache: home.join("cache").join("embeddings.db"),
        model_source: None,
        idle: Duration::from_secs(60),
        workers: 1,
    }
}

fn assert_longest_first_in_groups(calls: &Calls, expected: usize) {
    let calls = calls.lock().unwrap();
    assert!(calls.len() > 1, "fixture too small: {calls:?}");
    assert!(
        calls.iter().all(|c| c.len() <= 3),
        "a call exceeded the group"
    );
    let lens: Vec<usize> = calls.iter().flatten().map(String::len).collect();
    assert_eq!(lens.len(), expected);
    assert!(
        lens.windows(2).all(|w| w[0] >= w[1]),
        "not longest first: {lens:?}"
    );
}

#[test]
fn indexing_embeds_longest_first_in_the_embedders_groups() {
    let h = TestHome::new();
    let root = project(&h);
    let idx = vex_store::db::Index::open(&h.vex_home().join("index.db")).unwrap();
    let calls = Calls::default();
    let mut s = Session::with_embedder(Box::new(Recording(calls.clone())), Backend::Bge, None);
    let report = vex_app::index::run_with(&idx, &root, false, Some(&mut s), false).unwrap();
    assert_longest_first_in_groups(&calls, report.chunks_ingested);
}

#[test]
fn filling_a_deferred_index_embeds_longest_first_in_the_embedders_groups() {
    let h = TestHome::new();
    let home = h.vex_home();
    let root = project(&h);
    let plan = Plan {
        settings: settings(home),
        defer: true,
    };
    let r = vex_app::project::init(home, &root, &[], &plan).unwrap();
    let (id, _) = vex_app::project::lookup(home, &root).unwrap();
    let idx = vex_store::db::Index::open(&vex_store::registry::index_path(home, &id)).unwrap();
    let calls = Calls::default();
    let mut s = Session::with_embedder(Box::new(Recording(calls.clone())), Backend::Bge, None);
    let done = vex_app::embed::drain(&idx, &mut s).unwrap();
    assert_eq!(done, r.index.chunks_ingested);
    assert_longest_first_in_groups(&calls, done);
}
