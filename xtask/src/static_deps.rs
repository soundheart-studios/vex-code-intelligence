//! `cargo xtask check-static-deps`: release binaries carry ONNX Runtime
//! inside them.
//!
//! Proof, in the time a verification run allows:
//! 1. No crate in the workspace's resolved feature set turns on an ort or
//!    ort-sys feature that links or loads ONNX Runtime as a shared library
//!    (`load-dynamic`, `preload-dylibs`, `copy-dylibs`, `disable-linking`,
//!    `pkg-config`). Features unify across the build, so this is what every
//!    binary that links vex-embed gets, `vex` included.
//! 2. The vex-embed probe, built with the `depcheck` profile (release
//!    without fat LTO, which does not change what is linked dynamically),
//!    lists no onnxruntime library (otool -L on macOS, ldd elsewhere), no
//!    libonnxruntime is copied beside it, and run with `--ort` it reports
//!    the ONNX Runtime linked inside it.
//!
//! `--release-binary` also release-builds `vex` itself and lists its
//! libraries (the release pipeline runs this; a full fat-LTO link does not
//! fit a verification run).

use std::path::{Path, PathBuf};
use std::process::Command;

/// ort/ort-sys features that make ONNX Runtime a shared library.
pub const DYNAMIC_FEATURES: &[&str] = &[
    "load-dynamic",
    "preload-dylibs",
    "copy-dylibs",
    "disable-linking",
    "pkg-config",
];

/// The dynamic-library lines of an `otool -L`/`ldd` listing that name
/// onnxruntime, in the listing's order.
pub fn violations(listing: &str) -> Vec<String> {
    listing
        .lines()
        .map(str::trim)
        .filter(|l| l.to_ascii_lowercase().contains("onnxruntime"))
        .map(str::to_string)
        .collect()
}

/// The dynamic-linking features `cargo metadata`'s resolve turns on for
/// ort or ort-sys, as "package: feature".
pub fn dynamic_features(metadata: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    for node in metadata["resolve"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let id = node["id"].as_str().unwrap_or("");
        let name = id
            .rsplit('#')
            .next()
            .unwrap_or("")
            .split('@')
            .next()
            .unwrap_or("");
        if name != "ort" && name != "ort-sys" {
            continue;
        }
        for f in node["features"].as_array().into_iter().flatten() {
            if let Some(f) = f.as_str()
                && DYNAMIC_FEATURES.contains(&f)
            {
                out.push(format!("{name}: {f}"));
            }
        }
    }
    out
}

fn target_dir(root: &Path, profile: &str) -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target"))
        .join(profile)
}

fn listing(binary: &Path) -> Result<String, String> {
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = Command::new("otool");
        c.arg("-L");
        c
    } else {
        Command::new("ldd")
    };
    let out = cmd
        .arg(binary)
        .output()
        .map_err(|e| format!("cannot list {}: {e}", binary.display()))?;
    // ldd exits 1 for a fully static binary ("not a dynamic executable").
    Ok(String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr))
}

fn build(root: &Path, profile: &str, args: &[&str]) -> bool {
    let mut cmd = Command::new(env!("CARGO"));
    cmd.current_dir(root).args(["build", "--quiet"]);
    if profile == "release" {
        cmd.arg("--release");
    } else {
        cmd.args(["--profile", profile]);
    }
    matches!(cmd.args(args).status(), Ok(s) if s.success())
}

/// Report a binary's dynamic onnxruntime libraries; true when clean.
fn check_binary(rel: &str, binary: &Path) -> bool {
    match listing(binary) {
        Ok(text) => {
            let bad = violations(&text);
            for line in &bad {
                eprintln!("{rel}: links onnxruntime dynamically: {line}");
            }
            if bad.is_empty() {
                println!("{rel}: no dynamic onnxruntime");
            }
            bad.is_empty()
        }
        Err(e) => {
            eprintln!("{e}");
            false
        }
    }
}

pub fn main(root: &Path, args: &[String]) -> i32 {
    let mut ok = true;
    let metadata = Command::new(env!("CARGO"))
        .current_dir(root)
        .args(["metadata", "--format-version", "1", "--quiet"])
        .output();
    match metadata
        .ok()
        .and_then(|o| serde_json::from_slice::<serde_json::Value>(&o.stdout).ok())
    {
        Some(m) => {
            let bad = dynamic_features(&m);
            for f in &bad {
                eprintln!("ONNX Runtime would be linked dynamically: {f}");
            }
            ok &= bad.is_empty();
            if bad.is_empty() {
                println!("features: no dynamic ONNX Runtime linking in the workspace");
            }
        }
        None => {
            eprintln!("cargo metadata failed");
            return 1;
        }
    }

    let depcheck = target_dir(root, "depcheck");
    if !build(root, "depcheck", &["-p", "vex-embed", "--example", "probe"]) {
        eprintln!("build failed for the vex-embed probe");
        return 1;
    }
    let probe = depcheck.join("examples/probe");
    ok &= check_binary("examples/probe", &probe);
    for dir in [
        depcheck.clone(),
        depcheck.join("examples"),
        depcheck.join("deps"),
    ] {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if name.starts_with("libonnxruntime") {
                ok = false;
                eprintln!("copied ONNX Runtime library: {}", entry.path().display());
            }
        }
    }
    match Command::new(&probe).arg("--ort").output() {
        Ok(out) if out.status.success() => {
            let info = String::from_utf8_lossy(&out.stdout);
            println!("probe: {}", info.trim());
            if !info.contains("onnxruntime") {
                ok = false;
                eprintln!("probe did not report a linked ONNX Runtime");
            }
        }
        Ok(out) => {
            ok = false;
            eprintln!(
                "probe failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Err(e) => {
            ok = false;
            eprintln!("cannot run probe: {e}");
        }
    }

    if args.iter().any(|a| a == "--release-binary") {
        if !build(root, "release", &["-p", "vex-cli", "--bin", "vex"]) {
            eprintln!("release build failed for vex");
            return 1;
        }
        ok &= check_binary("vex", &target_dir(root, "release").join("vex"));
    }
    i32::from(!ok)
}

#[cfg(test)]
mod tests {
    use super::{dynamic_features, violations};
    use serde_json::json;

    #[test]
    fn a_dynamic_onnxruntime_is_named() {
        let otool = "target/release/vex:\n\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0)\n\t@rpath/libonnxruntime.1.22.0.dylib (compatibility version 0.0.0)\n";
        assert_eq!(
            violations(otool),
            vec!["@rpath/libonnxruntime.1.22.0.dylib (compatibility version 0.0.0)"]
        );
        let ldd = "\tlinux-vdso.so.1 (0x00007ffd)\n\tlibonnxruntime.so.1 => not found\n\tlibc.so.6 => /lib/libc.so.6\n";
        assert_eq!(violations(ldd), vec!["libonnxruntime.so.1 => not found"]);
    }

    #[test]
    fn system_libraries_alone_pass() {
        let otool = "vex:\n\t/usr/lib/libSystem.B.dylib\n\t/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation\n\t/usr/lib/libc++.1.dylib\n";
        assert!(violations(otool).is_empty());
        assert!(violations("\tnot a dynamic executable\n").is_empty());
    }

    #[test]
    fn dynamic_ort_features_are_named() {
        let meta = json!({"resolve": {"nodes": [
            {"id": "registry+https://github.com/rust-lang/crates.io-index#ort@2.0.0-rc.13",
             "features": ["std", "download-binaries", "load-dynamic"]},
            {"id": "registry+https://github.com/rust-lang/crates.io-index#ort-sys@2.0.0-rc.13",
             "features": ["copy-dylibs"]},
            {"id": "registry+https://github.com/rust-lang/crates.io-index#sort@1.0.0",
             "features": ["load-dynamic"]},
        ]}});
        assert_eq!(
            dynamic_features(&meta),
            vec!["ort: load-dynamic", "ort-sys: copy-dylibs"]
        );
        let clean = json!({"resolve": {"nodes": [
            {"id": "registry+https://github.com/rust-lang/crates.io-index#ort@2.0.0-rc.13",
             "features": ["std", "download-binaries", "tls-rustls"]}]}});
        assert!(dynamic_features(&clean).is_empty());
    }
}
