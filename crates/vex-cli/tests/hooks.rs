//! `vex hook install / uninstall / status` and `vex sync`: git hooks keep
//! the index in step with checkouts, merges and rewrites. Installing chains
//! into hooks that exist (plain scripts, husky, lefthook) and never
//! overwrites them; a hook never fails the git command, even when vex
//! fails or is slow; and after a branch checkout the index reflects the
//! new branch, via the hook alone.

use serde_json::Value;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Duration, Instant};
use vex_test_support::TestHome;

struct Env {
    home: TestHome,
    root: PathBuf,
}

impl Env {
    fn new() -> Env {
        let home = TestHome::new();
        let root = home.vex_home().join("repo");
        std::fs::create_dir_all(root.join("app")).unwrap();
        std::fs::write(root.join("app/main.py"), "def on_main():\n    return 1\n").unwrap();
        let env = Env { home, root };
        env.git(&["init", "-q", "-b", "main"]);
        env.git(&["add", "-A"]);
        env.git(&["commit", "-q", "-m", "base"]);
        let root = env.root.canonicalize().unwrap();
        Env { root, ..env }
    }

    fn vex_home(&self) -> PathBuf {
        self.home.vex_home().join("vexhome")
    }

    fn envs(&self, c: &mut Command) {
        c.env("VEX_HOME", self.vex_home())
            .env("VEX_EMBED_BACKEND", "none")
            .env("VEX_DAEMON_AUTOSTART", "never")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
    }

    fn git(&self, args: &[&str]) -> Output {
        let mut c = Command::new("git");
        c.arg("-C")
            .arg(&self.root)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@localhost",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args);
        self.envs(&mut c);
        let out = c.output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    fn vex(&self, args: &[&str]) -> (Option<i32>, Value, String) {
        let mut c = Command::new(env!("CARGO_BIN_EXE_vex"));
        c.args(args).current_dir(&self.root);
        self.envs(&mut c);
        let out = c.output().unwrap();
        (
            out.status.code(),
            serde_json::from_slice(&out.stdout).unwrap_or(Value::Null),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn hooks_dir(&self) -> PathBuf {
        self.root.join(".git/hooks")
    }

    /// Whether search finds `name` in returned code from a fresh index.
    fn finds(&self, name: &str) -> bool {
        let (code, doc, _) = self.vex(&["--json", "search", name]);
        code == Some(0)
            && doc["results"].as_array().is_some_and(|r| {
                r.iter()
                    .any(|h| h["content"].as_str().is_some_and(|c| c.contains(name)))
            })
    }
}

fn eventually(limit: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    cond()
}

#[test]
fn install_chains_into_existing_hooks_and_never_overwrites() {
    let env = Env::new();
    env.vex(&["init"]);
    // A hook the user already has.
    let mine = env.hooks_dir().join("post-checkout");
    std::fs::write(
        &mine,
        "#!/bin/sh\necho ran > \"$(git rev-parse --show-toplevel)/.mine\"\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&mine, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let (code, doc, err) = env.vex(&["--json", "hook", "install"]);
    assert_eq!(code, Some(0), "{err}");
    let text = std::fs::read_to_string(&mine).unwrap();
    assert!(
        text.starts_with("#!/bin/sh\necho ran"),
        "the user's hook was overwritten: {text}"
    );
    assert_eq!(text.matches(">>> vex").count(), 1);
    // The script calls this very binary by absolute path.
    let exe = std::fs::canonicalize(env!("CARGO_BIN_EXE_vex")).unwrap();
    assert!(text.contains(&exe.display().to_string()), "{text}");
    let installed: Vec<String> = doc["hooks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["name"].as_str().unwrap().to_string())
        .collect();
    for name in ["post-checkout", "post-merge", "post-rewrite", "post-commit"] {
        assert!(
            installed.contains(&name.to_string()),
            "{name} not installed: {doc}"
        );
    }
    let action = doc["hooks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["name"] == "post-checkout")
        .unwrap()["action"]
        .clone();
    assert_eq!(action, "chained");
    // post-commit only records HEAD (all a commit moves).
    let post_commit = std::fs::read_to_string(env.hooks_dir().join("post-commit")).unwrap();
    assert!(
        post_commit.contains("sync --hook post-commit --record-head"),
        "{post_commit}"
    );
    // Idempotent, and says so.
    let (_, again, _) = env.vex(&["--json", "hook", "install"]);
    assert!(
        again["hooks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|h| h["action"] == "unchanged"),
        "{again}"
    );
    assert_eq!(
        std::fs::read_to_string(&mine)
            .unwrap()
            .matches(">>> vex")
            .count(),
        1
    );
    // Both run on a checkout.
    env.git(&["checkout", "-q", "-b", "feature"]);
    assert!(eventually(Duration::from_secs(5), || env
        .root
        .join(".mine")
        .exists()));
    // Status reports it; uninstall restores the user's hook exactly.
    let (_, status, _) = env.vex(&["--json", "hook", "status"]);
    assert!(
        status["hooks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|h| h["installed"] == true),
        "{status}"
    );
    env.vex(&["hook", "uninstall"]);
    assert_eq!(
        std::fs::read_to_string(&mine).unwrap(),
        "#!/bin/sh\necho ran > \"$(git rev-parse --show-toplevel)/.mine\"\n"
    );
    assert!(
        !env.hooks_dir().join("post-merge").exists(),
        "a hook vex created alone was left behind"
    );
}

#[test]
fn install_follows_hook_managers() {
    // husky: core.hooksPath=.husky/_ ; user hooks live in .husky/.
    let env = Env::new();
    std::fs::create_dir_all(env.root.join(".husky/_")).unwrap();
    std::fs::write(env.root.join(".husky/_/h"), "#!/usr/bin/env sh\n").unwrap();
    env.git(&["config", "core.hooksPath", ".husky/_"]);
    let (code, doc, err) = env.vex(&["--json", "hook", "install"]);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(doc["manager"], "husky", "{doc}");
    let husky = std::fs::read_to_string(env.root.join(".husky/post-checkout")).unwrap();
    assert!(husky.contains(">>> vex"), "{husky}");
    assert!(
        !env.root.join(".husky/_/post-checkout").exists(),
        "wrote into husky's generated dir"
    );
    assert!(
        !env.hooks_dir().join("post-checkout").exists(),
        "ignored core.hooksPath"
    );

    // lefthook: its generated hooks are chained into and the manager named.
    let env = Env::new();
    std::fs::write(
        env.root.join("lefthook.yml"),
        "post-checkout:\n  commands: {}\n",
    )
    .unwrap();
    let generated = "#!/bin/sh\n# lefthook generated\ncall_lefthook run \"post-checkout\" \"$@\"\n";
    std::fs::write(env.hooks_dir().join("post-checkout"), generated).unwrap();
    let (_, doc, _) = env.vex(&["--json", "hook", "install"]);
    assert_eq!(doc["manager"], "lefthook", "{doc}");
    let text = std::fs::read_to_string(env.hooks_dir().join("post-checkout")).unwrap();
    assert!(text.starts_with(generated), "{text}");
    assert!(text.contains(">>> vex"));
    assert!(doc["note"].as_str().unwrap().contains("lefthook"), "{doc}");
}

#[test]
fn a_hook_exits_zero_even_when_vex_fails_or_times_out() {
    let env = Env::new();
    // vex failing: no VEX_HOME, no HOME.
    let out = Command::new(env!("CARGO_BIN_EXE_vex"))
        .args(["sync", "--hook", "post-checkout"])
        .current_dir(&env.root)
        .env_remove("VEX_HOME")
        .env_remove("HOME")
        .env("VEX_DAEMON_AUTOSTART", "never")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    // A project with no index at all.
    let (code, _, _) = env.vex(&["sync", "--hook", "post-merge"]);
    assert_eq!(code, Some(0));
    // A sync that cannot finish in time (another writer holds the lock).
    env.vex(&["init"]);
    let (id, _) = vex_app::project::lookup(&env.vex_home(), &env.root).unwrap();
    let index = vex_store::registry::index_path(&env.vex_home(), &id);
    let _held = vex_app::project::lock_index(&index, Duration::from_secs(5)).unwrap();
    std::fs::write(env.root.join("app/new.py"), "def x():\n    return 0\n").unwrap();
    let start = Instant::now();
    let mut c = Command::new(env!("CARGO_BIN_EXE_vex"));
    c.args(["sync", "--hook", "post-checkout"])
        .current_dir(&env.root);
    env.envs(&mut c);
    c.env("VEX_HOOK_TIMEOUT_MS", "500");
    let out = c.output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "the hook waited {:?}",
        start.elapsed()
    );
    // And the installed script itself: a broken vex path still exits 0.
    env.vex(&["hook", "install"]);
    let script = env.hooks_dir().join("post-merge");
    let text = std::fs::read_to_string(&script).unwrap();
    let exe = std::fs::canonicalize(env!("CARGO_BIN_EXE_vex")).unwrap();
    std::fs::write(
        &script,
        text.replace(&exe.display().to_string(), "/nonexistent/vex"),
    )
    .unwrap();
    let status = Command::new("sh")
        .arg(&script)
        .current_dir(&env.root)
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
}

#[test]
fn after_a_branch_checkout_the_index_reflects_the_new_branch() {
    let env = Env::new();
    env.git(&["checkout", "-q", "-b", "other"]);
    std::fs::write(
        env.root.join("app/other.py"),
        "def only_on_other():\n    return 2\n",
    )
    .unwrap();
    env.git(&["add", "-A"]);
    env.git(&["commit", "-q", "-m", "other"]);
    env.git(&["checkout", "-q", "main"]);
    let (code, _, err) = env.vex(&["init"]);
    assert_eq!(code, Some(0), "{err}");
    let (code, _, err) = env.vex(&["hook", "install"]);
    assert_eq!(code, Some(0), "{err}");
    assert!(!env.finds("only_on_other"));

    env.git(&["checkout", "-q", "other"]);
    assert!(
        eventually(Duration::from_secs(15), || env.finds("only_on_other")),
        "the checkout hook never synced the new branch"
    );
    env.git(&["checkout", "-q", "main"]);
    assert!(
        eventually(Duration::from_secs(15), || {
            let (code, doc, _) = env.vex(&["--json", "search", "only_on_other"]);
            code == Some(0) && !doc.to_string().contains("app/other.py")
        }),
        "checking out main again left the other branch's file indexed"
    );
    // A commit records HEAD without reindexing anything stale.
    std::fs::write(
        env.root.join("app/main.py"),
        "def on_main():\n    return 3\n",
    )
    .unwrap();
    env.vex(&["sync", "--wait"]);
    env.git(&["commit", "-q", "-am", "edit"]);
    let ok = eventually(Duration::from_secs(10), || {
        let (_, s, _) = env.vex(&["--json", "status"]);
        s["freshness"]["stale"] == false
    });
    assert!(ok, "post-commit did not record HEAD");
}

#[test]
fn sync_refreshes_in_process_without_a_daemon() {
    let env = Env::new();
    env.vex(&["init"]);
    std::fs::write(
        env.root.join("app/synced.py"),
        "def synced_now():\n    return 1\n",
    )
    .unwrap();
    let (code, doc, err) = env.vex(&["--json", "sync", "--paths", "app/synced.py"]);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(doc["mode"], "in_process", "{doc}");
    assert_eq!(doc["index"]["added"], 1, "{doc}");
    assert!(env.finds("synced_now"));
    // --record-head only stamps HEAD.
    let (code, doc, _) = env.vex(&["--json", "sync", "--record-head"]);
    assert_eq!(code, Some(0));
    assert!(doc["head"].as_str().is_some_and(|h| h.len() == 40), "{doc}");
}

#[test]
fn install_from_a_subdirectory_still_targets_the_repositorys_hooks() {
    let env = Env::new();
    let mut c = Command::new(env!("CARGO_BIN_EXE_vex"));
    c.args(["--json", "hook", "install"])
        .current_dir(env.root.join("app"));
    env.envs(&mut c);
    let out = c.output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(env.hooks_dir().join("post-checkout").exists());
    assert!(
        !env.root.join("app/.git").exists(),
        "hooks landed relative to the cwd"
    );
}

#[test]
fn sync_wait_refreshes_before_answering_even_with_a_daemon() {
    let env = Env::new();
    env.vex(&["init"]);
    let daemon = |args: &[&str]| -> (Option<i32>, Value) {
        let mut c = Command::new(env!("CARGO_BIN_EXE_vex"));
        c.args(args).current_dir(&env.root);
        env.envs(&mut c);
        c.env("VEX_DAEMON_AUTOSTART", "always")
            .env("VEX_SOCKET", env.home.socket());
        let out = c.output().unwrap();
        (
            out.status.code(),
            serde_json::from_slice(&out.stdout).unwrap_or(Value::Null),
        )
    };
    type Vex<'a> = &'a dyn Fn(&[&str]) -> (Option<i32>, Value);
    struct Stop<'a>(Vex<'a>);
    impl Drop for Stop<'_> {
        fn drop(&mut self) {
            (self.0)(&["daemon", "stop"]);
        }
    }
    let _stop = Stop(&daemon);
    // Start it first, on a fresh index, and let its start-up catch-up
    // finish; only then make the change sync must index itself.
    let (code, started) = daemon(&["--json", "daemon", "start"]);
    assert_eq!(code, Some(0), "{started}");
    assert!(eventually(Duration::from_secs(10), || {
        daemon(&["--json", "daemon", "status"]).1["catching_up"] == false
    }));
    let new = env.root.join("app/w.py");
    std::fs::write(new, "def waited_sync():\n    return 1\n").unwrap();
    let (code, doc) = daemon(&["--json", "sync", "--wait"]);
    assert_eq!(code, Some(0));
    assert_eq!(doc["mode"], "in_process", "{doc}");
    assert_eq!(doc["index"]["added"], 1, "{doc}");
    let later = env.root.join("app/x.py");
    std::fs::write(later, "def later_sync():\n    return 2\n").unwrap();
    let (_, doc) = daemon(&["--json", "sync"]);
    let mode = doc["mode"].as_str();
    assert!(matches!(mode, Some("dispatched" | "in_flight")), "{doc}");
}

#[test]
fn a_hook_without_a_final_newline_keeps_its_last_line() {
    let env = Env::new();
    let mine = env.hooks_dir().join("post-merge");
    std::fs::write(&mine, "#!/bin/sh\necho merged > .merged").unwrap();
    env.vex(&["hook", "install"]);
    let text = std::fs::read_to_string(&mine).unwrap();
    assert!(text.contains("echo merged > .merged\n# >>> vex"), "{text}");
}

#[test]
fn an_odd_vex_path_survives_the_shell_quoting() {
    let env = Env::new();
    // A directory name with every character double quotes do not protect.
    let odd = env.home.vex_home().join("odd $HOME \"q\" `x` \\ dir");
    std::fs::create_dir_all(&odd).unwrap();
    let vex = odd.join("vex");
    std::fs::copy(env!("CARGO_BIN_EXE_vex"), &vex).unwrap();
    let mut c = Command::new(&vex);
    c.args(["--json", "hook", "install"]).current_dir(&env.root);
    env.envs(&mut c);
    assert!(c.output().unwrap().status.success());
    let script = std::fs::read_to_string(env.hooks_dir().join("post-merge")).unwrap();
    let line = script
        .lines()
        .find(|l| l.contains("sync --hook post-merge"))
        .unwrap()
        .to_string();
    // Run the hook's own command line with sh, asking for the version.
    let probe = line.replace(
        " sync --hook post-merge >/dev/null 2>&1 </dev/null &",
        " --version",
    );
    let out = Command::new("sh").arg("-c").arg(&probe).output().unwrap();
    assert!(
        String::from_utf8_lossy(&out.stdout).starts_with("vex "),
        "sh did not run the quoted path: {probe}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
