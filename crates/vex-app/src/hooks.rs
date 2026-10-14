//! `vex hook install / uninstall / status`: git hooks that keep the index
//! in step with checkouts, merges, rewrites and commits.
//!
//! The hooks directory is git's own answer (`git rev-parse --git-path
//! hooks`, so worktrees and core.hooksPath are honoured). Under husky that
//! is its generated `_` directory, so the user-level `.husky/` is used
//! instead; lefthook's generated hooks are chained into and named, since
//! `lefthook install` regenerates them. vex only ever appends a fenced
//! block and never replaces what a hook already does; the block runs vex by
//! absolute path in the background, output discarded, so a hook's exit
//! status is never vex's.

use crate::project::{ProjectError, project_root};
use serde::Serialize;
use std::path::{Path, PathBuf};

pub const FENCE_START: &str = "# >>> vex (managed by `vex hook install`; edit outside this block)";
pub const FENCE_END: &str = "# <<< vex";
const SHEBANG: &str = "#!/bin/sh\n";

/// (hook, extra `vex sync` arguments).
const HOOKS: &[(&str, &str)] = &[
    ("post-checkout", ""),
    ("post-merge", ""),
    ("post-rewrite", ""),
    ("post-commit", " --record-head"),
];
const PRE_COMMIT: (&str, &str) = ("pre-commit", "");

#[derive(Debug, Serialize)]
pub struct HookReport {
    pub name: &'static str,
    pub path: PathBuf,
    /// created, chained, updated, unchanged, removed, absent.
    pub action: &'static str,
    pub installed: bool,
}

#[derive(Debug, Serialize)]
pub struct HooksReport {
    pub schema: u32,
    pub root: PathBuf,
    pub hooks_dir: PathBuf,
    /// husky, lefthook, pre-commit or core.hooksPath, when one is in charge.
    pub manager: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub hooks: Vec<HookReport>,
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Where hooks go, and who manages them.
fn locate(root: &Path) -> Result<(PathBuf, Option<&'static str>, Option<String>), ProjectError> {
    let raw = git(root, &["rev-parse", "--git-path", "hooks"]).ok_or(ProjectError::BadPath {
        path: root.to_path_buf(),
        error: "not a git repository".into(),
    })?;
    let mut dir = PathBuf::from(raw);
    if dir.is_relative() {
        dir = root.join(dir);
    }
    let custom = git(root, &["config", "--get", "core.hooksPath"]).is_some();
    if dir.file_name().is_some_and(|n| n == "_")
        && (dir.join("h").exists() || dir.join("husky.sh").exists())
        && let Some(parent) = dir.parent()
    {
        return Ok((parent.to_path_buf(), Some("husky"), None));
    }
    let lefthook = [
        "lefthook.yml",
        ".lefthook.yml",
        "lefthook.yaml",
        "lefthook-local.yml",
    ]
    .iter()
    .any(|f| root.join(f).exists());
    if lefthook {
        return Ok((
            dir,
            Some("lefthook"),
            Some(
                "lefthook manages these hooks: vex chained into its generated scripts, which \
                 `lefthook install` regenerates; run `vex hook install` again after it, or add \
                 `vex sync --hook <name>` to lefthook.yml"
                    .into(),
            ),
        ));
    }
    if root.join(".pre-commit-config.yaml").exists() {
        return Ok((dir, Some("pre-commit"), None));
    }
    Ok((dir, custom.then_some("core.hooksPath"), None))
}

/// `"..."` for sh, escaping what double quotes do not protect.
fn sh_quote(path: &Path) -> String {
    let mut s = String::from("\"");
    for c in path.display().to_string().chars() {
        if matches!(c, '"' | '\\' | '$' | '`') {
            s.push('\\');
        }
        s.push(c);
    }
    s.push('"');
    s
}

fn block(vex: &Path, name: &str, extra: &str) -> String {
    format!(
        "{FENCE_START}\n{} sync --hook {name}{extra} >/dev/null 2>&1 </dev/null &\n{FENCE_END}\n",
        sh_quote(vex)
    )
}

/// The text with any vex block removed (what the user had).
fn without_block(text: &str) -> Option<String> {
    let start = text.find(FENCE_START)?;
    let end = text[start..].find(FENCE_END)? + start + FENCE_END.len();
    let end = if text[end..].starts_with('\n') {
        end + 1
    } else {
        end
    };
    Some(format!("{}{}", &text[..start], &text[end..]))
}

#[cfg(unix)]
fn executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)?.permissions().mode();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode | 0o755))
}

fn io(path: &Path, e: std::io::Error) -> ProjectError {
    ProjectError::BadPath {
        path: path.to_path_buf(),
        error: e.to_string(),
    }
}

/// `vex hook install [--pre-commit]`: chain vex into each hook.
pub fn install(start: &Path, vex: &Path, pre_commit: bool) -> Result<HooksReport, ProjectError> {
    let root = project_root(start)?;
    let (dir, manager, note) = locate(&root)?;
    std::fs::create_dir_all(&dir).map_err(|e| io(&dir, e))?;
    let mut hooks = Vec::new();
    let wanted = HOOKS
        .iter()
        .copied()
        .chain(pre_commit.then_some(PRE_COMMIT));
    for (name, extra) in wanted {
        let path = dir.join(name);
        let wanted_block = block(vex, name, extra);
        let existing = std::fs::read_to_string(&path).ok();
        let (text, action) = match &existing {
            None => (format!("{SHEBANG}{wanted_block}"), "created"),
            Some(t) if t.contains(&wanted_block) => (t.clone(), "unchanged"),
            Some(t) => match without_block(t) {
                // An older vex block (another binary path): replace it.
                Some(user) => (append(&user, &wanted_block), "updated"),
                None => (append(t, &wanted_block), "chained"),
            },
        };
        if existing.as_deref() != Some(text.as_str()) {
            std::fs::write(&path, &text).map_err(|e| io(&path, e))?;
        }
        #[cfg(unix)]
        executable(&path).map_err(|e| io(&path, e))?;
        hooks.push(HookReport {
            name,
            path,
            action,
            installed: true,
        });
    }
    Ok(HooksReport {
        schema: crate::SCHEMA,
        root,
        hooks_dir: dir,
        manager,
        note,
        hooks,
    })
}

fn append(user: &str, block: &str) -> String {
    let mut text = user.to_string();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(block);
    text
}

fn all_names() -> impl Iterator<Item = &'static str> {
    HOOKS
        .iter()
        .map(|(n, _)| *n)
        .chain(std::iter::once(PRE_COMMIT.0))
}

/// `vex hook uninstall`: remove vex's blocks, leaving each hook as the user
/// had it (and deleting the ones vex created alone).
pub fn uninstall(start: &Path) -> Result<HooksReport, ProjectError> {
    let root = project_root(start)?;
    let (dir, manager, note) = locate(&root)?;
    let mut hooks = Vec::new();
    for name in all_names() {
        let path = dir.join(name);
        let Some(text) = std::fs::read_to_string(&path).ok() else {
            continue;
        };
        let Some(user) = without_block(&text) else {
            continue;
        };
        if user.trim().is_empty() || user == SHEBANG {
            std::fs::remove_file(&path).map_err(|e| io(&path, e))?;
        } else {
            std::fs::write(&path, &user).map_err(|e| io(&path, e))?;
        }
        hooks.push(HookReport {
            name,
            path,
            action: "removed",
            installed: false,
        });
    }
    Ok(HooksReport {
        schema: crate::SCHEMA,
        root,
        hooks_dir: dir,
        manager,
        note,
        hooks,
    })
}

/// `vex hook status`.
pub fn status(start: &Path) -> Result<HooksReport, ProjectError> {
    let root = project_root(start)?;
    let (dir, manager, note) = locate(&root)?;
    let hooks = HOOKS
        .iter()
        .map(|(name, _)| {
            let path = dir.join(name);
            let installed = std::fs::read_to_string(&path).is_ok_and(|t| t.contains(FENCE_START));
            HookReport {
                name,
                path,
                action: if installed { "unchanged" } else { "absent" },
                installed,
            }
        })
        .collect();
    Ok(HooksReport {
        schema: crate::SCHEMA,
        root,
        hooks_dir: dir,
        manager,
        note,
        hooks,
    })
}
