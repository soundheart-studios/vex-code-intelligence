//! Test helpers shared by every crate.
//!
//! Every resource a test creates is namespaced by `$PW_RUN_ID` (present in
//! every Planwright run), the process id and a per-process counter, so two
//! concurrent runs, or two tests in one run, never share a path. A fixed name
//! is a collision waiting for the second agent.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static SEQ: AtomicUsize = AtomicUsize::new(0);

/// macOS caps a Unix socket path at 104 bytes including the terminator.
const MAX_SOCKET_PATH: usize = 100;

/// A private VEX_HOME for one test, removed when dropped.
#[derive(Debug)]
pub struct TestHome {
    root: PathBuf,
    socket: PathBuf,
}

impl TestHome {
    /// A fresh home namespaced by `$PW_RUN_ID` (or `local` outside a run).
    pub fn new() -> TestHome {
        let run = std::env::var("PW_RUN_ID").unwrap_or_else(|_| "local".to_string());
        TestHome::with_run_id(&run)
    }

    /// A fresh home namespaced by an explicit run id.
    pub fn with_run_id(run_id: &str) -> TestHome {
        let safe: String = run_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let name = format!(
            "vex-test-{safe}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        );
        let root = std::env::temp_dir().join(&name);
        std::fs::create_dir_all(&root).expect("create test VEX_HOME");
        let mut socket = root.join("vex.sock");
        if socket.as_os_str().len() > MAX_SOCKET_PATH {
            // Deep temp dirs (macOS /var/folders/...) overflow the limit; a
            // short /tmp name derived from the unique root stays unique.
            let mut h = DefaultHasher::new();
            root.hash(&mut h);
            socket = PathBuf::from(format!("/tmp/vex-{:016x}.sock", h.finish()));
        }
        TestHome { root, socket }
    }

    /// The directory to use as VEX_HOME.
    pub fn vex_home(&self) -> &Path {
        &self.root
    }

    /// The daemon socket path for this home, always within the OS limit.
    pub fn socket(&self) -> &Path {
        &self.socket
    }
}

impl Default for TestHome {
    fn default() -> Self {
        TestHome::new()
    }
}

impl Drop for TestHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
        let _ = std::fs::remove_file(&self.socket);
    }
}
