//! `cargo xtask <task>`: development tasks for vex. Never shipped.

mod bench;
mod e2e;
mod gate;
mod guards;
mod parity;
mod profile;
mod static_deps;
mod verify;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("verify-evidence") => verify::main(&args[1..]),
        Some("check-profile") => profile::main(&workspace_root()),
        Some("check-static-deps") => static_deps::main(&workspace_root(), &args[1..]),
        Some("parity") => parity::main(&workspace_root(), &args[1..]),
        Some("gate") => gate::main(&workspace_root(), &args[1..]),
        Some("bench") => bench::main(&workspace_root(), &args[1..]),
        Some("e2e") => e2e::main(&workspace_root()),
        Some("guards") => guards::main(&workspace_root(), &args[1..]),
        _ => {
            eprintln!(
                "usage: cargo xtask <verify-evidence [--self-check | --print-workflow | --explain-subject] | check-profile | check-static-deps [--release-binary] | parity [--mvp] [--lang L]... [--verify-only | --diff A B] | gate | guards | bench [PKG...] [--edges] [--mvp] | e2e>"
            );
            1
        }
    };
    std::process::exit(code);
}

/// The workspace root: xtask lives one level below it.
fn workspace_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a parent")
        .to_path_buf()
}
