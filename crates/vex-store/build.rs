//! Compile the vendored sqlite-vec 0.1.9 with the SIMD kernels the official
//! wheels use, so vector distances (and their tie order) match Cortex's.

fn main() {
    let dir = "vendor/sqlite-vec";
    println!("cargo:rerun-if-changed={dir}/sqlite-vec.c");
    println!("cargo:rerun-if-changed={dir}/sqlite-vec.h");
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let mut build = cc::Build::new();
    build
        .file(format!("{dir}/sqlite-vec.c"))
        .include(dir)
        .define("SQLITE_CORE", None)
        .warnings(false);
    match arch.as_str() {
        "aarch64" => {
            // sqlite-vec's int8 NEON kernels pass int8x16_t where uint8x16_t
            // is declared; clang allows it, GCC 14 rejects it without this.
            build
                .define("SQLITE_VEC_ENABLE_NEON", None)
                .flag_if_supported("-flax-vector-conversions");
        }
        "x86_64" => {
            build
                .define("SQLITE_VEC_ENABLE_AVX", None)
                .flag_if_supported("-mavx");
        }
        _ => {}
    }
    build.compile("sqlite_vec0");
}
