//! `vex search` through the binary: a stale index still answers, carries a
//! freshness block, and exits 3; a fresh one exits 0; no index exits 2.

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

    fn vex(&self, cwd: &Path, args: &[&str]) -> (Option<i32>, Value, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_vex"))
            .args(args)
            .current_dir(cwd)
            .env("VEX_EMBED_BACKEND", "none")
            .env("VEX_DAEMON_AUTOSTART", "never")
            .env("VEX_HOME", self.home.vex_home().join("vexhome"))
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        (
            out.status.code(),
            serde_json::from_str(&stdout).unwrap_or(Value::Null),
            stdout,
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// A committed two-file project.
    fn project(&self) -> PathBuf {
        let root = self.home.vex_home().join("proj");
        std::fs::create_dir_all(root.join("pkg")).unwrap();
        std::fs::write(
            root.join("pkg/a.py"),
            "from pkg.b import helper\n\ndef run_signer():\n    return helper(1)\n",
        )
        .unwrap();
        std::fs::write(root.join("pkg/b.py"), "def helper(x):\n    return x\n").unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["add", "-A"],
            &["commit", "-q", "-m", "init"],
        ] {
            let ok = Command::new("git")
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@localhost",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .current_dir(&root)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        }
        root
    }
}

#[test]
fn stale_index_returns_results_with_freshness_and_exit_3() {
    let e = Env::new();
    let root = e.project();
    let (code, _, _, err) = e.vex(&root, &["init", "--json"]);
    assert_eq!(code, Some(0), "{err}");

    let (code, fresh, _, err) = e.vex(&root, &["search", "helper", "--json"]);
    assert_eq!(code, Some(0), "fresh index: {err}");
    assert_eq!(fresh["schema"], 1);
    assert!(
        fresh.get("freshness").is_none(),
        "a fresh answer carries no freshness block"
    );
    assert!(!fresh["results"].as_array().unwrap().is_empty());

    std::fs::write(root.join("pkg/b.py"), "def helper(x):\n    return x + 1\n").unwrap();
    let (code, stale, stdout, err) = e.vex(&root, &["search", "helper", "--json"]);
    assert_eq!(code, Some(3), "stale index must exit 3: {err}");
    assert_eq!(stale["schema"], 1);
    assert!(
        !stale["results"].as_array().unwrap().is_empty(),
        "results still returned"
    );
    assert_eq!(stale["freshness"]["index_stale"], true);
    assert_eq!(stale["freshness"]["reindex"], "no_dispatcher");
    assert!(stale["freshness"]["index_age_s"].is_null());
    assert!(
        !stdout.contains('\u{1b}'),
        "--json output has no ANSI codes"
    );

    let (code, _, _, err) = e.vex(&root, &["search", "helper"]);
    assert_eq!(code, Some(3), "human output exits 3 too");
    assert!(err.contains("behind the working tree"), "{err}");

    let (code, list, _, _) = e.vex(&root, &["search", "helper", "run_signer", "--json"]);
    assert_eq!(code, Some(3), "a list query on a stale index exits 3");
    assert_eq!(list["schema"], 1);
    assert!(list["fetch_targets"].as_array().is_some());
}

#[test]
fn search_without_an_index_exits_2() {
    let e = Env::new();
    let root = e.project();
    let (code, _, _, err) = e.vex(&root, &["search", "helper", "--json"]);
    assert_eq!(code, Some(2), "{err}");
}

#[test]
fn refused_list_exits_1_with_the_reason() {
    let e = Env::new();
    let root = e.project();
    let (code, _, _, err) = e.vex(&root, &["init", "--json"]);
    assert_eq!(code, Some(0), "{err}");
    let qs = ["a1", "b2", "c3", "d4", "e5", "f6", "g7"];
    let mut args = vec!["search"];
    args.extend(qs);
    args.push("--json");
    let (code, r, _, _) = e.vex(&root, &args);
    assert_eq!(code, Some(1));
    assert_eq!(r["error"], "invalid_request");
    assert_eq!(r["message"], "'queries' supports at most 6 items");
}
