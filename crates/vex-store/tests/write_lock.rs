//! One writer at a time; readers are never blocked (WAL).

use vex_store::db::Index;
use vex_store::lock::{LockError, WriteLock};
use vex_test_support::TestHome;

#[test]
fn a_second_writer_is_refused_while_the_lock_is_held() {
    let h = TestHome::new();
    let path = h.vex_home().join("p/write.lock");
    let held = WriteLock::try_acquire(&path).unwrap();
    assert!(matches!(
        WriteLock::try_acquire(&path),
        Err(LockError::Busy(_))
    ));
    drop(held);
    assert!(WriteLock::try_acquire(&path).is_ok(), "released on drop");
}

#[test]
fn readers_proceed_during_a_write() {
    let h = TestHome::new();
    let db = h.vex_home().join("p/index.db");
    let writer = Index::open(&db).unwrap();
    writer.conn.execute("INSERT INTO files (path, hash, language, indexed_at) VALUES ('a.py', 'h1', 'python', 1.0)", []).unwrap();
    let _lock = WriteLock::try_acquire(&h.vex_home().join("p/write.lock")).unwrap();
    writer.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    writer.conn.execute("INSERT INTO files (path, hash, language, indexed_at) VALUES ('b.py', 'h2', 'python', 2.0)", []).unwrap();
    // A reader on its own connection sees the committed state, unblocked.
    let reader = Index::open(&db).unwrap();
    let n: i64 = reader
        .conn
        .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
    writer.conn.execute_batch("COMMIT").unwrap();
    let n: i64 = reader
        .conn
        .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 2);
}

#[test]
fn the_busy_error_names_the_lock() {
    let h = TestHome::new();
    let path = h.vex_home().join("p/write.lock");
    let _held = WriteLock::try_acquire(&path).unwrap();
    let err = WriteLock::try_acquire(&path).unwrap_err().to_string();
    assert!(
        err.contains("write.lock") && err.contains("another vex process"),
        "{err}"
    );
}
