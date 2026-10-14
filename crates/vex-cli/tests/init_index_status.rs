//! vex init / index / status / forget through the binary, with a private
//! VEX_HOME per test.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use vex_test_support::TestHome;

struct Env {
    home: TestHome,
}

impl Env {
    fn new() -> Env {
        Env {
            home: TestHome::new(),
        }
    }

    fn vex_home(&self) -> PathBuf {
        self.home.vex_home().join("vexhome")
    }

    fn vex(&self, cwd: &Path, args: &[&str]) -> (Option<i32>, Value, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_vex"))
            .args(args)
            .current_dir(cwd)
            .env("VEX_EMBED_BACKEND", "none")
            .env("VEX_DAEMON_AUTOSTART", "never")
            .env("VEX_HOME", self.vex_home())
            .output()
            .unwrap();
        let json = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
        (
            out.status.code(),
            json,
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn project(&self) -> PathBuf {
        let root = self.home.vex_home().join("proj");
        std::fs::create_dir_all(root.join("pkg")).unwrap();
        std::fs::write(
            root.join("pkg/a.py"),
            "from pkg.b import helper\n\ndef run():\n    return helper(1)\n",
        )
        .unwrap();
        std::fs::write(root.join("pkg/b.py"), "def helper(x):\n    return x\n").unwrap();
        root
    }
}

fn git(dir: &Path, args: &[&str]) {
    let st = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@localhost",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?}");
}

#[test]
fn init_registers_and_indexes_and_status_reports_counts() {
    let e = Env::new();
    let root = e.project();
    let (code, r, err) = e.vex(&root, &["init", "--json"]);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(r["schema"], 1);
    assert_eq!(r["registration"], "new");
    assert_eq!(r["index"]["added"], 2);
    assert!(r["index"]["call_edges"].as_i64().unwrap() >= 1);
    let id = r["project_id"].as_str().unwrap().to_string();
    assert!(
        e.vex_home()
            .join("projects")
            .join(&id)
            .join("index.db")
            .is_file()
    );

    let (code, s, _) = e.vex(&root, &["status", "--json"]);
    assert_eq!(code, Some(0));
    assert_eq!(s["schema"], 1);
    assert_eq!(s["project_id"], id.as_str());
    assert_eq!(
        (s["files"].as_i64(), s["symbols"].as_i64()),
        (Some(2), Some(2))
    );
    assert_eq!(s["freshness"]["stale"], false);
    assert!(s["edges"]["CALLS"].as_i64().unwrap() >= 1);
}

#[test]
fn an_edit_makes_status_stale_and_index_refreshes_only_that_file() {
    let e = Env::new();
    let root = e.project();
    e.vex(&root, &["init", "--json"]);
    std::fs::write(
        root.join("pkg/b.py"),
        "def helper(x):\n    return x + 1\n\ndef more():\n    pass\n",
    )
    .unwrap();
    let (_, s, _) = e.vex(&root, &["status", "--json"]);
    assert_eq!(s["freshness"]["workspace_changed"], true);
    assert_eq!(s["freshness"]["stale"], true);
    let (code, r, _) = e.vex(&root, &["index", "--json"]);
    assert_eq!(code, Some(0));
    assert_eq!(
        (
            r["index"]["added"].as_i64(),
            r["index"]["modified"].as_i64()
        ),
        (Some(0), Some(1))
    );
    let (_, s, _) = e.vex(&root, &["status", "--json"]);
    assert_eq!(s["freshness"]["stale"], false);
    assert_eq!(s["symbols"], 3);
}

#[test]
fn a_directory_without_an_index_exits_2_with_the_init_hint() {
    let e = Env::new();
    let root = e.project();
    for cmd in [vec!["status"], vec!["index"]] {
        let (code, _, err) = e.vex(&root, &cmd);
        assert_eq!(code, Some(2), "{cmd:?}");
        assert!(err.contains("vex init"), "{err}");
    }
}

#[test]
fn forget_removes_the_index_and_the_registry_entry() {
    let e = Env::new();
    let root = e.project();
    let (_, r, _) = e.vex(&root, &["init", "--json"]);
    let id = r["project_id"].as_str().unwrap().to_string();
    let (code, f, _) = e.vex(&root, &["forget", "--json"]);
    assert_eq!(code, Some(0));
    assert_eq!(f["project_id"], id.as_str());
    assert!(!e.vex_home().join("projects").join(&id).exists());
    let reg = std::fs::read_to_string(e.vex_home().join("projects/registry.json")).unwrap();
    assert!(!reg.contains(&id));
    assert_eq!(e.vex(&root, &["status"]).0, Some(2));
}

#[test]
fn the_project_is_the_git_toplevel_and_its_remote_is_recorded() {
    let e = Env::new();
    let root = e.project();
    git(&root, &["init", "-q"]);
    git(
        &root,
        &["remote", "add", "origin", "git@github.com:acme/proj.git"],
    );
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "init"]);
    // From a subdirectory, the project is the repository's toplevel.
    let (code, r, err) = e.vex(&root.join("pkg"), &["init", "--json"]);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(
        Path::new(r["root"].as_str().unwrap()),
        std::fs::canonicalize(&root).unwrap()
    );
    let reg: Value = serde_json::from_slice(
        &std::fs::read(e.vex_home().join("projects/registry.json")).unwrap(),
    )
    .unwrap();
    let project = &reg["projects"][r["project_id"].as_str().unwrap()];
    assert_eq!(project["remote"], "git@github.com:acme/proj.git");
    let (_, s, _) = e.vex(&root, &["status", "--json"]);
    assert_eq!(
        s["indexed_head"].as_str().map(str::len),
        Some(40),
        "HEAD recorded"
    );
    // A new commit moves HEAD: the index is stale until refreshed.
    std::fs::write(root.join("pkg/c.py"), "x = 1\n").unwrap();
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "more"]);
    let (_, s, _) = e.vex(&root, &["status", "--json"]);
    assert_eq!(s["freshness"]["head_changed"], true);
}

#[test]
fn a_second_init_reuses_the_project() {
    let e = Env::new();
    let root = e.project();
    let (_, first, _) = e.vex(&root, &["init", "--json"]);
    let (_, second, _) = e.vex(&root, &["init", "--json"]);
    assert_eq!(second["registration"], "existing");
    assert_eq!(second["project_id"], first["project_id"]);
    assert_eq!(second["index"]["added"], 0);
}
