//! `vex sync --hook NAME` (what git hooks run) and the hook reports.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

/// How long a hook lets a sync run before git gets its prompt back.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

fn timeout() -> Duration {
    std::env::var("VEX_HOOK_TIMEOUT_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_TIMEOUT)
}

/// Sync quietly, through the daemon when it can take the work (it returns
/// at once), in-process otherwise; give up after the hook timeout. Always
/// exit 0: a hook must never fail the git command that ran it.
pub fn sync_as_hook(
    path: Option<PathBuf>,
    paths: Vec<String>,
    wait: bool,
    record_head: bool,
) -> i32 {
    let limit = timeout();
    let (done, finished) = mpsc::channel();
    let start = crate::out::resolve(path);
    std::thread::spawn(move || {
        let mut argv: Vec<String> = vec!["vex".into(), "--json".into(), "sync".into()];
        argv.push(start.display().to_string());
        for p in &paths {
            argv.extend(["--paths".into(), p.clone()]);
        }
        if wait {
            argv.push("--wait".into());
        }
        if record_head {
            argv.push("--record-head".into());
        }
        let routed = crate::route::connect(crate::route::policy()).and_then(|mut conn| {
            let _ = conn.set_timeout(Some(limit));
            let cwd = std::env::current_dir().ok()?;
            crate::route::call(
                &mut conn,
                serde_json::json!({"kind": "cli", "argv": argv, "cwd": cwd}),
            )
        });
        if routed.is_none()
            && let Some(home) = vex_app::home()
        {
            let req = vex_app::sync::Request {
                paths,
                wait,
                record_head,
            };
            let settings = vex_app::embed::Settings::from_env(&home);
            let _ = vex_app::sync::sync(&home, &start, &req, &settings);
        }
        let _ = done.send(());
    });
    let _ = finished.recv_timeout(limit);
    crate::exit::OK
}

pub fn print_report(r: &vex_app::hooks::HooksReport) {
    if let Some(m) = r.manager {
        outln!("hooks managed by {m} ({})", r.hooks_dir.display());
    }
    for h in &r.hooks {
        outln!("{}: {} ({})", h.name, h.action, h.path.display());
    }
    if let Some(n) = &r.note {
        errln!("vex: {n}");
    }
}
