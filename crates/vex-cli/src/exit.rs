//! Exit codes are an API. Hook mode always exits OK.

pub const OK: i32 = 0;
pub const USAGE: i32 = 1;
pub const NO_INDEX: i32 = 2;
pub const STALE: i32 = 3;
pub const RUNTIME: i32 = 4;
