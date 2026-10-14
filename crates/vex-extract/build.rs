//! Compiles the vendored tree-sitter-hcl 1.2.0 grammar (vendor/), which
//! crates.io does not publish, when the lang-hcl feature is on.

fn main() {
    #[cfg(feature = "lang-hcl")]
    {
        let dir = std::path::Path::new("vendor/tree-sitter-hcl/src");
        cc::Build::new()
            .include(dir)
            .file(dir.join("parser.c"))
            .file(dir.join("scanner.c"))
            .warnings(false)
            .flag_if_supported("-std=c11")
            .compile("tree-sitter-hcl");
        println!("cargo:rerun-if-changed=vendor/tree-sitter-hcl/src");
    }
}
