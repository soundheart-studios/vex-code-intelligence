//! `cargo xtask check-profile`: the release profile carries every
//! compile-hardening setting the ADR requires.

use std::path::Path;

/// (key, expected value as TOML) for `[profile.release]`.
pub const REQUIRED: &[(&str, &str)] = &[
    ("lto", "\"fat\""),
    ("codegen-units", "1"),
    ("panic", "\"abort\""),
    ("strip", "true"),
];

/// Every way the manifest's release profile falls short, empty when it complies.
pub fn violations(manifest: &str) -> Vec<String> {
    let doc: toml::Table = match manifest.parse() {
        Ok(d) => d,
        Err(e) => return vec![format!("Cargo.toml does not parse: {e}")],
    };
    let release = doc
        .get("profile")
        .and_then(|p| p.get("release"))
        .and_then(|r| r.as_table());
    let Some(release) = release else {
        return vec!["[profile.release] is missing".to_string()];
    };
    REQUIRED
        .iter()
        .filter_map(|(key, want)| {
            let want: toml::Value =
                toml::from_str::<toml::Table>(&format!("v = {want}")).ok()?["v"].clone();
            match release.get(*key) {
                Some(got) if *got == want => None,
                Some(got) => Some(format!("profile.release.{key} is {got}, expected {want}")),
                None => Some(format!("profile.release.{key} is missing, expected {want}")),
            }
        })
        .collect()
}

pub fn main(root: &Path) -> i32 {
    let manifest = match std::fs::read_to_string(root.join("Cargo.toml")) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("cannot read Cargo.toml: {e}");
            return 1;
        }
    };
    let problems = violations(&manifest);
    if problems.is_empty() {
        println!("release profile: hardened");
        return 0;
    }
    for p in &problems {
        eprintln!("{p}");
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str =
        "[profile.release]\nlto = \"fat\"\ncodegen-units = 1\npanic = \"abort\"\nstrip = true\n";

    #[test]
    fn a_hardened_profile_passes() {
        assert!(violations(GOOD).is_empty());
    }

    #[test]
    fn each_missing_or_wrong_setting_is_named() {
        let weak = "[profile.release]\nlto = \"thin\"\ncodegen-units = 1\nstrip = true\n";
        let v = violations(weak);
        assert_eq!(v.len(), 2, "{v:?}");
        assert!(v.iter().any(|m| m.contains("lto")));
        assert!(v.iter().any(|m| m.contains("panic")));
    }

    #[test]
    fn a_missing_profile_fails() {
        assert_eq!(
            violations("[workspace]\n"),
            ["[profile.release] is missing"]
        );
    }
}
