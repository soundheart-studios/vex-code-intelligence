//! The walker against Cortex's walk() on the declarative fixture tree.

mod common;

use common::{build_tree, fixture};
use serde_json::{Value, json};
use vex_app::walk::{fnmatch, walk};
use vex_test_support::TestHome;

fn rel_list(home: &TestHome) -> (Vec<String>, Vec<String>, std::path::PathBuf) {
    let root = home.vex_home().join("tree");
    build_tree(&root);
    let w = walk(&root);
    let files = w
        .files
        .iter()
        .map(|p| {
            p.strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    (files, w.skipped_caches, root)
}

#[test]
fn walk_returns_cortexs_files_in_cortexs_order() {
    let h = TestHome::new();
    let (files, _, _) = rel_list(&h);
    assert_eq!(json!(files), fixture("expected.json")["walk"]);
}

#[test]
fn skipped_caches_match_cortex() {
    let h = TestHome::new();
    let (_, skipped, _) = rel_list(&h);
    assert_eq!(json!(skipped), fixture("expected.json")["skipped_caches"]);
}

#[test]
fn symlinks_are_never_followed() {
    let h = TestHome::new();
    let (files, _, root) = rel_list(&h);
    // The fixture links outside the root, loops back to its parent and
    // points at a file; none of them may appear or be descended into.
    for f in &files {
        assert!(
            !f.starts_with("outside_link") && !f.starts_with("src/loop") && f != "file_link.py",
            "{f}"
        );
    }
    assert!(
        std::fs::symlink_metadata(root.join("src/loop"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn symlinked_files_and_directories_are_not_returned() {
    // No loops here, so a walker that followed links would fail at once
    // (returning extra files) instead of recursing to the path-length limit.
    let h = TestHome::new();
    let root = h.vex_home().join("root");
    let sibling = h.vex_home().join("sibling");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&sibling).unwrap();
    std::fs::write(root.join("real.py"), "r = 1\n").unwrap();
    std::fs::write(sibling.join("out.py"), "o = 1\n").unwrap();
    std::os::unix::fs::symlink("../sibling", root.join("dirlink")).unwrap();
    std::os::unix::fs::symlink("real.py", root.join("filelink.py")).unwrap();
    let files: Vec<String> = walk(&root)
        .files
        .iter()
        .map(|p| {
            p.strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(files, ["real.py"]);
}

#[test]
fn a_vexignore_glob_excludes_like_cortexignore() {
    let h = TestHome::new();
    let root = h.vex_home().join("tree");
    build_tree(&root);
    std::fs::write(root.join(".vexignore"), "lib\nbin/tool\n").unwrap();
    let files: Vec<String> = walk(&root)
        .files
        .iter()
        .map(|p| {
            p.strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert!(
        !files
            .iter()
            .any(|f| f.starts_with("lib/") || f == "bin/tool"),
        "{files:?}"
    );
    assert!(files.contains(&"main.py".to_string()));
}

#[test]
fn fnmatch_follows_pythons_rules() {
    // Expected values: Python's fnmatch.fnmatch on POSIX.
    let cases: Value = json!([
        ["a/b/c.py", "*.py", true],
        ["x.py", "?.py", true],
        ["xy.py", "?.py", false],
        ["gen_b.py", "gen_[a-c].py", true],
        ["gen_d.py", "gen_[a-c].py", false],
        ["xonly.ts", "[!x]only.ts", false],
        ["conly.ts", "[!x]only.ts", true],
        ["a[b", "a[b", true],
        ["-", "[-a]", true],
        ["b", "[!-a]", true],
        ["a", "[!-a]", false],
        ["c", "[z-a]", false],
        ["c", "[!z-a]", true],
        ["-", "[a-]", true],
        ["b", "[a-c-e]", true],
        ["-", "[a-c-e]", true],
        ["d", "[a-c-e]", false],
        ["e", "[a-c-e]", true],
        ["Main.PY", "*.py", false],
        ["", "*", true],
        ["abc", "a**c", true],
        ["a\\b", "a\\b", true],
        ["]", "[]]", true],
        ["a", "[]a]", true],
        ["!", "[!]", false]
    ]);
    for c in cases.as_array().unwrap() {
        let (name, pat, want) = (
            c[0].as_str().unwrap(),
            c[1].as_str().unwrap(),
            c[2].as_bool().unwrap(),
        );
        assert_eq!(fnmatch(name, pat), want, "fnmatch({name:?}, {pat:?})");
    }
}

#[test]
fn agent_folders_and_tool_caches_are_not_indexed_but_project_dotfolders_are() {
    let h = TestHome::new();
    let root = h.vex_home().join("dots");
    for rel in [
        "src/app.py",
        ".claude/skills/vex-code-context/SKILL.md",
        ".codex/config.toml",
        ".agents/skills/x/SKILL.md",
        ".cursor/rules/style.md",
        ".specstory/history/chat.md",
        ".aider.chat.history.md",
        ".aider.tags.cache.v4/cache.py",
        ".gradle/caches/gen.java",
        ".turbo/cache.js",
        ".svelte-kit/output/app.js",
        "web/.next/server.js",
        "web/.yarn/releases/yarn.cjs",
        ".github/workflows/ci.yml",
        ".github/actions/setup/index.js",
        ".storybook/main.js",
        ".husky/pre-commit",
    ] {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "#!/bin/sh\nx = 1\n").unwrap();
    }
    let files: Vec<String> = walk(&root)
        .files
        .iter()
        .map(|p| {
            p.strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    for kept in [
        "src/app.py",
        ".github/actions/setup/index.js",
        ".storybook/main.js",
        ".husky/pre-commit",
    ] {
        assert!(
            files.iter().any(|f| f == kept),
            "{kept} not indexed: {files:?}"
        );
    }
    for f in &files {
        assert!(
            [
                ".claude",
                ".codex",
                ".agents",
                ".cursor",
                ".specstory",
                ".aider",
                ".gradle",
                ".turbo",
                ".svelte-kit",
                ".next",
                ".yarn"
            ]
            .iter()
            .all(|d| !f.contains(d)),
            "{f} indexed"
        );
    }
}
