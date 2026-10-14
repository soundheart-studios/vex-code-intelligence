# Grammar pins

vex must parse with exactly the grammars the golden reference set was recorded
with (`golden/REVISION`), or golden `Parsed` parity is meaningless. Versions
were read from the reference environment's Python wheels on 2026-10-02. The Rust crates are published from the same upstream tags as the
Python wheels.

Source commits are the upstream git commits each crate was published from
(`.cargo_vcs_info.json` in the crate package). The wheels are built from the
same release tags; the fingerprints below are what confirms the two builds
agree.

A matching version number is necessary but not sufficient, so each wired
grammar also carries a structural fingerprint read from the Python wheel
(`Language.abi_version`, `node_kind_count`, `field_count`,
`parse_state_count`). A different `parser.c` would change at least one of
them. `cargo test -p vex-extract --test grammar_versions` asserts both the
locked crate versions and the fingerprints.

## Versions

| Grammar | Python wheel | Rust source | Wired |
| --- | --- | --- | --- |
| tree-sitter (core) | 0.25.2 | crate `tree-sitter` =0.25.2 | yes |
| python | 0.25.0 | crate `tree-sitter-python` =0.25.0 | yes |
| javascript | 0.25.0 | crate `tree-sitter-javascript` =0.25.0 | yes |
| typescript, tsx | 0.23.2 | crate `tree-sitter-typescript` =0.23.2 | yes |
| go | 0.25.0 | crate `tree-sitter-go` =0.25.0 | yes |
| java | 0.23.5 | crate `tree-sitter-java` =0.23.5 | yes |
| rust | 0.24.2 | crate `tree-sitter-rust` =0.24.2 | yes |
| c | 0.24.2 | crate `tree-sitter-c` =0.24.2 | yes |
| cpp | 0.23.4 | crate `tree-sitter-cpp` =0.23.4 | yes |
| c-sharp | 0.23.5 | crate `tree-sitter-c-sharp` =0.23.5 | yes |
| objc | 3.0.2 | crate `tree-sitter-objc` =3.0.2 | yes |
| swift | 0.7.3 | crate `tree-sitter-swift` =0.7.3 | yes |
| scala | 0.26.0 | crate `tree-sitter-scala` =0.26.0 | yes |
| lua | 0.5.0 | crate `tree-sitter-lua` =0.5.0 | yes |
| zig | 1.1.2 | crate `tree-sitter-zig` =1.1.2 | yes |
| elixir | 0.3.5 | crate `tree-sitter-elixir` =0.3.5 | yes |
| bash | 0.25.1 | crate `tree-sitter-bash` =0.25.1 | yes |
| json | 0.24.8 | crate `tree-sitter-json` =0.24.8 | yes |
| julia | 0.23.1 | crate `tree-sitter-julia` =0.23.1 | yes |
| verilog | 1.0.3 | crate `tree-sitter-verilog` =1.0.3 | yes |
| fortran | 0.6.0 | crate `tree-sitter-fortran` =0.6.0 | yes |
| powershell | 0.26.4 | crate `tree-sitter-powershell` =0.26.4 | yes |
| groovy | 0.1.2 | crate `tree-sitter-groovy` =0.1.2 | yes |
| kotlin | 1.1.0 | crate `tree-sitter-kotlin-ng` =1.1.0 | yes |
| php | 0.24.1 | crate `tree-sitter-php` =0.24.2 (crates.io skips 0.24.1; 0.24.2's parser.c differs from the v0.24.1 tag only in `patch_version`, scanner identical) | yes |
| ruby | 0.23.1 | crate `tree-sitter-ruby` =0.23.1 | yes |
| sql (optional) | 0.3.11 | crate `tree-sitter-sequel` =0.3.11 | yes |
| hcl (optional) | 1.2.0 | vendored `parser.c` from the 1.2.0 PyPI sdist (crates.io stops at 1.1.0; vendor/tree-sitter-hcl) | yes |

## Fingerprints (machine-read by the test)

```json
{
  "commits": {
    "tree-sitter": "8b5c63bffae4c1948106eb0a8d722bf2a9141ce5",
    "tree-sitter-python": "293fdc02038ee2bf0e2e206711b69c90ac0d413f",
    "tree-sitter-javascript": "44c892e0be055ac465d5eeddae6d3e194424e7de",
    "tree-sitter-typescript": "f975a621f4e7f532fe322e13c4f79495e0a7b2e7",
    "tree-sitter-go": "1547678a9da59885853f5f5cc8a99cc203fa2e2c",
    "tree-sitter-java": "94703d5a6bed02b98e438d7cad1136c01a60ba2c",
    "tree-sitter-rust": "e2bee853694a1d3e0f6ef308fe3674542fec95d7",
    "tree-sitter-c": "b780e47fc780ddc8da13afa35a3f4ed5c157823d",
    "tree-sitter-cpp": "f41e1a044c8a84ea9fa8577fdd2eab92ec96de02",
    "tree-sitter-c-sharp": "cac6d5fb595f5811a076336682d5d595ac1c9e85",
    "tree-sitter-objc": "18802acf31d0b5c1c1d50bdbc9eb0e1636cab9ed",
    "tree-sitter-swift": "b8b22bffbb3441780e6471665bacfb263741c86a",
    "tree-sitter-scala": "38950b525c9dfc44c8b60d44bdd6e54217286ca8",
    "tree-sitter-lua": "10fe0054734eec83049514ea2e718b2a56acd0c9",
    "tree-sitter-zig": "b670c8df85a1568f498aa5c8cae42f51a90473c0",
    "tree-sitter-elixir": "e2d9e6e0e76b0c436fa48a0b8c32a031d0cbdf49",
    "tree-sitter-bash": "a06c2e4415e9bc0346c6b86d401879ffb44058f7",
    "tree-sitter-json": "ee35a6ebefcef0c5c416c0d1ccec7370cfca5a24",
    "tree-sitter-julia": "a8e1262997d5a45520a06cbe1b86c0737d507054",
    "tree-sitter-verilog": "521b535e41a5acd2c6539a922d4649bbe8275110",
    "tree-sitter-fortran": "48593829df8a929bfe58528eedcf405171921005",
    "tree-sitter-powershell": "d398441825243b00e317e87e1829b9d6a3e54ce0",
    "tree-sitter-groovy": "70efb0b9b50f95bcbd89dcfd42b275e0304e10cf",
    "tree-sitter-sequel": "7b51ecda191d36b92f5a90a8d1bc3faef1c7b8b8",
    "tree-sitter-kotlin-ng": "77dd60ea0a9003ce062c9728a513ffe1aaff8c82",
    "tree-sitter-php": "5b5627faaa290d89eb3d01b9bf47c3bb9e797dea",
    "tree-sitter-ruby": "71bd32fb7607035768799732addba884a37a6210"
  },
  "crates": {
    "tree-sitter": "0.25.2",
    "tree-sitter-python": "0.25.0",
    "tree-sitter-javascript": "0.25.0",
    "tree-sitter-typescript": "0.23.2",
    "tree-sitter-go": "0.25.0",
    "tree-sitter-java": "0.23.5",
    "tree-sitter-rust": "0.24.2",
    "tree-sitter-c": "0.24.2",
    "tree-sitter-cpp": "0.23.4",
    "tree-sitter-c-sharp": "0.23.5",
    "tree-sitter-objc": "3.0.2",
    "tree-sitter-swift": "0.7.3",
    "tree-sitter-scala": "0.26.0",
    "tree-sitter-lua": "0.5.0",
    "tree-sitter-zig": "1.1.2",
    "tree-sitter-elixir": "0.3.5",
    "tree-sitter-bash": "0.25.1",
    "tree-sitter-json": "0.24.8",
    "tree-sitter-julia": "0.23.1",
    "tree-sitter-verilog": "1.0.3",
    "tree-sitter-fortran": "0.6.0",
    "tree-sitter-powershell": "0.26.4",
    "tree-sitter-groovy": "0.1.2",
    "tree-sitter-sequel": "0.3.11",
    "tree-sitter-kotlin-ng": "1.1.0",
    "tree-sitter-php": "0.24.2",
    "tree-sitter-ruby": "0.23.1"
  },
  "abi_exceptions": {
    "sql": {"wheel": 15, "crate": 14, "why": "tree-sitter-sequel 0.3.11's parser.c was generated by an older tree-sitter CLI than the wheel's (v0.25.10). Once [STATE(n)] is written [n], the two files differ only in generator metadata (ABI number, version struct, header comment, renamed table types); every parse and lex table and the scanner are identical."}
  },
  "languages": {
    "python":     {"abi": 15, "kinds": 274, "fields": 32, "states": 2786},
    "javascript": {"abi": 15, "kinds": 265, "fields": 36, "states": 1870},
    "typescript": {"abi": 14, "kinds": 383, "fields": 40, "states": 5870},
    "tsx":        {"abi": 14, "kinds": 400, "fields": 43, "states": 5986},
    "go":         {"abi": 15, "kinds": 219, "fields": 35, "states": 1442},
    "java":       {"abi": 14, "kinds": 321, "fields": 40, "states": 1385},
    "rust":       {"abi": 15, "kinds": 355, "fields": 31, "states": 3825},
    "c":          {"abi": 15, "kinds": 363, "fields": 39, "states": 2015},
    "cpp":        {"abi": 14, "kinds": 543, "fields": 50, "states": 8637},
    "csharp":     {"abi": 15, "kinds": 533, "fields": 26, "states": 8053},
    "objc":       {"abi": 14, "kinds": 588, "fields": 48, "states": 6799},
    "swift":      {"abi": 15, "kinds": 563, "fields": 46, "states": 10321},
    "scala":      {"abi": 15, "kinds": 362, "fields": 32, "states": 26620},
    "lua":        {"abi": 15, "kinds": 143, "fields": 23, "states": 282},
    "zig":        {"abi": 14, "kinds": 274, "fields": 16, "states": 1852},
    "elixir":     {"abi": 14, "kinds": 235, "fields": 9, "states": 7001},
    "bash":       {"abi": 15, "kinds": 280, "fields": 19, "states": 7571},
    "json":       {"abi": 14, "kinds": 25, "fields": 2, "states": 32},
    "julia":      {"abi": 14, "kinds": 245, "fields": 6, "states": 19618},
    "verilog":    {"abi": 14, "kinds": 1223, "fields": 0, "states": 20534},
    "fortran":    {"abi": 15, "kinds": 692, "fields": 34, "states": 9251},
    "powershell": {"abi": 15, "kinds": 419, "fields": 14, "states": 2142},
    "groovy":     {"abi": 14, "kinds": 348, "fields": 43, "states": 2928},
    "sql":        {"abi": 15, "kinds": 729, "fields": 53, "states": 17329},
    "hcl":        {"abi": 14, "kinds": 121, "fields": 2, "states": 678},
    "kotlin":     {"abi": 14, "kinds": 289, "fields": 8, "states": 11432},
    "php":        {"abi": 15, "kinds": 432, "fields": 31, "states": 3371},
    "ruby":       {"abi": 14, "kinds": 351, "fields": 32, "states": 5989}
  }
}
```

## Moving a pin

Only when the reference moves its pin: regenerate the golden set at the new
reference revision, update `golden/REVISION`, re-read versions and fingerprints from the
Python environment, and change this file and `crates/vex-extract/Cargo.toml`
in one dedicated commit.
