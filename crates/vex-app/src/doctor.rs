//! `vex doctor`: setup diagnosis. Every failing check names its fix.

use serde::Serialize;
use std::path::{Path, PathBuf};
use vex_store::home;

#[derive(Debug, Serialize)]
pub struct Check {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
    /// What to do about it; present only when the check fails.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub schema: u32,
    pub ok: bool,
    pub vex_home: Option<PathBuf>,
    pub checks: Vec<Check>,
}

/// Diagnose the setup rooted at `vex_home` (None: no home could be resolved).
/// Creates the home on first run, which is when doctor is usually reached for.
pub fn doctor(vex_home: Option<&Path>) -> Report {
    let mut checks = Vec::new();
    match vex_home {
        None => checks.push(Check {
            name: "vex_home",
            ok: false,
            detail: "neither VEX_HOME nor HOME is set".into(),
            fix: Some("set VEX_HOME to a private directory, e.g. export VEX_HOME=~/.vex".into()),
        }),
        Some(root) => {
            let created = home::ensure(root);
            checks.push(Check {
                name: "vex_home",
                ok: created.is_ok(),
                detail: match &created {
                    Ok(()) => format!("{} exists", root.display()),
                    Err(e) => format!("cannot create {}: {e}", root.display()),
                },
                fix: created.as_ref().err().map(|_| {
                    format!(
                        "make {} writable, or point VEX_HOME elsewhere",
                        root.display()
                    )
                }),
            });
            if created.is_ok() {
                checks.push(permissions(root));
            }
        }
    }
    Report {
        schema: crate::SCHEMA,
        ok: checks.iter().all(|c| c.ok),
        vex_home: vex_home.map(Path::to_path_buf),
        checks,
    }
}

fn permissions(root: &Path) -> Check {
    match home::mode(root) {
        None => Check {
            name: "vex_home_permissions",
            ok: true,
            detail: "not applicable on this platform".into(),
            fix: None,
        },
        Some(m) if m & 0o077 == 0 => Check {
            name: "vex_home_permissions",
            ok: true,
            detail: format!("mode {m:o} (owner only)"),
            fix: None,
        },
        Some(m) => Check {
            name: "vex_home_permissions",
            ok: false,
            detail: format!("mode {m:o}: other users can read derived source code"),
            fix: Some(format!("chmod 700 {}", root.display())),
        },
    }
}
