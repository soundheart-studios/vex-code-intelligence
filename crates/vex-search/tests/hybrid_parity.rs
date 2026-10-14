//! Hybrid retrieval against Cortex with the hash backend: the vector channel
//! fused with BM25 by the one RRF, over the keyword oracle's repo indexed
//! with vectors. Held to Cortex's retriever result sets (ids, relative
//! scores, channels; equal scores may swap places), its raw vector channel
//! (ties in distance may reorder) and the search tool's exact wire strings,
//! including the warm-up note a deferred index reports while it fills.
//! A list query embeds all its questions in one model call.
//! Fixtures: tests/fixtures/hybrid/cases.json, recorded from Cortex on
//! aarch64, where sqlite-vec's NEON L2 kernel fixes the order of near-ties;
//! the exact-parity tests run only there (x86's AVX kernel rounds differently).

use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use vex_app::embed::{Backend, Session};
use vex_app::search::{self, Request};
use vex_embed::{EmbedError, Embedder, HashEmbedder};
use vex_search::{retriever, vector};
use vex_store::db::Index;
use vex_test_support::TestHome;

fn fixture() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hybrid/cases.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn copy_tree(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            std::fs::create_dir_all(&dest).unwrap();
            copy_tree(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), dest).unwrap();
        }
    }
}

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .env("GIT_AUTHOR_NAME", "vex")
        .env("GIT_AUTHOR_EMAIL", "vex@example.invalid")
        .env("GIT_COMMITTER_NAME", "vex")
        .env("GIT_COMMITTER_EMAIL", "vex@example.invalid")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap()
        .status;
    assert!(status.success(), "git {args:?}");
}

/// The hash backend, counting its model calls and keeping what it embedded.
struct Counting {
    calls: Arc<AtomicUsize>,
    texts: Arc<std::sync::Mutex<Vec<String>>>,
}

impl Embedder for Counting {
    fn model_name(&self) -> &str {
        HashEmbedder.model_name()
    }
    fn dim(&self) -> usize {
        HashEmbedder.dim()
    }
    fn context_tokens(&self) -> Option<usize> {
        None
    }
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.texts
            .lock()
            .unwrap()
            .extend(texts.iter().map(|t| t.to_string()));
        HashEmbedder.embed(texts)
    }
}

fn hash_session() -> (Session, Arc<AtomicUsize>) {
    let (s, calls, _) = recording_session();
    (s, calls)
}

type Texts = Arc<std::sync::Mutex<Vec<String>>>;

fn recording_session() -> (Session, Arc<AtomicUsize>, Texts) {
    let calls = Arc::new(AtomicUsize::new(0));
    let texts: Texts = Arc::default();
    let s = Session::with_embedder(
        Box::new(Counting {
            calls: calls.clone(),
            texts: texts.clone(),
        }),
        Backend::Hash,
        None,
    );
    (s, calls, texts)
}

struct Project {
    _home: TestHome,
    root: PathBuf,
    idx: Index,
}

fn project(defer: bool) -> Project {
    let home = TestHome::new();
    let root = home.vex_home().join("qa");
    std::fs::create_dir_all(&root).unwrap();
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    copy_tree(&manifest.join("../../tests/fixtures/repos/qa/base"), &root);
    copy_tree(
        &manifest.join("../vex-app/tests/fixtures/search/overlay"),
        &root,
    );
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "base"]);
    let root = root.canonicalize().unwrap();
    let idx = Index::open(&home.vex_home().join("index.db")).unwrap();
    let (mut s, _) = hash_session();
    vex_app::index::run_with(&idx, &root, false, Some(&mut s), defer).unwrap();
    Project {
        _home: home,
        root,
        idx,
    }
}

fn request(call: &Value) -> Request {
    let (queries, list) = match &call["query"] {
        Value::Array(qs) => (
            qs.iter().map(|q| q.as_str().unwrap().to_string()).collect(),
            true,
        ),
        q => (vec![q.as_str().unwrap().to_string()], false),
    };
    Request {
        queries,
        list,
        top_k: call.get("top_k").and_then(Value::as_i64).unwrap_or(5),
        max_bytes: call.get("max_bytes").and_then(Value::as_i64).unwrap_or(0),
        path: call
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        select: String::new(),
        force_fresh: false,
    }
}

fn first_difference(got: &str, want: &str) -> String {
    let at = got
        .chars()
        .zip(want.chars())
        .position(|(a, b)| a != b)
        .unwrap_or(got.chars().count().min(want.chars().count()));
    let window = |s: &str| {
        s.chars()
            .skip(at.saturating_sub(60))
            .take(160)
            .collect::<String>()
    };
    format!(
        "first difference at char {at}\n  got  …{}\n  want …{}",
        window(got),
        window(want)
    )
}

fn index_version(idx: &Index) -> String {
    idx.conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'index_version'",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

fn check_wires(p: &Project, cases: &Value, cortex_version: &str, section: &str) -> Vec<String> {
    let vex_version = index_version(&p.idx);
    let mut failures = Vec::new();
    for case in cases.as_array().unwrap() {
        let req = request(&case["call"]);
        let (mut s, _) = hash_session();
        let out = search::search_with(&p.idx, &p.root, &req, &|| 0.0, Some(&mut s)).unwrap();
        let want = case["wire"].as_str().unwrap().replace(
            &format!("\"index_version\": \"{cortex_version}\""),
            &format!("\"index_version\": \"{vex_version}\""),
        );
        let got = out.answer.wire();
        if got != want {
            failures.push(format!(
                "{section} {}: {}",
                case["call"],
                first_difference(&got, &want)
            ));
        }
    }
    failures
}

fn assert_none(failures: Vec<String>) {
    assert!(
        failures.is_empty(),
        "{} case(s) differ:\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

/// Hits as Cortex records them: [id, score, channels, ranking_signals].
type Row = (String, f64, Vec<String>, Option<Vec<String>>);

/// Equal as sequences once each run of equal scores is compared as a set.
fn same_up_to_ties(got: &[Row], want: &[Row]) -> bool {
    if got.len() != want.len() {
        return false;
    }
    let runs = |rows: &[Row]| -> Vec<(u64, Vec<String>)> {
        let mut out: Vec<(u64, Vec<String>)> = Vec::new();
        for r in rows {
            let key = r.1.to_bits();
            let item = format!("{}|{:?}|{:?}", r.0, r.2, r.3);
            match out.last_mut() {
                Some((k, items)) if *k == key => items.push(item),
                _ => out.push((key, vec![item])),
            }
        }
        for (_, items) in &mut out {
            items.sort();
        }
        out
    };
    runs(got) == runs(want)
}

#[test]
#[cfg_attr(
    not(target_arch = "aarch64"),
    ignore = "Cortex was recorded with sqlite-vec's NEON kernels; x86 AVX sums distances in another order"
)]
fn hybrid_result_sets_equal_cortex() {
    let doc = fixture();
    let p = project(false);
    assert_eq!(vex_store::vec::dim(&p.idx.conn).unwrap(), Some(256));
    assert_eq!(vex_store::vec::count_pending(&p.idx.conn).unwrap(), 0);
    let mut failures = Vec::new();
    let mut semantic_reorders = 0;
    for case in doc["single"].as_array().unwrap() {
        let call = &case["call"];
        let query = call["query"].as_str().unwrap();
        let top_k = call.get("top_k").and_then(Value::as_i64).unwrap_or(5);
        let path = call.get("path").and_then(Value::as_str).unwrap_or("");
        let qvec = HashEmbedder.embed(&[query]).unwrap().remove(0);

        // The raw vector channel. Positions may differ only between chunks
        // at exactly the same distance (hash vectors tie often; the k-th
        // neighbour can tie with one just outside the slate).
        let depth = retriever::semantic_depth(top_k, "");
        let got_ids: Vec<String> = vector::search(&p.idx, &qvec, depth)
            .unwrap()
            .iter()
            .map(|c| c.id.clone())
            .collect();
        let want_ids: Vec<String> = serde_json::from_value(case["semantic"].clone()).unwrap();
        let wide = vector::search(&p.idx, &qvec, depth * 4).unwrap();
        let ranked = vex_store::vec::search(&p.idx, &qvec, depth * 4).unwrap();
        let distance: BTreeMap<&str, f64> = wide
            .iter()
            .zip(&ranked)
            .map(|(c, (_, d))| (c.id.as_str(), *d))
            .collect();
        let tie = |a: &str, b: &str| matches!((distance.get(a), distance.get(b)), (Some(x), Some(y)) if x == y);
        let aligned = got_ids.len() == want_ids.len()
            && got_ids
                .iter()
                .zip(&want_ids)
                .all(|(g, w)| g == w || tie(g, w));
        if !aligned {
            failures.push(format!("semantic {call}: {got_ids:?}\n  want {want_ids:?}"));
        }
        if got_ids != want_ids {
            semantic_reorders += 1;
        }

        let semantic =
            vector::search(&p.idx, &qvec, retriever::semantic_depth(top_k, path)).unwrap();
        let (hits, diag) = retriever::search(&p.idx.conn, semantic, query, top_k, path).unwrap();
        let got: Vec<Row> = hits
            .iter()
            .map(|h| {
                (
                    h.chunk.id.clone(),
                    h.score,
                    h.channels.clone(),
                    (!h.ranking_signals.is_empty()).then(|| h.ranking_signals.clone()),
                )
            })
            .collect();
        let want: Vec<Row> = serde_json::from_value(case["hits"].clone()).unwrap();
        if !same_up_to_ties(&got, &want) {
            failures.push(format!(
                "retriever {call}:\n  got  {got:?}\n  want {want:?}"
            ));
        }
        let want_diag: BTreeMap<String, Value> =
            serde_json::from_value(case["diagnostics"].clone()).unwrap();
        let got_diag = serde_json::to_value(&diag).unwrap();
        for (k, v) in &want_diag {
            if &got_diag[k] != v {
                failures.push(format!("diagnostics {call} {k}: {} vs {v}", got_diag[k]));
            }
        }
    }
    eprintln!("semantic channel order differed (ties) in {semantic_reorders} case(s)");
    assert_none(failures);
}

#[test]
#[cfg_attr(
    not(target_arch = "aarch64"),
    ignore = "Cortex was recorded with sqlite-vec's NEON kernels; x86 AVX sums distances in another order"
)]
fn hybrid_wire_strings_equal_cortex() {
    let doc = fixture();
    let p = project(false);
    let version = doc["index_version"].as_str().unwrap();
    let mut failures = check_wires(&p, &doc["single"], version, "single");
    failures.extend(check_wires(&p, &doc["list"], version, "list"));
    assert_none(failures);
}

#[test]
#[cfg_attr(
    not(target_arch = "aarch64"),
    ignore = "Cortex was recorded with sqlite-vec's NEON kernels; x86 AVX sums distances in another order"
)]
fn a_filling_index_reports_its_warmup_like_cortex() {
    let doc = fixture();
    let warm = &doc["warmup"];
    let p = project(true);
    let chunks: i64 = p
        .idx
        .conn
        .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(chunks, warm["chunks"].as_i64().unwrap());
    let version = warm["index_version"].as_str().unwrap();
    let before = &warm["before"];
    assert_eq!(
        vex_store::vec::count_pending(&p.idx.conn).unwrap(),
        before["pending"].as_i64().unwrap()
    );
    let mut failures = check_wires(&p, &before["single"], version, "before");
    failures.extend(check_wires(&p, &before["list"], version, "before-list"));

    // One drain batch, as Cortex's drain_pending(max_batches=1).
    let batch = vex_store::vec::pending(&p.idx.conn, vex_app::embed::BATCH).unwrap();
    let texts: Vec<&str> = batch.iter().map(|(_, c)| c.as_str()).collect();
    let vecs = HashEmbedder.embed(&texts).unwrap();
    let rows: Vec<(i64, Vec<f32>)> = batch.iter().map(|(r, _)| *r).zip(vecs).collect();
    vex_store::vec::set_embeddings(&p.idx, &rows).unwrap();
    let partial = &warm["partial"];
    assert_eq!(
        vex_store::vec::count_pending(&p.idx.conn).unwrap(),
        partial["pending"].as_i64().unwrap()
    );
    failures.extend(check_wires(&p, &partial["single"], version, "partial"));
    failures.extend(check_wires(&p, &partial["list"], version, "partial-list"));
    assert_none(failures);
}

#[test]
fn a_list_query_with_n_questions_makes_one_embedding_call() {
    let p = project(false);
    for (queries, list) in [
        (
            vec![
                "TimestampSigner unsign",
                "BadSignature",
                "want_bytes base64_encode",
                "Serializer loads",
                "URLSafeSerializer",
                "JSON dumps",
            ],
            true,
        ),
        (vec!["  sign  ", "", "verify"], true),
        (vec!["want_bytes"], false),
    ] {
        let mut req = Request::single("");
        req.queries = queries.iter().map(|q| q.to_string()).collect();
        req.list = list;
        let (mut s, calls) = hash_session();
        let out = search::search_with(&p.idx, &p.root, &req, &|| 0.0, Some(&mut s)).unwrap();
        assert!(out.notice.is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 1, "{queries:?}");
    }
    // A refused list (too many questions) embeds nothing.
    let mut req = Request::single("");
    req.queries = (0..7).map(|i| format!("q{i}")).collect();
    req.list = true;
    let (mut s, calls) = hash_session();
    search::search_with(&p.idx, &p.root, &req, &|| 0.0, Some(&mut s)).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn a_query_embedder_of_another_width_is_refused() {
    let p = project(false);
    struct Wide;
    impl Embedder for Wide {
        fn model_name(&self) -> &str {
            "wide"
        }
        fn dim(&self) -> usize {
            384
        }
        fn context_tokens(&self) -> Option<usize> {
            None
        }
        fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
            Ok(texts.iter().map(|_| vec![0.0; 384]).collect())
        }
    }
    let mut s = Session::with_embedder(Box::new(Wide), Backend::Bge, None);
    let out = search::search_with(
        &p.idx,
        &p.root,
        &Request::single("want_bytes"),
        &|| 0.0,
        Some(&mut s),
    )
    .unwrap();
    let wire = out.answer.wire();
    assert!(
        wire.starts_with("CORTEX ERROR (index_dimension_mismatch)"),
        "{wire}"
    );
    assert!(
        wire.contains("256") && wire.contains("384") && wire.contains("vex index --force"),
        "{wire}"
    );
}

#[test]
fn queries_are_embedded_exactly_as_the_retriever_sees_them() {
    let p = project(false);
    // A single query is embedded raw, as Cortex's retriever receives it.
    let (mut s, _, texts) = recording_session();
    search::search_with(
        &p.idx,
        &p.root,
        &Request::single("  want_bytes\t"),
        &|| 0.0,
        Some(&mut s),
    )
    .unwrap();
    assert_eq!(*texts.lock().unwrap(), vec!["  want_bytes\t".to_string()]);
    // A list's questions are stripped and the empty ones dropped.
    let (mut s, _, texts) = recording_session();
    let mut req = Request::single("");
    req.queries = vec!["  sign  ".into(), "".into(), " verify".into()];
    req.list = true;
    search::search_with(&p.idx, &p.root, &req, &|| 0.0, Some(&mut s)).unwrap();
    assert_eq!(
        *texts.lock().unwrap(),
        vec!["sign".to_string(), "verify".to_string()]
    );
}

#[test]
fn a_broken_vector_table_answers_nothing_rather_than_failing() {
    let home = TestHome::new();
    let idx = Index::open(&home.vex_home().join("broken.db")).unwrap();
    // The width is recorded but the vec0 table is gone.
    idx.conn
        .execute(
            "INSERT INTO meta (key, value) VALUES ('embedding_dim', '4')",
            [],
        )
        .unwrap();
    assert!(
        vector::search(&idx, &[1.0, 0.0, 0.0, 0.0], 5)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn the_warmup_percentage_truncates_like_cortex() {
    // Cortex: pct = int((total - pending) / total * 100). Find a fill level
    // where truncating differs from rounding and from an off-by-one total.
    let home = TestHome::new();
    let root = home.vex_home().join("tiny");
    std::fs::create_dir_all(&root).unwrap();
    for name in ["alpha", "beta", "gamma"] {
        std::fs::write(
            root.join(format!("{name}.py")),
            format!("def {name}_fn():\n    return 1\n"),
        )
        .unwrap();
    }
    let idx = Index::open(&home.vex_home().join("tiny.db")).unwrap();
    let (mut s, _) = hash_session();
    vex_app::index::run_with(&idx, &root, false, Some(&mut s), true).unwrap();
    let total = vex_store::vec::count_pending(&idx.conn).unwrap();
    let pct = |done: i64, t: i64| (done as f64 / t as f64 * 100.0) as i64;
    let done = (1..total)
        .find(|&k| {
            let exact = k as f64 / total as f64 * 100.0;
            pct(k, total) != exact.round() as i64 && pct(k, total) != pct(k + 1, total + 1)
        })
        .expect("a distinguishing fill level");
    let batch = vex_store::vec::pending(&idx.conn, done as usize).unwrap();
    let texts: Vec<&str> = batch.iter().map(|(_, c)| c.as_str()).collect();
    let rows: Vec<(i64, Vec<f32>)> = batch
        .iter()
        .map(|(r, _)| *r)
        .zip(HashEmbedder.embed(&texts).unwrap())
        .collect();
    vex_store::vec::set_embeddings(&idx, &rows).unwrap();
    let (mut s, _) = hash_session();
    let out = search::search_with(
        &idx,
        &root,
        &Request::single("alpha_fn"),
        &|| 0.0,
        Some(&mut s),
    )
    .unwrap();
    let want = format!(
        "\"warmup\": \"index {}% embedded \\u2014 semantic recall still improving; keyword matching covers everything already\"",
        pct(done, total)
    );
    assert!(
        out.answer.wire().contains(&want),
        "{}\nwant {want}",
        out.answer.wire()
    );
}
