# sqlite-vec 0.1.9 (vendored)

The C source of sqlite-vec v0.1.9 (Alex Garcia, MIT OR Apache-2.0), copied
unchanged from the `sqlite-vec` 0.1.9 crate (commit e9f598ab, the version
the golden reference set was recorded with). Vendored rather than used through that crate
because its build script compiles without SIMD, while the official 0.1.9
wheels the reference was recorded with are built with NEON on aarch64: the NEON L2 kernel sums
in a different order, near-tied distances round differently, and the vector
channel's order at a tie diverges from the reference. `build.rs` compiles it
with SQLITE_CORE plus SQLITE_VEC_ENABLE_NEON on aarch64 (verified against
the wheel's `vec_debug()`: "Build flags: neon") and SQLITE_VEC_ENABLE_AVX on
x86_64 (assumed to match the x86_64 wheels; not verified on this machine).

sha256:
- sqlite-vec.c  ba081a47fa02eadc3cf6b16c314b695b84081269349aac722b4efa338fe8fd85
- sqlite-vec.h  86eda29a7f69e272a555e46b92364d8e02666bb582d5e688a2067dd3bd0b529c
- sqlite3.h     882ad3c0448d0324fb3a6b1a85333a9173d539ac669c9972ae1f03722ff86282
- sqlite3ext.h  b184dd1586d935133d37ad76fa353faf0a1021ff2fdedeedcc3498fff74bbb94
