//! Each structural guard passes on a clean workspace and fails, by name, on a
//! planted violation. The real repository must pass every guard.

use std::path::{Path, PathBuf};
use std::process::Command;
use vex_test_support::TestHome;

/// A minimal clean workspace shaped like vex.
struct Fixture {
    _home: TestHome,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        let home = TestHome::new();
        let root = home.vex_home().join("ws");
        let f = Fixture { _home: home, root };
        for (name, deps, src) in [
            (
                "vex-extract",
                "",
                "pub fn parse(b: &[u8]) -> usize { b.len() }\n",
            ),
            ("vex-resolve", "", "pub fn resolve() {}\n"),
            ("vex-search", "", "pub fn rrf_fuse() {}\n"),
            (
                "vex-app",
                "vex-store = { path = \"../vex-store\" }\n",
                "pub fn run() {}\n",
            ),
            (
                "vex-store",
                "",
                "pub fn open() { let _ = std::fs::read(\"x\"); }\n",
            ),
            (
                "vex-embed",
                "reqwest = \"0.12\"\n",
                "pub fn download() {}\n",
            ),
            (
                "vex-cli",
                "vex-app = { path = \"../vex-app\" }\n",
                "fn main() {}\n",
            ),
        ] {
            f.krate(name, deps, src);
        }
        f
    }

    fn krate(&self, name: &str, deps: &str, src: &str) {
        let dir = self.root.join("crates").join(name);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\n\n[dependencies]\n{deps}"),
        )
        .unwrap();
        std::fs::write(dir.join("src/lib.rs"), src).unwrap();
    }

    fn file(&self, rel: &str, body: &str) {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    fn guards(&self) -> (i32, String) {
        guards(&self.root)
    }
}

fn guards(root: &Path) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["guards", "--root", root.to_str().unwrap()])
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.code().unwrap_or(-1), text)
}

fn assert_fails(f: &Fixture, guard: &str, detail: &str) {
    let (code, text) = f.guards();
    assert_eq!(code, 1, "{guard} should fail:\n{text}");
    assert!(text.contains(&format!("FAIL {guard}")), "{text}");
    assert!(text.contains(detail), "{text}");
}

#[test]
fn a_clean_workspace_passes_every_guard() {
    let (code, text) = Fixture::new().guards();
    assert_eq!(code, 0, "{text}");
}

#[test]
fn the_real_repository_passes_every_guard() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let (code, text) = guards(root);
    assert_eq!(code, 0, "{text}");
}

#[test]
fn pure_crates_reject_filesystem_access() {
    let f = Fixture::new();
    f.krate(
        "vex-extract",
        "",
        "pub fn parse() { let _ = std::fs::read(\"a\"); }\n",
    );
    assert_fails(&f, "pure-crates", "uses std::fs");
}

#[test]
fn pure_crates_reject_io_dependencies() {
    let f = Fixture::new();
    f.krate(
        "vex-resolve",
        "rusqlite = \"0.32\"\n",
        "pub fn resolve() {}\n",
    );
    assert_fails(
        &f,
        "pure-crates",
        "vex-resolve depends on I/O crate rusqlite",
    );
}

#[test]
fn a_comment_mentioning_io_is_not_a_violation() {
    let f = Fixture::new();
    f.krate(
        "vex-extract",
        "",
        "// never touches std::fs\npub fn parse() {}\n",
    );
    assert_eq!(f.guards().0, 0);
}

#[test]
fn adapters_may_not_depend_on_logic_crates() {
    let f = Fixture::new();
    f.krate(
        "vex-mcp",
        "vex-search = { path = \"../vex-search\" }\n",
        "pub fn serve() {}\n",
    );
    assert_fails(&f, "thin-adapters", "vex-mcp depends on vex-search");
}

#[test]
fn a_second_rrf_implementation_fails() {
    let f = Fixture::new();
    f.krate("vex-app", "", "pub fn reciprocal_rank_merge() {}\n");
    assert_fails(&f, "one-rrf", "2 reciprocal-rank implementations");
}

#[test]
fn spawning_an_interpreter_fails() {
    let f = Fixture::new();
    f.krate(
        "vex-app",
        "",
        "pub fn run() { std::process::Command::new(\"python3\").status().ok(); }\n",
    );
    assert_fails(&f, "no-interpreters", "spawns python3");
}

#[test]
fn an_http_client_outside_vex_embed_fails() {
    let f = Fixture::new();
    f.krate("vex-search", "ureq = \"2\"\n", "pub fn rrf_fuse() {}\n");
    assert_fails(&f, "no-http", "vex-search depends on HTTP client ureq");
}

#[test]
fn tracked_golden_data_fails() {
    let f = Fixture::new();
    f.file("golden/REVISION", "abc\n");
    f.file("golden/data/manifest.json", "{}\n");
    let git = |args: &[&str]| {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(&f.root)
                .status()
                .unwrap()
                .success()
        );
    };
    git(&["init", "-q"]);
    git(&["add", "-f", "golden"]);
    assert_fails(
        &f,
        "golden-untracked",
        "golden/data/manifest.json is tracked",
    );
}

#[test]
fn list_names_every_guard() {
    let out = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["guards", "--list"])
        .output()
        .unwrap();
    let names: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split('\t').next())
        .map(String::from)
        .collect();
    assert_eq!(
        names,
        [
            "pure-crates",
            "thin-adapters",
            "one-rrf",
            "no-interpreters",
            "no-http",
            "golden-untracked"
        ]
    );
}
