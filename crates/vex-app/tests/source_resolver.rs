//! The filesystem resolver behind Bash `source` imports during indexing:
//! Cortex's `(path.parent / raw).resolve().relative_to(root.resolve())`,
//! kept only when the file exists.

use vex_app::parse::source_resolver;
use vex_test_support::TestHome;

#[test]
fn sourced_paths_resolve_under_the_root_only_when_they_exist() {
    let h = TestHome::new();
    let root = h.vex_home().join("proj");
    std::fs::create_dir_all(root.join("lib")).unwrap();
    std::fs::write(root.join("lib/util.sh"), "").unwrap();
    std::fs::write(root.join("top.sh"), "").unwrap();
    std::fs::write(h.vex_home().join("outside.sh"), "").unwrap();
    let resolve = source_resolver(&root);

    assert_eq!(resolve("", "./lib/util.sh").as_deref(), Some("lib/util.sh"));
    assert_eq!(resolve("lib", "./util.sh").as_deref(), Some("lib/util.sh"));
    assert_eq!(resolve("lib", "../top.sh").as_deref(), Some("top.sh"));
    assert_eq!(
        resolve("lib", "./missing.sh"),
        None,
        "a missing file is no import"
    );
    assert_eq!(
        resolve("", "../outside.sh"),
        None,
        "outside the root is no import"
    );
    // An absolute path counts when it lands inside the root.
    let inside = root.join("top.sh").canonicalize().unwrap();
    assert_eq!(
        resolve("lib", inside.to_str().unwrap()).as_deref(),
        Some("top.sh")
    );
    assert_eq!(resolve("", "/etc/hosts"), None);
    // The root itself is `.`, as Python's relative_to prints it.
    assert_eq!(resolve("lib", "..").as_deref(), Some("."));
}

#[cfg(unix)]
#[test]
fn a_symlink_resolves_to_its_target_like_path_resolve() {
    let h = TestHome::new();
    let root = h.vex_home().join("proj");
    std::fs::create_dir_all(root.join("real")).unwrap();
    std::fs::write(root.join("real/a.sh"), "").unwrap();
    std::os::unix::fs::symlink(root.join("real/a.sh"), root.join("link.sh")).unwrap();
    let resolve = source_resolver(&root);
    assert_eq!(resolve("", "./link.sh").as_deref(), Some("real/a.sh"));
}
