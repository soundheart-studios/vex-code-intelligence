//! Where a command's output goes and which directory it runs in.
//!
//! A command run directly prints to the process's stdout and stderr and
//! resolves paths against the process's cwd. The daemon runs the very same
//! code for a client: it points this thread's sinks at buffers and its cwd
//! at the client's, so the bytes it returns are the bytes the client would
//! have printed itself.

use std::cell::RefCell;
use std::io::Write;
use std::path::{Path, PathBuf};

thread_local! {
    static CAPTURE: RefCell<Option<(Vec<u8>, Vec<u8>)>> = const { RefCell::new(None) };
    static CWD: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

pub fn write_out(bytes: &[u8]) {
    CAPTURE.with(|c| match c.borrow_mut().as_mut() {
        Some((out, _)) => out.extend_from_slice(bytes),
        None => {
            let mut s = std::io::stdout().lock();
            let _ = s.write_all(bytes);
            let _ = s.flush();
        }
    });
}

pub fn write_err(bytes: &[u8]) {
    CAPTURE.with(|c| match c.borrow_mut().as_mut() {
        Some((_, err)) => err.extend_from_slice(bytes),
        None => {
            let _ = std::io::stderr().lock().write_all(bytes);
        }
    });
}

/// Run `f` with this thread's output captured and paths resolved against
/// `cwd`; returns its result and the captured stdout and stderr.
pub fn captured<T>(cwd: &Path, f: impl FnOnce() -> T) -> (T, Vec<u8>, Vec<u8>) {
    CAPTURE.with(|c| *c.borrow_mut() = Some((Vec::new(), Vec::new())));
    CWD.with(|c| *c.borrow_mut() = Some(cwd.to_path_buf()));
    let result = f();
    CWD.with(|c| *c.borrow_mut() = None);
    let (out, err) = CAPTURE.with(|c| c.borrow_mut().take()).unwrap_or_default();
    (result, out, err)
}

/// A path argument as a command uses it: as typed (the current directory
/// when absent) in-process, against the client's directory under the
/// daemon. In-process it is never rewritten, so messages echo the user's
/// own spelling.
pub fn resolve(path: Option<PathBuf>) -> PathBuf {
    let p = path.unwrap_or_else(|| PathBuf::from("."));
    match CWD.with(|c| c.borrow().clone()) {
        Some(base) if p.is_relative() => base.join(p),
        _ => p,
    }
}

macro_rules! outln {
    () => { $crate::out::write_out(b"\n") };
    ($($arg:tt)*) => {{
        let mut s = format!($($arg)*);
        s.push('\n');
        $crate::out::write_out(s.as_bytes());
    }};
}

macro_rules! errln {
    ($($arg:tt)*) => {{
        let mut s = format!($($arg)*);
        s.push('\n');
        $crate::out::write_err(s.as_bytes());
    }};
}
