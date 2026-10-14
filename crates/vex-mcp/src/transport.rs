//! Where a tool call is answered. By default in this process; the CLI can
//! install a route that sends calls to the daemon first, so `vex mcp`
//! shares the daemon's warm state, falling back here when it is away.

use crate::Called;
use rmcp::model::JsonObject;
use std::sync::Arc;

/// Answer a call elsewhere, or None to answer it in this process.
pub type Route = Arc<dyn Fn(&str, &JsonObject) -> Option<Called> + Send + Sync>;
