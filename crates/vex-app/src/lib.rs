//! One function per command. The only API: the CLI, daemon and MCP adapters
//! call it and hold no logic.

pub mod agent;
pub mod changes;
pub mod doctor;
pub mod embed;
pub mod expand;
pub mod freshness;
pub mod graph;
pub mod grep;
pub mod hooks;
pub mod index;
pub mod map;
pub mod models;
pub mod parse;
pub mod preflight;
pub mod progress;
pub mod project;
pub mod pyjson;
mod pyprintable;
pub mod recovery;
pub mod search;
pub mod selector;
pub mod symbols;
pub mod sync;
pub mod tools;
pub mod version;
pub mod walk;
pub mod windows;

/// The `--json` contract version every command result carries.
pub const SCHEMA: u32 = 1;

/// VEX_HOME for this process (`VEX_HOME`, else `$HOME/.vex`).
pub fn home() -> Option<std::path::PathBuf> {
    vex_store::home::from_env()
}
