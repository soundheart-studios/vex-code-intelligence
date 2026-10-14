# Contributing to vex

Thanks for helping make vex better.

## License and the contributor agreement

vex is dual licensed: [AGPL-3.0-only](LICENSE) for everyone, and commercial
licenses sold by Soundheart Studios. Offering both is only possible when
Soundheart Studios can license every line of the code, so **every contributor
signs a contributor license agreement (CLA) before their first pull request is
merged**. You keep the copyright to your work; the CLA grants Soundheart
Studios the right to license your contribution under the AGPL and under
commercial terms.

The CLA check runs automatically on your first pull request and walks you
through signing. Questions: dev@soundheart.io.

## Before you open a pull request

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Extraction, resolution and search changes must keep golden parity (`cargo xtask parity`, `cargo xtask gate`). If a dependency changes,
regenerate `THIRD_PARTY_LICENSES.md` (the command is at the top of that file).
