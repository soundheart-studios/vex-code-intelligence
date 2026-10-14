//! Core types shared by every vex crate: chunks, symbols, Parsed, ids and the
//! canonical JSON the golden outputs are written in. Pure: no I/O.

pub mod canonical;
pub mod ids;
pub mod model;

pub use model::{CAP_CHUNK_CHARS, Chunk, Parsed, Symbol};

/// The `--json` contract version every command result carries.
pub const SCHEMA_VERSION: u32 = 1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_version_is_one() {
        assert_eq!(SCHEMA_VERSION, 1);
    }
}
