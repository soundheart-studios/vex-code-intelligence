//! `vex init` at a terminal: structure is indexed first, the model is timed
//! on a sample of the longest chunks, and only when the rest would take
//! long is the person asked whether to embed now, in the background (the
//! daemon) or later (`vex embed`).

use std::path::{Path, PathBuf};
use std::time::Duration;
use vex_app::embed::{Backend, Plan, Settings};
use vex_app::project::{EmbedChoice, init_measured};
use vex_test_support::TestHome;

fn project(home: &TestHome) -> PathBuf {
    let dir = home.vex_home().join("work").join("p");
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..150 {
        let body: String = (0..(i % 9) + 1)
            .map(|k| format!("    v{k} = {k} * {i}\n"))
            .collect();
        std::fs::write(
            dir.join(format!("m{i}.py")),
            format!("def f{i}():\n{body}    return 0\n"),
        )
        .unwrap();
    }
    dir
}

fn plan(home: &Path, backend: Backend, defer: bool) -> Plan {
    Plan {
        settings: Settings {
            backend,
            models: home.join("models"),
            cache: home.join("cache").join("embeddings.db"),
            model_source: None,
            idle: Duration::from_secs(60),
            workers: 1,
        },
        defer,
    }
}

fn pending(home: &Path, root: &Path) -> i64 {
    let s = vex_app::project::status(home, root).unwrap();
    s.chunks - s.embedded_chunks
}

#[test]
fn a_long_estimate_asks_and_later_leaves_the_rest_pending() {
    let h = TestHome::new();
    let home = h.vex_home();
    let root = project(&h);
    let mut asked = None;
    let (r, choice) = init_measured(
        home,
        &root,
        &[],
        &plan(home, Backend::Hash, false),
        Duration::ZERO,
        |e| {
            asked = Some((e.pending, e.estimate));
            EmbedChoice::Later
        },
    )
    .unwrap();
    let chunks = r.index.chunks_ingested;
    let e = r.index.embed.expect("embed report");
    let (left, _) = asked.expect("a long estimate asks");
    assert_eq!(choice, Some(EmbedChoice::Later));
    // The sample is embedded; the rest waits for `vex embed` or the daemon.
    assert!(e.embedded > 0 && e.embedded < chunks, "{e:?}");
    assert_eq!(e.pending, left);
    assert_eq!(e.embedded + e.pending, chunks);
    assert_eq!(pending(home, &root), left as i64);
}

#[test]
fn now_embeds_everything_before_returning() {
    let h = TestHome::new();
    let home = h.vex_home();
    let root = project(&h);
    let (r, choice) = init_measured(
        home,
        &root,
        &[],
        &plan(home, Backend::Hash, false),
        Duration::ZERO,
        |_| EmbedChoice::Now,
    )
    .unwrap();
    let e = r.index.embed.unwrap();
    assert_eq!(choice, Some(EmbedChoice::Now));
    assert_eq!((e.embedded, e.pending), (r.index.chunks_ingested, 0));
    assert_eq!(pending(home, &root), 0);
}

#[test]
fn a_short_estimate_embeds_without_asking() {
    let h = TestHome::new();
    let home = h.vex_home();
    let root = project(&h);
    let (r, choice) = init_measured(
        home,
        &root,
        &[],
        &plan(home, Backend::Hash, false),
        Duration::from_secs(3600),
        |_| panic!("asked about a short embed"),
    )
    .unwrap();
    let e = r.index.embed.unwrap();
    assert_eq!(choice, None);
    assert_eq!((e.embedded, e.pending), (r.index.chunks_ingested, 0));
}

#[test]
fn the_estimate_covers_what_is_left() {
    let h = TestHome::new();
    let home = h.vex_home();
    let root = project(&h);
    let mut seen = None;
    init_measured(
        home,
        &root,
        &[],
        &plan(home, Backend::Hash, false),
        Duration::ZERO,
        |e| {
            seen = Some(e.clone());
            EmbedChoice::Later
        },
    )
    .unwrap();
    let e = seen.unwrap();
    assert!(e.pending > 0);
    assert!(e.sampled > 0);
    assert!(e.estimate > Duration::ZERO, "{e:?}");
}

#[test]
fn deferred_off_or_offline_never_asks() {
    let never = |_: &vex_app::project::EmbedEstimate| -> EmbedChoice { panic!("asked") };
    for (backend, defer) in [(Backend::Hash, true), (Backend::None, false)] {
        let h = TestHome::new();
        let home = h.vex_home();
        let root = project(&h);
        let (r, choice) = init_measured(
            home,
            &root,
            &[],
            &plan(home, backend, defer),
            Duration::ZERO,
            never,
        )
        .unwrap();
        assert_eq!(choice, None);
        if let Some(e) = r.index.embed {
            assert_eq!(e.embedded, 0);
        }
    }
    // The model cannot download: keyword-only, said so, nothing asked.
    let h = TestHome::new();
    let home = h.vex_home();
    let root = project(&h);
    let mut p = plan(home, Backend::Bge, false);
    // Nothing listens on the discard port: the download fails at once.
    p.settings.model_source = Some("http://127.0.0.1:9".into());
    let (r, choice) = init_measured(home, &root, &[], &p, Duration::ZERO, never).unwrap();
    assert_eq!(choice, None);
    let e = r.index.embed.unwrap();
    assert_eq!((e.embedded, e.pending), (0, r.index.chunks_ingested));
    assert!(e.notice.unwrap().contains("keyword"));
}

#[test]
fn the_daemon_embeds_with_fewer_sessions_than_a_foreground_command() {
    let workers = |background: bool| {
        vex_app::embed::set_background(background);
        Settings::from_env(Path::new("/nonexistent")).workers
    };
    if std::env::var_os("VEX_EMBED_WORKERS").is_none() {
        assert_eq!(workers(false), 0, "foreground picks per core");
        let bg = workers(true);
        assert!(bg >= 1, "background runs at least one session");
        assert!(bg <= vex_embed::batching::default_workers(16) / 2);
    }
    vex_app::embed::set_background(false);
}
