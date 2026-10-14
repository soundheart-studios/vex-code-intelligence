//! Per-run isolation: concurrent runs and concurrent tests never share a path.

use vex_test_support::TestHome;

#[test]
fn a_home_is_namespaced_by_the_run_id() {
    let h = TestHome::with_run_id("run-42");
    assert!(h.vex_home().to_string_lossy().contains("run-42"));
    assert!(h.vex_home().is_dir());
}

#[test]
fn the_default_home_uses_pw_run_id_when_present() {
    let h = TestHome::new();
    let expected = std::env::var("PW_RUN_ID").unwrap_or_else(|_| "local".into());
    let safe: String = expected
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    assert!(
        h.vex_home().to_string_lossy().contains(&safe),
        "{:?} lacks {safe}",
        h.vex_home()
    );
}

#[test]
fn two_homes_in_one_run_never_share_a_path() {
    let a = TestHome::with_run_id("same");
    let b = TestHome::with_run_id("same");
    assert_ne!(a.vex_home(), b.vex_home());
    assert_ne!(a.socket(), b.socket());
}

#[test]
fn a_run_id_cannot_escape_the_temp_dir() {
    let h = TestHome::with_run_id("../../etc");
    assert!(h.vex_home().starts_with(std::env::temp_dir()));
}

#[test]
fn the_socket_path_fits_the_os_limit() {
    let h = TestHome::with_run_id(&"x".repeat(80));
    assert!(h.socket().as_os_str().len() <= 100, "{:?}", h.socket());
}

#[test]
fn dropping_a_home_removes_it() {
    let path = {
        let h = TestHome::with_run_id("drop");
        h.vex_home().to_path_buf()
    };
    assert!(!path.exists());
}
