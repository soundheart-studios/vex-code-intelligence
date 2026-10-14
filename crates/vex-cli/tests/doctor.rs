//! `vex doctor`: creates VEX_HOME on first run, reports its permissions.

use serde_json::Value;
use std::process::Command;
use vex_test_support::TestHome;

fn doctor(vex_home: &std::path::Path) -> (Option<i32>, Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_vex"))
        .args(["doctor", "--json"])
        .env("VEX_EMBED_BACKEND", "none")
        .env("VEX_DAEMON_AUTOSTART", "never")
        .env("VEX_HOME", vex_home)
        .output()
        .unwrap();
    (
        out.status.code(),
        serde_json::from_slice(&out.stdout).unwrap(),
    )
}

fn check<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .expect(name)
}

#[test]
fn doctor_creates_and_reports_the_home() {
    let h = TestHome::new();
    let root = h.vex_home().join("home");
    let (code, r) = doctor(&root);
    assert_eq!(code, Some(0), "{r}");
    assert_eq!(r["schema"], 1);
    assert_eq!(r["vex_home"], root.to_str().unwrap());
    assert_eq!(check(&r, "vex_home_permissions")["ok"], true);
    assert!(root.join("projects").is_dir());
}

#[cfg(unix)]
#[test]
fn loose_permissions_fail_with_the_chmod_fix() {
    use std::os::unix::fs::PermissionsExt;
    let h = TestHome::new();
    let root = h.vex_home().join("loose");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (code, r) = doctor(&root);
    assert_eq!(code, Some(4));
    let c = check(&r, "vex_home_permissions");
    assert_eq!(c["ok"], false);
    assert_eq!(c["fix"], format!("chmod 700 {}", root.display()));
}
