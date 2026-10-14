//! Stable project ids: a moved repository re-attaches by its remote; a
//! second clone of the same remote is a separate project.

use vex_store::registry::{Attached, Registry, index_path, normalize_remote};
use vex_test_support::TestHome;

fn dir(h: &TestHome, name: &str) -> std::path::PathBuf {
    let p = h.vex_home().join(name);
    std::fs::create_dir_all(&p).unwrap();
    p
}

#[test]
fn the_same_root_is_the_same_project() {
    let h = TestHome::new();
    let root = dir(&h, "repo");
    let mut r = Registry::load(h.vex_home());
    let first = r.attach(&root, Some("git@github.com:o/r.git"));
    assert!(matches!(first, Attached::New(_)));
    assert_eq!(
        r.attach(&root, None),
        Attached::Existing(first.id().to_string())
    );
}

#[test]
fn a_moved_repo_with_the_same_remote_reattaches_to_its_id() {
    let h = TestHome::new();
    let old = dir(&h, "old-place");
    let mut r = Registry::load(h.vex_home());
    let id = r
        .attach(&old, Some("https://github.com/o/r"))
        .id()
        .to_string();
    r.save().unwrap();
    std::fs::remove_dir_all(&old).unwrap();
    let new = dir(&h, "new-place");
    let mut r = Registry::load(h.vex_home());
    assert_eq!(
        r.attach(&new, Some("git@github.com:o/r.git")),
        Attached::Reattached {
            id: id.clone(),
            from: old
        }
    );
    assert_eq!(r.get(&id).unwrap().root, new);
    assert_eq!(r.get(&id).unwrap().name, "new-place");
}

#[test]
fn a_second_clone_of_the_same_remote_is_a_new_project() {
    let h = TestHome::new();
    let a = dir(&h, "clone-a");
    let b = dir(&h, "clone-b");
    let mut r = Registry::load(h.vex_home());
    let ia = r
        .attach(&a, Some("https://github.com/o/r.git"))
        .id()
        .to_string();
    let ib = r.attach(&b, Some("https://github.com/o/r.git"));
    assert!(matches!(ib, Attached::New(_)));
    assert_ne!(ib.id(), ia);
}

#[test]
fn a_moved_repo_without_a_remote_cannot_be_recognised() {
    let h = TestHome::new();
    let old = dir(&h, "local-only");
    let mut r = Registry::load(h.vex_home());
    let id = r.attach(&old, None).id().to_string();
    std::fs::remove_dir_all(&old).unwrap();
    let new = dir(&h, "local-moved");
    let again = r.attach(&new, None);
    assert!(matches!(again, Attached::New(_)));
    assert_ne!(again.id(), id);
}

#[test]
fn remote_spellings_of_one_repository_normalize_alike() {
    let forms = [
        "git@github.com:Owner/Repo.git",
        "https://github.com/Owner/Repo",
        "https://github.com/Owner/Repo.git/",
        "ssh://git@github.com/Owner/Repo.git",
        "https://GitHub.com/Owner/Repo",
    ];
    let first = normalize_remote(forms[0]);
    assert_eq!(first, "github.com/Owner/Repo");
    for f in forms {
        assert_eq!(normalize_remote(f), first, "{f}");
    }
    assert_ne!(normalize_remote("https://github.com/owner/other"), first);
    assert_eq!(
        normalize_remote("ssh://git@host:2222/o/r.git"),
        "host:2222/o/r"
    );
}

#[test]
fn the_registry_persists_owner_only() {
    let h = TestHome::new();
    let root = dir(&h, "persisted");
    let mut r = Registry::load(h.vex_home());
    let id = r.attach(&root, Some("https://x/y")).id().to_string();
    r.save().unwrap();
    let reloaded = Registry::load(h.vex_home());
    assert_eq!(reloaded.get(&id).unwrap().root, root);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(Registry::path_in(h.vex_home()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[test]
fn each_project_has_its_own_index_under_projects() {
    let h = TestHome::new();
    assert_eq!(
        index_path(h.vex_home(), "abc"),
        h.vex_home().join("projects/abc/index.db")
    );
}

#[test]
fn forget_removes_the_entry() {
    let h = TestHome::new();
    let root = dir(&h, "gone");
    let mut r = Registry::load(h.vex_home());
    let id = r.attach(&root, None).id().to_string();
    assert!(r.forget(&id).is_some());
    assert!(r.get(&id).is_none());
}
