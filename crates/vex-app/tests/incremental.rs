//! Incremental indexing on a self-contained project: unchanged files are
//! never re-parsed, an edit re-parses only its file, removed files leave no
//! rows behind.

use rusqlite::Connection;
use vex_app::index::run;
use vex_store::db::Index;
use vex_test_support::TestHome;

fn project(h: &TestHome) -> std::path::PathBuf {
    let root = h.vex_home().join("proj");
    std::fs::create_dir_all(root.join("pkg")).unwrap();
    std::fs::write(
        root.join("pkg/a.py"),
        "from pkg.b import helper\n\ndef run():\n    return helper(1)\n",
    )
    .unwrap();
    std::fs::write(root.join("pkg/b.py"), "def helper(x):\n    return x + 1\n\n\nclass Thing:\n    def go(self):\n        return helper(2)\n").unwrap();
    std::fs::write(
        root.join("pkg/c.py"),
        "import os\n\ndef cwd():\n    return os.getcwd()\n",
    )
    .unwrap();
    root
}

fn count(c: &Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}

fn rowids(c: &Connection, file: &str) -> Vec<i64> {
    let mut stmt = c
        .prepare("SELECT rowid FROM chunks WHERE file_path = ?1 ORDER BY rowid")
        .unwrap();
    stmt.query_map([file], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn an_unchanged_tree_re_parses_nothing() {
    let h = TestHome::new();
    let root = project(&h);
    let idx = Index::open(&h.vex_home().join("i.db")).unwrap();
    let first = run(&idx, &root, false).unwrap();
    assert_eq!((first.added, first.modified, first.removed), (3, 0, 0));
    assert!(first.chunks_ingested > 0 && first.call_edges > 0);
    let before = rowids(&idx.conn, "pkg/a.py");
    let second = run(&idx, &root, false).unwrap();
    assert_eq!(
        (
            second.added,
            second.modified,
            second.removed,
            second.chunks_ingested
        ),
        (0, 0, 0, 0)
    );
    assert_eq!(
        second.index_version, first.index_version,
        "nothing changed, so the version holds"
    );
    assert_eq!(rowids(&idx.conn, "pkg/a.py"), before);
}

#[test]
fn an_edit_re_parses_only_that_file() {
    let h = TestHome::new();
    let root = project(&h);
    let idx = Index::open(&h.vex_home().join("i.db")).unwrap();
    let first = run(&idx, &root, false).unwrap();
    let untouched = rowids(&idx.conn, "pkg/c.py");
    std::fs::write(
        root.join("pkg/b.py"),
        "def helper(x):\n    return x + 2\n\ndef extra():\n    return helper(3)\n",
    )
    .unwrap();
    let r = run(&idx, &root, false).unwrap();
    assert_eq!((r.added, r.modified, r.removed), (0, 1, 0));
    assert_ne!(r.index_version, first.index_version);
    assert_eq!(
        rowids(&idx.conn, "pkg/c.py"),
        untouched,
        "other files keep their rows"
    );
    assert_eq!(
        count(
            &idx.conn,
            "SELECT COUNT(*) FROM symbols WHERE qualified_name = 'pkg.b.py.extra'"
        ),
        1
    );
    assert_eq!(
        count(
            &idx.conn,
            "SELECT COUNT(*) FROM symbols WHERE qualified_name = 'pkg.b.py.Thing'"
        ),
        0,
        "the old class is gone"
    );
}

#[test]
fn a_removed_file_leaves_no_rows_behind() {
    let h = TestHome::new();
    let root = project(&h);
    let idx = Index::open(&h.vex_home().join("i.db")).unwrap();
    run(&idx, &root, false).unwrap();
    std::fs::remove_file(root.join("pkg/b.py")).unwrap();
    let r = run(&idx, &root, false).unwrap();
    assert_eq!(r.removed, 1);
    for table in [
        "chunks",
        "chunks_fts",
        "symbols",
        "symbol_sites",
        "refs",
        "import_bindings",
        "calls",
        "member_types",
    ] {
        let n = count(
            &idx.conn,
            &format!("SELECT COUNT(*) FROM {table} WHERE file_path = 'pkg/b.py'"),
        );
        assert_eq!(n, 0, "{table}");
    }
    assert_eq!(
        count(
            &idx.conn,
            "SELECT COUNT(*) FROM files WHERE path = 'pkg/b.py'"
        ),
        0
    );
    assert_eq!(
        count(
            &idx.conn,
            "SELECT COUNT(*) FROM edges WHERE src_file = 'pkg/b.py' OR dst_file = 'pkg/b.py'"
        ),
        0
    );
}

#[test]
fn force_rebuilds_every_file() {
    let h = TestHome::new();
    let root = project(&h);
    let idx = Index::open(&h.vex_home().join("i.db")).unwrap();
    run(&idx, &root, false).unwrap();
    let r = run(&idx, &root, true).unwrap();
    assert_eq!((r.added, r.modified, r.removed), (3, 0, 0));
}
