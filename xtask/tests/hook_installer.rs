//! The blocking pre-commit hook installer: names every guard, is idempotent,
//! never touches what is outside its markers, and uninstalls exactly.

use std::path::{Path, PathBuf};
use std::process::Command;
use vex_test_support::TestHome;

fn installer() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("scripts/install-git-hooks.sh")
}

fn repo(home: &TestHome) -> PathBuf {
    let r = home.vex_home().join("repo");
    std::fs::create_dir_all(&r).unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&r)
            .status()
            .unwrap()
            .success()
    );
    r
}

fn run(repo: &Path, extra: &[&str]) {
    let st = Command::new("sh")
        .arg(installer())
        .args(extra)
        .arg(repo)
        .status()
        .unwrap();
    assert!(st.success());
}

fn hook(repo: &Path) -> String {
    std::fs::read_to_string(repo.join(".git/hooks/pre-commit")).unwrap_or_default()
}

#[test]
fn the_installer_exists_and_is_executable() {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(installer())
        .expect("installer missing")
        .permissions()
        .mode();
    assert!(mode & 0o111 != 0, "installer is not executable");
}

#[test]
fn the_installer_names_every_guard() {
    let script = std::fs::read_to_string(installer()).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["guards", "--list"])
        .output()
        .unwrap();
    let missing: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split('\t').next())
        .filter(|g| !script.contains(g))
        .map(String::from)
        .collect();
    assert!(
        missing.is_empty(),
        "guards the installer does not name: {missing:?}"
    );
    assert!(
        script.contains("cargo xtask guards"),
        "the hook must run the guards"
    );
}

#[test]
fn install_creates_an_executable_hook_running_the_guards() {
    use std::os::unix::fs::PermissionsExt;
    let h = TestHome::new();
    let r = repo(&h);
    run(&r, &[]);
    let text = hook(&r);
    assert!(
        text.starts_with("#!/bin/sh\n") && text.contains("cargo xtask guards"),
        "{text}"
    );
    let mode = std::fs::metadata(r.join(".git/hooks/pre-commit"))
        .unwrap()
        .permissions()
        .mode();
    assert!(mode & 0o111 != 0);
}

#[test]
fn install_is_idempotent() {
    let h = TestHome::new();
    let r = repo(&h);
    run(&r, &[]);
    let once = hook(&r);
    run(&r, &[]);
    assert_eq!(hook(&r), once);
    assert_eq!(once.matches("# >>> vex guards >>>").count(), 1);
}

#[test]
fn an_existing_hook_is_preserved_and_uninstall_restores_it_exactly() {
    let h = TestHome::new();
    let r = repo(&h);
    let theirs = "#!/bin/sh\necho 'someone else's check'\n";
    std::fs::create_dir_all(r.join(".git/hooks")).unwrap();
    std::fs::write(r.join(".git/hooks/pre-commit"), theirs).unwrap();
    run(&r, &[]);
    assert!(
        hook(&r).starts_with(theirs),
        "the existing hook was clobbered"
    );
    run(&r, &["--uninstall"]);
    assert_eq!(hook(&r), theirs);
}

#[test]
fn core_hooks_path_is_honoured() {
    let h = TestHome::new();
    let r = repo(&h);
    assert!(
        Command::new("git")
            .args(["config", "core.hooksPath", ".githooks"])
            .current_dir(&r)
            .status()
            .unwrap()
            .success()
    );
    run(&r, &[]);
    assert!(
        std::fs::read_to_string(r.join(".githooks/pre-commit"))
            .unwrap()
            .contains("cargo xtask guards")
    );
    assert!(!r.join(".git/hooks/pre-commit").exists());
}
