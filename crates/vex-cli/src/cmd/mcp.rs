//! `vex mcp`: serve the query tools over MCP stdio from the current
//! directory's project (a tool's `project` argument can name another).
//! Each tool call goes to the daemon when it can answer (per the autostart
//! policy), and is answered in this process otherwise.

use serde_json::json;
use std::sync::{Arc, Mutex};
use vex_daemon::client::Conn;

pub fn run() -> i32 {
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(e) => {
            eprintln!("vex: cannot read the current directory: {e}");
            return crate::exit::RUNTIME;
        }
    };
    let policy = crate::route::policy();
    let conn: Mutex<Option<Conn>> = Mutex::new(None);
    let route_cwd = cwd.clone();
    let route: vex_mcp::transport::Route = Arc::new(move |name, args| {
        let mut slot = conn.lock().unwrap_or_else(|p| p.into_inner());
        let payload = json!({"kind": "mcp", "name": name, "args": args, "cwd": route_cwd});
        // One retry on a fresh connection: the daemon may have restarted.
        for _ in 0..2 {
            if slot.is_none() {
                *slot = crate::route::connect(policy);
            }
            let c = slot.as_mut()?;
            if let Some(reply) = crate::route::call(c, payload.clone()) {
                return Some(vex_mcp::Called {
                    text: reply["text"].as_str()?.to_string(),
                    is_error: reply["is_error"].as_bool()?,
                });
            }
            *slot = None;
        }
        None
    });
    match vex_mcp::serve_stdio_routed(vex_app::home(), cwd, Some(route)) {
        Ok(()) => crate::exit::OK,
        Err(e) => {
            eprintln!("vex: mcp: {e}");
            crate::exit::RUNTIME
        }
    }
}
