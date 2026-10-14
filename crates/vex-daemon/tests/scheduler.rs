//! The daemon's scheduler: a stale query dispatches exactly one background
//! refresh per project (later stale queries attach to it), queries after it
//! see fresh results, force_fresh answers with an edit made just before the
//! call even while a background refresh runs (one writer at a time), and an
//! idle daemon exits.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use vex_app::embed::{Backend, Plan, Settings};
use vex_app::freshness::Reindex;
use vex_app::search::{Answer, Request};
use vex_daemon::scheduler::Scheduler;
use vex_daemon::server::{self, Config, Exit, Handler};
use vex_test_support::TestHome;

fn settings(home: &Path) -> Settings {
    Settings {
        backend: Backend::Hash,
        models: home.join("models"),
        cache: home.join("cache/embeddings.db"),
        model_source: None,
        idle: Duration::from_secs(60),
        workers: 1,
    }
}

fn project(h: &TestHome) -> PathBuf {
    let dir = h.vex_home().join("work/p");
    std::fs::create_dir_all(dir.join("app")).unwrap();
    std::fs::write(
        dir.join("app/config.py"),
        "def parse_config(path):\n    return open(path).read()\n",
    )
    .unwrap();
    let plan = Plan {
        settings: settings(h.vex_home()),
        defer: false,
    };
    vex_app::project::init(h.vex_home(), &dir, &[], &plan).unwrap();
    dir
}

fn search(home: &Path, dir: &Path, q: &str, force_fresh: bool) -> vex_app::search::Outcome {
    let mut req = Request::single(q);
    req.force_fresh = force_fresh;
    vex_app::search::run_with(home, dir, &req, &settings(home)).unwrap()
}

/// The code the answer returned (not its notes, which echo the query).
fn text(out: &vex_app::search::Outcome) -> String {
    use vex_app::pyjson::Json;
    match &out.answer {
        Answer::Payload(p) => match p.get("results") {
            Some(Json::Arr(results)) => results
                .iter()
                .filter_map(|r| r.get("content").and_then(Json::as_str))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

/// The scheduler is process-wide (it is the freshness dispatcher), so the
/// tests that install it take turns.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn a_stale_query_dispatches_exactly_one_background_refresh() {
    let _turn = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let h = TestHome::new();
    let home = h.vex_home();
    let dir = project(&h);
    let sched = Scheduler::new(settings(home));
    sched.install();

    std::fs::write(
        dir.join("app/fresh.py"),
        "def brand_new_helper():\n    return 42\n",
    )
    .unwrap();
    // Several stale queries while the refresh is pending or running.
    let mut verdicts = Vec::new();
    for _ in 0..5 {
        verdicts.push(
            search(home, &dir, "brand_new_helper", false)
                .freshness
                .reindex,
        );
    }
    assert_eq!(verdicts[0], Reindex::Dispatched, "{verdicts:?}");
    for v in &verdicts[1..] {
        assert!(
            // Attached to the running job, already fresh, or (Cortex's
            // cooldown) withheld because a refresh just ran.
            matches!(
                v,
                Reindex::InFlight | Reindex::NotNeeded | Reindex::Cooldown
            ),
            "a second refresh was dispatched: {verdicts:?}"
        );
    }
    assert!(
        sched.wait_idle(Duration::from_secs(30)),
        "the refresh never finished"
    );
    assert_eq!(sched.refreshes(), 1, "exactly one background refresh");

    let after = search(home, &dir, "brand_new_helper", false);
    assert!(
        !after.freshness.index_stale,
        "still stale after the refresh"
    );
    assert!(
        text(&after).contains("brand_new_helper"),
        "{}",
        text(&after)
    );
    assert_eq!(after.freshness.reindex, Reindex::NotNeeded);
    // The refresh embedded what it indexed.
    let status = vex_app::project::status(home, &dir).unwrap();
    assert_eq!(status.embedded_chunks, status.chunks);
    vex_app::freshness::set_dispatcher(None);
}

#[test]
fn force_fresh_reflects_an_edit_made_just_before_the_call() {
    let _turn = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let h = TestHome::new();
    let home = h.vex_home();
    let dir = project(&h);
    let sched = Scheduler::new(settings(home));
    sched.install();
    for round in 0..3 {
        let name = format!("edited_fn_{round}");
        std::fs::write(
            dir.join("app/config.py"),
            format!("def {name}(path):\n    return path\n"),
        )
        .unwrap();
        // A plain stale query starts a background refresh; force_fresh,
        // right behind it, must still answer with the edit (it waits its
        // turn as the one writer instead of racing it).
        let _ = search(home, &dir, "anything", false);
        let out = search(home, &dir, &name, true);
        assert!(!out.freshness.index_stale, "round {round}: answered stale");
        assert!(text(&out).contains(&name), "round {round}: {}", text(&out));
    }
    assert!(sched.wait_idle(Duration::from_secs(30)));
    vex_app::freshness::set_dispatcher(None);
}

struct Nothing;
impl Handler for Nothing {
    fn call(&self, payload: serde_json::Value) -> serde_json::Value {
        payload
    }
}

#[test]
fn the_daemon_exits_after_the_configured_idle_period() {
    let h = TestHome::new();
    let cfg = Config {
        home: h.vex_home().to_path_buf(),
        socket: h.socket().to_path_buf(),
        version: "idle-test".into(),
        idle_exit: Some(Duration::from_millis(400)),
    };
    let socket = cfg.socket.clone();
    let start = Instant::now();
    let daemon = std::thread::spawn(move || server::run(cfg, Arc::new(Nothing)));
    // An open connection keeps it alive past the idle period.
    let mut conn = loop {
        if let vex_daemon::client::Connect::Ready(c) =
            vex_daemon::client::connect(&socket, "idle-test")
        {
            break c;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "daemon never served"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    std::thread::sleep(Duration::from_millis(900));
    assert!(!daemon.is_finished(), "exited while a client was connected");
    conn.request(&vex_daemon::protocol::Request::Ping).unwrap();
    drop(conn);
    let closed = Instant::now();
    let exit = daemon.join().unwrap().unwrap();
    assert_eq!(exit, Exit::Idle);
    let waited = closed.elapsed();
    assert!(
        waited >= Duration::from_millis(300) && waited < Duration::from_secs(5),
        "idle exit after {waited:?}"
    );
    assert!(!socket.exists());
}

/// Hold the project's write lock (as another writer would) for `hold`.
fn hold_lock(index: &Path, hold: Duration) -> std::thread::JoinHandle<()> {
    let lock = vex_app::project::lock_index(index, Duration::from_secs(5)).unwrap();
    std::thread::spawn(move || {
        std::thread::sleep(hold);
        drop(lock);
    })
}

fn index_of(home: &Path, dir: &Path) -> PathBuf {
    let (id, _) = vex_app::project::lookup(home, dir).unwrap();
    vex_store::registry::index_path(home, &id)
}

#[test]
fn writers_wait_for_the_one_writer() {
    let _turn = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let h = TestHome::new();
    let home = h.vex_home();
    let dir = project(&h);
    let index = index_of(home, &dir);
    let sched = Scheduler::new(settings(home));

    // A background refresh waits while another writer holds the lock.
    std::fs::write(
        dir.join("app/b.py"),
        "def waited_for_the_lock():\n    return 1\n",
    )
    .unwrap();
    let held = hold_lock(&index, Duration::from_millis(600));
    sched.request(&index, &dir, vex_daemon::scheduler::Trigger::Edit);
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        vex_app::project::is_stale(&index, &dir),
        "a refresh wrote while another writer held the lock"
    );
    held.join().unwrap();
    assert!(sched.wait_idle(Duration::from_secs(30)));
    assert!(!vex_app::project::is_stale(&index, &dir));

    // force_fresh waits for the lock too, then answers fresh.
    std::fs::write(
        dir.join("app/c.py"),
        "def forced_after_waiting():\n    return 2\n",
    )
    .unwrap();
    let start = Instant::now();
    let held = hold_lock(&index, Duration::from_millis(600));
    let out = search(home, &dir, "forced_after_waiting", true);
    assert!(
        start.elapsed() >= Duration::from_millis(500),
        "force_fresh did not wait"
    );
    assert!(
        text(&out).contains("forced_after_waiting"),
        "{}",
        text(&out)
    );
    held.join().unwrap();
}

#[test]
fn a_refresh_fills_vectors_a_deferred_index_left_pending() {
    let _turn = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let h = TestHome::new();
    let home = h.vex_home();
    let dir = h.vex_home().join("work/deferred");
    std::fs::create_dir_all(dir.join("app")).unwrap();
    std::fs::write(dir.join("app/a.py"), "def deferred_fn():\n    return 1\n").unwrap();
    let plan = Plan {
        settings: settings(home),
        defer: true,
    };
    vex_app::project::init(home, &dir, &[], &plan).unwrap();
    assert_eq!(
        vex_app::project::status(home, &dir)
            .unwrap()
            .embedded_chunks,
        0
    );
    std::fs::write(dir.join("app/b.py"), "def later_fn():\n    return 2\n").unwrap();
    let sched = Scheduler::new(settings(home));
    sched.request(
        &index_of(home, &dir),
        &dir,
        vex_daemon::scheduler::Trigger::Edit,
    );
    assert!(sched.wait_idle(Duration::from_secs(30)));
    let s = vex_app::project::status(home, &dir).unwrap();
    assert_eq!(
        s.embedded_chunks, s.chunks,
        "pending vectors were not drained"
    );
}

#[test]
fn changes_during_a_job_rerun_it_with_the_scan_they_need() {
    use vex_daemon::scheduler::Trigger;
    let _turn = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let h = TestHome::new();
    let home = h.vex_home();
    let dir = project(&h);
    let index = index_of(home, &dir);
    let sched = Scheduler::new(settings(home));

    // An edit, then more edits while that job runs: exactly one rerun.
    let held = hold_lock(&index, Duration::from_millis(400));
    assert_eq!(
        sched.request(&index, &dir, Trigger::Edit),
        Reindex::Dispatched
    );
    assert_eq!(
        sched.request(&index, &dir, Trigger::Edit),
        Reindex::InFlight
    );
    assert_eq!(
        sched.request(&index, &dir, Trigger::Edit),
        Reindex::InFlight
    );
    held.join().unwrap();
    assert!(sched.wait_idle(Duration::from_secs(30)));
    assert_eq!(sched.scans(), (2, 0));

    // A query while a job runs attaches: no rerun.
    let held = hold_lock(&index, Duration::from_millis(400));
    sched.request(&index, &dir, Trigger::Edit);
    assert_eq!(
        sched.request(&index, &dir, Trigger::Query),
        Reindex::InFlight
    );
    held.join().unwrap();
    assert!(sched.wait_idle(Duration::from_secs(30)));
    assert_eq!(sched.scans(), (3, 0));

    // A lost-events rescan during an incremental job reruns it in full.
    let held = hold_lock(&index, Duration::from_millis(400));
    sched.request(&index, &dir, Trigger::Edit);
    sched.request(&index, &dir, Trigger::Rescan);
    sched.request(&index, &dir, Trigger::Edit);
    held.join().unwrap();
    assert!(sched.wait_idle(Duration::from_secs(30)));
    assert_eq!(sched.scans(), (4, 1));
}

#[test]
fn query_triggered_refreshes_cool_down() {
    use vex_daemon::scheduler::Trigger;
    let _turn = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let h = TestHome::new();
    let home = h.vex_home();
    let dir = project(&h);
    let index = index_of(home, &dir);
    let sched = Scheduler::new(settings(home));
    assert_eq!(
        sched.request(&index, &dir, Trigger::Query),
        Reindex::Dispatched
    );
    assert!(sched.wait_idle(Duration::from_secs(30)));
    // Within the cooldown a query does not start another refresh...
    assert_eq!(
        sched.request(&index, &dir, Trigger::Query),
        Reindex::Cooldown
    );
    // ...but an edit seen by the watcher does.
    assert_eq!(
        sched.request(&index, &dir, Trigger::Edit),
        Reindex::Dispatched
    );
    assert!(sched.wait_idle(Duration::from_secs(30)));
}
