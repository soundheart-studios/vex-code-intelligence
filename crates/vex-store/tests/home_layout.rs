//! VEX_HOME resolution and its owner-only layout.

use std::ffi::OsStr;
use std::path::Path;
use vex_store::home;
use vex_test_support::TestHome;

#[test]
fn the_override_wins_over_the_user_home() {
    let got = home::resolve(
        Some(OsStr::new("/custom/vex")),
        Some(Path::new("/Users/me")),
    );
    assert_eq!(got.unwrap(), Path::new("/custom/vex"));
}

#[test]
fn without_an_override_it_is_dot_vex_in_the_user_home() {
    assert_eq!(
        home::resolve(None, Some(Path::new("/Users/me"))).unwrap(),
        Path::new("/Users/me/.vex")
    );
    assert_eq!(
        home::resolve(Some(OsStr::new("")), Some(Path::new("/h"))).unwrap(),
        Path::new("/h/.vex")
    );
}

#[test]
fn first_run_creates_the_home_and_subdirs_owner_only() {
    let t = TestHome::new();
    let root = t.vex_home().join("fresh");
    home::ensure(&root).unwrap();
    assert_eq!(home::mode(&root), Some(0o700));
    for sub in home::SUBDIRS {
        assert_eq!(home::mode(&root.join(sub)), Some(0o700), "{sub}");
    }
}

#[test]
fn ensure_is_idempotent() {
    let t = TestHome::new();
    let root = t.vex_home().join("again");
    home::ensure(&root).unwrap();
    home::ensure(&root).unwrap();
    assert!(root.join("projects").is_dir());
}

#[cfg(unix)]
#[test]
fn an_existing_home_is_not_re_permissioned() {
    use std::os::unix::fs::PermissionsExt;
    let t = TestHome::new();
    let root = t.vex_home().join("loose");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
    home::ensure(&root).unwrap();
    // doctor reports it; ensure never changes what the user set.
    assert_eq!(home::mode(&root), Some(0o755));
}
