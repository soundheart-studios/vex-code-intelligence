//! File watching: an edit reaches search without `vex index`; edits made
//! while the daemon was down are picked up when it starts (catch-up by the
//! recorded stats and git HEAD); and a watcher overflow triggers a full
//! hash rescan, which catches even a change that kept the file's size and
//! mtime (invisible to the incremental scan).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};
use vex_app::embed::{Backend, Plan, Settings};
use vex_app::search::{Answer, Request};
use vex_daemon::scheduler::Scheduler;
use vex_daemon::watch::{self, Watcher};
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

fn git(root: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@localhost",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?}");
}

fn project(h: &TestHome) -> (PathBuf, PathBuf) {
    let dir = h.vex_home().join("work/p");
    std::fs::create_dir_all(dir.join("app")).unwrap();
    std::fs::write(
        dir.join("app/config.py"),
        "def parse_config(path):\n    return path\n",
    )
    .unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "base"]);
    let plan = Plan {
        settings: settings(h.vex_home()),
        defer: false,
    };
    let r = vex_app::project::init(h.vex_home(), &dir, &[], &plan).unwrap();
    let index = vex_store::registry::index_path(h.vex_home(), &r.project_id);
    (r.root, index)
}

/// A fresh answer whose returned code (not its notes, which echo the
/// query) contains `q`.
fn found(home: &Path, dir: &Path, q: &str) -> bool {
    use vex_app::pyjson::Json;
    let out = vex_app::search::run_with(home, dir, &Request::single(q), &settings(home)).unwrap();
    let Answer::Payload(p) = &out.answer else {
        return false;
    };
    let hit = match p.get("results") {
        Some(Json::Arr(results)) => results.iter().any(|r| {
            r.get("content")
                .and_then(Json::as_str)
                .is_some_and(|c| c.contains(q))
        }),
        _ => false,
    };
    !out.freshness.index_stale && hit
}

fn eventually(limit: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    cond()
}

/// The freshness dispatcher is process-wide; tests take turns.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn an_edit_is_reflected_in_search_without_running_vex_index() {
    let _turn = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let h = TestHome::new();
    let home = h.vex_home();
    let (root, index) = project(&h);
    let sched = Scheduler::new(settings(home));
    let watcher = Watcher::new(sched.clone(), Duration::from_millis(100));
    watcher.watch(&index, &root).unwrap();
    assert_eq!(watcher.watched(), vec![root.clone()]);

    std::fs::write(
        root.join("app/watched.py"),
        "def seen_by_the_watcher():\n    return 1\n",
    )
    .unwrap();
    assert!(
        eventually(Duration::from_secs(20), || found(
            home,
            &root,
            "seen_by_the_watcher"
        )),
        "the edit never reached search"
    );
    let (incremental, full) = sched.scans();
    assert!(incremental >= 1, "watcher refreshes are incremental");
    assert_eq!(full, 0, "no full rescan without an overflow");
    // Removing the file is noticed too.
    std::fs::remove_file(root.join("app/watched.py")).unwrap();
    assert!(eventually(Duration::from_secs(20), || {
        let out = vex_app::search::run_with(
            home,
            &root,
            &Request::single("seen_by_the_watcher"),
            &settings(home),
        )
        .unwrap();
        !out.freshness.index_stale && !out.answer.wire().contains("app/watched.py")
    }));
    drop(watcher);
}

#[test]
fn edits_made_while_the_daemon_was_down_are_picked_up_on_restart() {
    let _turn = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let h = TestHome::new();
    let home = h.vex_home();
    let (root, _) = project(&h);
    // "Down": nothing watching. Edit a file and move HEAD.
    std::fs::write(
        root.join("app/offline.py"),
        "def written_while_down():\n    return 2\n",
    )
    .unwrap();
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "offline work"]);
    // A file vex never indexed, and one gone missing.
    std::fs::remove_file(root.join("app/config.py")).unwrap();

    let sched = Scheduler::new(settings(home));
    let dispatched = watch::catch_up(home, &sched);
    assert_eq!(dispatched, 1, "the stale project is caught up");
    assert!(sched.wait_idle(Duration::from_secs(30)));
    assert!(found(home, &root, "written_while_down"));
    let out = vex_app::search::run_with(
        home,
        &root,
        &Request::single("parse_config"),
        &settings(home),
    )
    .unwrap();
    assert!(
        !out.answer.wire().contains("app/config.py"),
        "a deleted file survived catch-up"
    );
    // A fresh project is left alone.
    assert_eq!(watch::catch_up(home, &sched), 0);
}

#[test]
fn a_watcher_overflow_triggers_a_full_hash_rescan() {
    let _turn = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let h = TestHome::new();
    let home = h.vex_home();
    let (root, index) = project(&h);
    let sched = Scheduler::new(settings(home));
    let watcher = Watcher::new(sched.clone(), Duration::from_millis(100));
    watcher.watch(&index, &root).unwrap();

    // Same size, same mtime, different content: only hashing sees it.
    let path = root.join("app/config.py");
    let before = std::fs::read(&path).unwrap();
    let mtime: SystemTime = std::fs::metadata(&path).unwrap().modified().unwrap();
    let swapped = String::from_utf8(before.clone())
        .unwrap()
        .replace("parse_config", "covert_thing");
    assert_eq!(swapped.len(), before.len());
    std::fs::write(&path, &swapped).unwrap();
    let f = std::fs::File::options().write(true).open(&path).unwrap();
    f.set_modified(mtime).unwrap();
    drop(f);
    // Whatever the watcher made of the write, the incremental scan cannot
    // see this change.
    assert!(sched.wait_idle(Duration::from_secs(30)));
    std::thread::sleep(Duration::from_millis(400));
    assert!(sched.wait_idle(Duration::from_secs(30)));
    assert!(
        !found(home, &root, "covert_thing"),
        "an incremental scan saw an unchanged stat"
    );

    let rescan = notify::Event::new(notify::EventKind::Other).set_flag(notify::event::Flag::Rescan);
    watcher.handle(&root, &rescan);
    assert!(sched.wait_idle(Duration::from_secs(30)));
    assert_eq!(sched.scans().1, 1, "exactly one full rescan");
    assert!(
        found(home, &root, "covert_thing"),
        "the full rescan missed the change"
    );
}

/// Every row of the tables a query reads, in rowid order.
fn dump(idx: &vex_store::db::Index) -> Vec<String> {
    let mut out = Vec::new();
    for (table, cols) in [
        ("files", "path, hash, language"),
        (
            "chunks",
            "rowid, id, file_path, start_line, end_line, kind, language, content, token_est, symbols, full_chars, parent_id",
        ),
        ("chunks_fts", "rowid, id, content, file_path, symbols"),
        ("symbols", "rowid, *"),
        ("symbol_sites", "rowid, *"),
        ("edges", "rowid, *"),
        ("refs", "rowid, *"),
        ("calls", "rowid, *"),
        ("import_bindings", "rowid, *"),
        ("member_types", "rowid, *"),
    ] {
        let sql = format!("SELECT {cols} FROM {table} ORDER BY rowid");
        let mut stmt = idx.conn.prepare(&sql).unwrap();
        let n = stmt.column_count();
        let rows = stmt
            .query_map([], |r| {
                let mut line = table.to_string();
                for i in 0..n {
                    let v: rusqlite::types::Value = r.get(i)?;
                    line.push_str(&format!("|{v:?}"));
                }
                Ok(line)
            })
            .unwrap();
        out.extend(rows.map(Result::unwrap));
    }
    out
}

#[test]
fn an_incremental_scan_leaves_the_index_a_full_scan_would() {
    use vex_app::index::{Scan, run_scan};
    let h = TestHome::new();
    let root = h.vex_home().join("same");
    std::fs::create_dir_all(root.join("pkg")).unwrap();
    for (name, body) in [
        (
            "a",
            "from pkg.b import helper\n\ndef run():\n    return helper(1)\n",
        ),
        ("b", "def helper(x):\n    return x + 1\n"),
        ("c", "import os\n\ndef cwd():\n    return os.getcwd()\n"),
        ("d", "class Thing:\n    def go(self):\n        return 3\n"),
    ] {
        std::fs::write(root.join(format!("pkg/{name}.py")), body).unwrap();
    }
    let full = vex_store::db::Index::open(&h.vex_home().join("full.db")).unwrap();
    let inc = vex_store::db::Index::open(&h.vex_home().join("inc.db")).unwrap();
    run_scan(&full, &root, false, None, false, Scan::Full).unwrap();
    run_scan(&inc, &root, false, None, false, Scan::Full).unwrap();
    for round in 0..3 {
        // A modification, an addition and a removal each round.
        std::fs::write(
            root.join("pkg/b.py"),
            format!("def helper(x):\n    return x + {round}\n\n\ndef extra_{round}():\n    return helper({round})\n"),
        )
        .unwrap();
        std::fs::write(
            root.join(format!("pkg/new_{round}.py")),
            format!("from pkg.b import helper\n\ndef added_{round}():\n    return helper(0)\n"),
        )
        .unwrap();
        if round == 1 {
            std::fs::remove_file(root.join("pkg/d.py")).unwrap();
        }
        let a = run_scan(&full, &root, false, None, false, Scan::Full).unwrap();
        let b = run_scan(&inc, &root, false, None, false, Scan::Incremental).unwrap();
        assert_eq!(
            (a.added, a.modified, a.removed, a.chunks_ingested),
            (b.added, b.modified, b.removed, b.chunks_ingested),
            "round {round}"
        );
        assert_eq!(dump(&full), dump(&inc), "round {round}: the indexes differ");
    }
}

#[test]
fn only_git_internals_that_move_head_count_as_changes() {
    let root = Path::new("/r");
    for (path, want) in [
        ("/r/src/a.py", true),
        ("/r/.git/HEAD", true),
        ("/r/.git/refs/heads/main", true),
        ("/r/.git/packed-refs", true),
        ("/r/.git/objects/ab/cdef", false),
        ("/r/.git/index", false),
        ("/r/.github/workflows/ci.yml", true),
        ("/elsewhere/x.py", true),
    ] {
        assert_eq!(watch::relevant(root, Path::new(path)), want, "{path}");
    }
}

#[test]
fn changes_inside_directories_never_indexed_are_not_changes() {
    let root = Path::new("/r");
    for (path, want) in [
        ("/r/target/debug/build/x.o", false),
        ("/r/node_modules/ky/index.js", false),
        ("/r/web/node_modules/ky/index.js", false),
        ("/r/.claude/settings.local.json", false),
        ("/r/.codex/config.toml", false),
        ("/r/.gradle/caches/x.bin", false),
        ("/r/pkg.egg-info/PKG-INFO", false),
        // The directory entry itself, and look-alikes, still count.
        ("/r/target", true),
        ("/r/src/target.py", true),
        ("/r/src/targets/a.py", true),
        ("/r/.husky/pre-commit", true),
    ] {
        assert_eq!(watch::relevant(root, Path::new(path)), want, "{path}");
    }
}

#[test]
fn a_watcher_error_triggers_a_full_rescan() {
    let _turn = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let h = TestHome::new();
    let home = h.vex_home();
    let (root, index) = project(&h);
    let sched = Scheduler::new(settings(home));
    let watcher = Watcher::new(sched.clone(), Duration::from_secs(3600));
    watcher.watch(&index, &root).unwrap();
    watcher.on_result(
        &root,
        Err(vec![notify::Error::generic("events were dropped")]),
    );
    assert!(sched.wait_idle(Duration::from_secs(30)));
    assert_eq!(sched.scans(), (0, 1));
}

#[test]
fn catch_up_leaves_a_project_whose_root_is_gone_alone() {
    let _turn = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let h = TestHome::new();
    let home = h.vex_home();
    let (root, index) = project(&h);
    let chunks = |idx: &Path| -> i64 {
        let i = vex_store::db::Index::open(idx).unwrap();
        i.conn
            .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))
            .unwrap()
    };
    let before = chunks(&index);
    assert!(before > 0);
    // An unmounted drive, a deleted checkout: refreshing it would empty
    // the index.
    let parked = root.with_file_name("parked");
    std::fs::rename(&root, &parked).unwrap();
    let sched = Scheduler::new(settings(home));
    assert_eq!(watch::catch_up(home, &sched), 0);
    assert!(sched.wait_idle(Duration::from_secs(30)));
    assert_eq!(
        chunks(&index),
        before,
        "catch-up emptied a missing project's index"
    );
    std::fs::rename(&parked, &root).unwrap();
}
