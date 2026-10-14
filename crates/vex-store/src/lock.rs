//! The single-writer lock for in-process indexing (`write.lock` next to a
//! project's index). The daemon is the only writer when it runs; without
//! it, a writer takes this lock. Readers never take it: WAL lets them read
//! while a write is in progress.

use std::fs::File;
use std::path::{Path, PathBuf};

/// A held write lock, released on drop.
#[derive(Debug)]
pub struct WriteLock {
    _file: File,
    pub path: PathBuf,
}

#[derive(Debug)]
pub enum LockError {
    /// Another writer holds the lock.
    Busy(PathBuf),
    Io(std::io::Error),
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockError::Busy(p) => write!(
                f,
                "another vex process is writing this index ({})",
                p.display()
            ),
            LockError::Io(e) => write!(f, "cannot take the write lock: {e}"),
        }
    }
}

impl WriteLock {
    /// Take the lock without waiting; Busy if another writer holds it.
    pub fn try_acquire(path: &Path) -> Result<WriteLock, LockError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(LockError::Io)?;
        }
        let file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .map_err(LockError::Io)?;
        match file.try_lock() {
            Ok(()) => Ok(WriteLock {
                _file: file,
                path: path.to_path_buf(),
            }),
            Err(std::fs::TryLockError::WouldBlock) => Err(LockError::Busy(path.to_path_buf())),
            Err(std::fs::TryLockError::Error(e)) => Err(LockError::Io(e)),
        }
    }
}
