//! `vex --version`: version, build and compiled-in grammars.

use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct Build {
    pub profile: &'static str,
    pub os: &'static str,
    pub arch: &'static str,
}

#[derive(Debug, Serialize)]
pub struct VersionInfo {
    pub schema: u32,
    pub version: &'static str,
    pub build: Build,
    pub grammars: Vec<&'static str>,
    /// What this binary greets the daemon with (see `handshake`).
    pub daemon_handshake: String,
}

/// The daemon handshake: this vex's version plus a fingerprint of the
/// settings a daemon inherits from the client that spawns it. A client whose
/// version or settings differ makes a running daemon drain and exit, so it
/// can start one that answers as it would itself.
pub fn handshake() -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for key in [
        "VEX_EMBED_BACKEND",
        "VEX_MODEL_BASE_URL",
        "VEX_EMBED_IDLE_SECS",
        "VEX_EMBED_WORKERS",
        "VEX_DAEMON_IDLE_EXIT_SECS",
    ] {
        h.update(key.as_bytes());
        h.update(b"=");
        h.update(std::env::var(key).unwrap_or_default().as_bytes());
        h.update(b"\n");
    }
    let digest = h.finalize();
    let tag: String = digest[..6].iter().map(|b| format!("{b:02x}")).collect();
    format!("{}+{tag}", env!("CARGO_PKG_VERSION"))
}

pub fn version() -> VersionInfo {
    VersionInfo {
        schema: crate::SCHEMA,
        version: env!("CARGO_PKG_VERSION"),
        build: Build {
            profile: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            },
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
        },
        grammars: vex_extract::compiled_grammars(),
        daemon_handshake: handshake(),
    }
}
