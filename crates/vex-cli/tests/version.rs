//! `vex --version`: human and `--json` forms.

use serde_json::Value;
use std::process::Command;
use vex_test_support::TestHome;

fn vex(home: &TestHome, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_vex"))
        .args(args)
        .env("VEX_EMBED_BACKEND", "none")
        .env("VEX_DAEMON_AUTOSTART", "never")
        .env("VEX_HOME", home.vex_home())
        .output()
        .unwrap()
}

#[test]
fn json_version_carries_schema_version_and_grammars() {
    let h = TestHome::new();
    let out = vex(&h, &["--version", "--json"]);
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["schema"], 1);
    assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
    assert!(v["grammars"].is_array());
    assert!(v["build"]["os"].is_string());
}

#[test]
fn json_output_has_no_colour_codes() {
    let h = TestHome::new();
    let out = vex(&h, &["--version", "--json"]);
    assert!(!out.stdout.contains(&0x1b), "ANSI escape in --json output");
}

#[test]
fn human_version_names_the_binary() {
    let h = TestHome::new();
    let out = vex(&h, &["-V"]);
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(
        text.starts_with(&format!("vex {}", env!("CARGO_PKG_VERSION"))),
        "{text}"
    );
}

#[test]
fn an_unknown_command_is_a_usage_error() {
    let h = TestHome::new();
    assert_eq!(vex(&h, &["no-such-command"]).status.code(), Some(1));
}
