//! The VEX_HOME directory: `~/.vex` unless `VEX_HOME` overrides it.
//!
//! The index holds derived source code, so the directory is as sensitive as
//! the code itself: created owner-only (0700), files 0600. An existing
//! directory is never re-permissioned behind the user's back; `vex doctor`
//! reports loose permissions and names the fix.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

/// Subdirectories every VEX_HOME has.
pub const SUBDIRS: &[&str] = &["projects", "models", "cache", "logs"];

/// Mode for every directory vex creates.
pub const DIR_MODE: u32 = 0o700;

/// Resolve VEX_HOME from an override and the user's home directory.
pub fn resolve(override_: Option<&OsStr>, user_home: Option<&Path>) -> Option<PathBuf> {
    match override_ {
        Some(v) if !v.is_empty() => Some(PathBuf::from(v)),
        _ => user_home.map(|h| h.join(".vex")),
    }
}

/// VEX_HOME for this process, from `VEX_HOME` or `$HOME`.
pub fn from_env() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    resolve(std::env::var_os("VEX_HOME").as_deref(), home.as_deref())
}

/// Create VEX_HOME and its subdirectories (0700) where missing.
pub fn ensure(root: &Path) -> io::Result<()> {
    create_private(root)?;
    for sub in SUBDIRS {
        create_private(&root.join(sub))?;
    }
    Ok(())
}

fn create_private(dir: &Path) -> io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(DIR_MODE);
    }
    builder.create(dir)?;
    // The umask can only remove bits, never add them, so mode() is exact
    // unless the directory already existed; set it explicitly regardless.
    set_private(dir)
}

#[cfg(unix)]
fn set_private(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(DIR_MODE))
}

#[cfg(not(unix))]
fn set_private(_dir: &Path) -> io::Result<()> {
    Ok(())
}

/// The permission bits of a directory (unix), or None where not applicable.
pub fn mode(dir: &Path) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(dir)
            .ok()
            .map(|m| m.permissions().mode() & 0o777)
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        None
    }
}
