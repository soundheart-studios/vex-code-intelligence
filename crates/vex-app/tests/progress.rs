//! Progress reporting for `init`, `index` and `embed`: each long phase is
//! announced with its total, advanced to it and ended, so a front end can
//! draw it; first use fetches the model up front (as its own stage) and a
//! failed fetch is not retried by the embedding that follows.

use std::cell::RefCell;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use vex_app::embed::{Backend, Plan, Settings};
use vex_app::progress::{Reporter, Stage};
use vex_test_support::TestHome;

#[derive(Debug, Clone, PartialEq)]
enum Event {
    Begin(Stage, u64),
    Advance(u64),
    End,
}

#[derive(Default)]
struct Recorder(RefCell<Vec<Event>>);

impl Reporter for Recorder {
    fn begin(&self, stage: Stage, total: u64) {
        self.0.borrow_mut().push(Event::Begin(stage, total));
    }
    fn advance(&self, n: u64) {
        self.0.borrow_mut().push(Event::Advance(n));
    }
    fn end(&self) {
        self.0.borrow_mut().push(Event::End);
    }
}

/// Each stage seen: (stage, announced total, units advanced), checking
/// that every stage ends before the next begins.
fn stages(events: &[Event]) -> Vec<(Stage, u64, u64)> {
    let mut out = Vec::new();
    let mut open: Option<(Stage, u64, u64)> = None;
    for e in events {
        match e {
            Event::Begin(s, t) => {
                assert!(
                    open.is_none(),
                    "{s:?} began inside another stage: {events:?}"
                );
                open = Some((*s, *t, 0));
            }
            Event::Advance(n) => open.as_mut().expect("advance outside a stage").2 += n,
            Event::End => out.push(open.take().expect("end outside a stage")),
        }
    }
    assert!(open.is_none(), "a stage never ended: {events:?}");
    out
}

fn recorded<T>(f: impl FnOnce() -> T) -> (T, Vec<Event>) {
    let rec = Rc::new(Recorder::default());
    let out = vex_app::progress::with(rec.clone(), f);
    let events = rec.0.borrow().clone();
    (out, events)
}

fn project(home: &TestHome) -> PathBuf {
    let dir = home.vex_home().join("work").join("p");
    for (rel, text) in [
        ("a.py", "def alpha():\n    return 1\n"),
        (
            "b.py",
            "from a import alpha\n\n\ndef beta():\n    return alpha()\n",
        ),
        (
            "c.py",
            "class Gamma:\n    def go(self):\n        return 3\n",
        ),
    ] {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(rel), text).unwrap();
    }
    dir
}

fn settings(home: &Path, backend: Backend, source: Option<String>) -> Settings {
    Settings {
        backend,
        models: home.join("models"),
        cache: home.join("cache").join("embeddings.db"),
        model_source: source,
        idle: Duration::from_secs(60),
        workers: 1,
    }
}

/// Serve `body` for every request; counts requests.
fn serve(body: Vec<u8>) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            counter.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf);
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(head.as_bytes());
            let _ = s.write_all(&body);
        }
    });
    (url, hits)
}

#[test]
fn init_reports_scan_parse_and_embed_with_their_totals() {
    let h = TestHome::new();
    let home = h.vex_home();
    let root = project(&h);
    let plan = Plan {
        settings: settings(home, Backend::Hash, None),
        defer: false,
    };
    let (r, events) = recorded(|| vex_app::project::init(home, &root, &[], &plan));
    let chunks = r.unwrap().index.chunks_ingested as u64;
    assert_eq!(
        stages(&events),
        vec![
            (Stage::Scan, 3, 3),
            (Stage::Parse, 3, 3),
            (Stage::Embed, chunks, chunks),
        ]
    );
}

#[test]
fn a_reindex_with_nothing_changed_parses_and_embeds_nothing() {
    let h = TestHome::new();
    let home = h.vex_home();
    let root = project(&h);
    let plan = Plan {
        settings: settings(home, Backend::Hash, None),
        defer: false,
    };
    vex_app::project::init(home, &root, &[], &plan).unwrap();
    let (_, events) = recorded(|| vex_app::project::index(home, &root, false, &plan).unwrap());
    let seen = stages(&events);
    assert_eq!(seen[0], (Stage::Scan, 3, 3), "{seen:?}");
    assert!(
        seen[1..].iter().all(|(_, total, _)| *total == 0),
        "{seen:?}"
    );
}

#[test]
fn embed_fills_pending_vectors_as_one_stage() {
    let h = TestHome::new();
    let home = h.vex_home();
    let root = project(&h);
    let deferred = Plan {
        settings: settings(home, Backend::Hash, None),
        defer: true,
    };
    let r = vex_app::project::init(home, &root, &[], &deferred).unwrap();
    let chunks = r.index.chunks_ingested as u64;
    let (fill, events) = recorded(|| {
        vex_app::embed::fill(home, &root, &settings(home, Backend::Hash, None)).unwrap()
    });
    assert_eq!(fill.embedded as u64, chunks);
    assert_eq!(stages(&events), vec![(Stage::Embed, chunks, chunks)]);
}

#[test]
fn first_init_downloads_the_model_as_its_own_stage_and_never_twice() {
    let h = TestHome::new();
    let home = h.vex_home();
    let root = project(&h);
    // Every file comes back corrupted: the download is refused.
    let (url, hits) = serve(b"not the model".to_vec());
    let plan = Plan {
        settings: settings(home, Backend::Bge, Some(url)),
        defer: false,
    };
    let (r, events) = recorded(|| vex_app::project::init(home, &root, &[], &plan));
    let r = r.expect("init still indexes, keyword-only");
    let seen = stages(&events);
    let pinned: u64 = vex_embed::weights::BGE_FILES.iter().map(|f| f.size).sum();
    assert_eq!(seen[0].0, Stage::Download, "{seen:?}");
    assert_eq!(seen[0].1, pinned, "the download total is the pinned size");
    assert!(seen[0].2 > 0, "bytes arriving are reported: {seen:?}");
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "a refused download is not fetched again by the embedding step"
    );
    let notice = r
        .index
        .embed
        .and_then(|e| e.notice)
        .expect("keyword-only notice");
    assert!(notice.contains("keyword-only"), "{notice}");
    assert!(notice.contains("failed verification"), "{notice}");
}

#[test]
fn a_deferred_init_does_not_download_the_model() {
    let h = TestHome::new();
    let home = h.vex_home();
    let root = project(&h);
    let (url, hits) = serve(b"not the model".to_vec());
    let plan = Plan {
        settings: settings(home, Backend::Bge, Some(url)),
        defer: true,
    };
    let (r, events) = recorded(|| vex_app::project::init(home, &root, &[], &plan));
    r.unwrap();
    assert!(
        stages(&events)
            .iter()
            .all(|(s, _, _)| *s != Stage::Download),
        "{events:?}"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

#[test]
fn without_a_reporter_nothing_is_reported_and_init_works() {
    let h = TestHome::new();
    let home = h.vex_home();
    let root = project(&h);
    let plan = Plan {
        settings: settings(home, Backend::Hash, None),
        defer: false,
    };
    assert!(vex_app::project::init(home, &root, &[], &plan).is_ok());
}
