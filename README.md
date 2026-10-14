<p align="center"><img src="Vx.png" alt="vex" width="160"></p>

# vex

Local code intelligence in one binary. vex indexes a repository on your machine — symbols, call
graph, keyword and vector search — and answers the questions coding agents and people ask about it.
Your code never leaves the machine.

Its output is pinned by a golden reference set and a regression gate: every supported language's
extraction, the call graph and search results are checked byte for byte on every change.

## Install

**Platforms:** macOS and Linux (arm64 and x64). Windows support is coming in a future release.

### Prebuilt binary

Download the archive for your platform from
[Releases](https://github.com/soundheart-studios/vex-code-intelligence/releases)
(macOS and Linux, arm64 and x64), then put `vex` on your `PATH`:

```sh
tar -xzf vex-v0.1.0-aarch64-apple-darwin.tar.gz        # use your version and platform
sudo mv vex-v0.1.0-aarch64-apple-darwin/vex /usr/local/bin/
xattr -d com.apple.quarantine /usr/local/bin/vex        # macOS only: binaries aren't signed yet
vex --version
```

Each archive has a `.sha256` file next to it for checking the download.

### From source

**Requirements:** macOS or Linux, Rust 1.85+ (edition 2024; install via [rustup](https://rustup.rs)),
a C compiler (Xcode Command Line Tools on macOS, `build-essential` on Debian/Ubuntu), `git`, and
network access for the first build (it downloads a statically linked ONNX Runtime).

```sh
git clone https://github.com/soundheart-studios/vex-code-intelligence.git
cd vex-code-intelligence
cargo install --locked --path crates/vex-cli   # installs `vex` into ~/.cargo/bin
vex --version                                  # lists the compiled-in grammars
vex doctor                                     # checks the setup; failing checks name their fix
```

Make sure `~/.cargo/bin` is on your `PATH`. The release profile is fully optimised (fat LTO), so the
first build takes several minutes.

## Use it in a project

```sh
cd my-repo
vex init                      # register and index; asks which agent to wire (or --agent claude|codex|all)
vex hook install              # optional: git hooks keep the index in step with checkouts and commits
vex search "where are sessions verified"
```

`vex init` downloads and verifies the embedding model (bge-small) on first use; `vex models pull`
fetches it ahead of time. On a large repo, `vex init --defer-embed` indexes structure now and
`vex embed` fills vectors later. State lives in `~/.vex` (override with `VEX_HOME`); `vex forget`
removes a project's index.

**Agents:** `vex init --agent claude` (or `codex`) registers `vex mcp` with the harness, so the agent
gets `search`, `expand`, `context`, `impact`, `map`, `grep` and `detect_changes` as MCP tools (plus `reindex` and `list_projects`).

A self-starting, self-stopping daemon keeps indexes fresh (file watchers + git hooks) and the
embedding model warm. If the daemon is unavailable, the CLI runs the same code in-process and
prints byte-identical output.

## Commands

| Area | Commands |
| --- | --- |
| Setup | `init [--agent claude\|codex\|all]`, `hook install/uninstall/status`, `forget`, `projects`, `doctor` |
| Indexing | `index [--force] [--defer-embed]`, `sync [--wait]`, `embed`, `status` |
| Queries | `search`, `expand`, `context`, `impact`, `map`, `grep`, `changes [--base REF] [--staged]`, `parse` |
| Runtime | `daemon status/start/stop/restart`, `models list/pull/unload`, `mcp` |

Every command takes `--json` for a stable, versioned machine format (`"schema": 1`). `vex mcp`
serves the same query tools over MCP stdio. Exit codes: `0` ok, `1` usage, `2` no index, `3` stale
index (results still printed), `4` runtime error.

## Supported languages

All at golden parity, parsed with pinned grammar revisions (see
[docs/grammars.md](docs/grammars.md)):

**Python · TypeScript / TSX · JavaScript · Go · Java · Rust · C · C++ (incl. CUDA, Metal) · C# ·
Objective-C / Objective-C++ · Swift · Kotlin · Scala · PHP · Ruby · Lua · Zig · Elixir · Bash ·
PowerShell · Julia · Fortran · Verilog / SystemVerilog · Groovy / Gradle · SQL · HCL / Terraform ·
JSON**

Markdown, YAML and TOML are indexed as line chunks (searchable, no symbols).

Call-graph quality is strongest for TypeScript, Java, Go, C++, Rust and Python. C# and JavaScript
are untuned; Kotlin, PHP and Ruby have known gaps (see below).

## Development

```sh
cargo test --workspace               # unit, golden and contract tests
cargo xtask parity                   # diff `vex parse` against the golden export
scripts/link-golden.sh               # link the golden data set (not public), pinned by golden/REVISION
scripts/install-git-hooks.sh         # structural-guard pre-commit hook
```

CI runs fmt, clippy (`-D warnings`), the test suite and the release-profile check on macOS and
Linux, plus a separate parity workflow.

**Releasing:** push a version tag (`git tag v0.1.0 && git push origin v0.1.0`). The `release`
workflow builds every target, checks ONNX Runtime is linked statically, and publishes the archives
with checksums and license files to a GitHub release. Run it by hand from the Actions tab to get
the archives as build artifacts without publishing.

## Not done yet

- **Hardening & housekeeping** — `vex gc`, `vex config get/set`, a full `vex doctor`;
  pathological inputs and repo edge cases; opt-in login service (`vex daemon install`); fast
  `changes --staged` on large repos.
- **Windows support** (coming) — named pipes for the daemon in place of Unix sockets, and
  forward-slash path normalisation so indexes match across platforms.
- **Distribution** — brew / shell / `cargo binstall` installers; signing and notarisation.
  Tagged releases already build macOS and Linux archives in CI.
- **Call-graph gaps** — Ruby false edges on dynamic
  calls; unresolved typed receivers in Kotlin and PHP; C# and JavaScript untuned.
- **More languages** — Vue / Svelte / Astro, Jupyter notebooks, Dart, R, Solidity,
  GraphQL / Protobuf, Dockerfile / Makefile / CMake.
- **Final sign-off** of the full parity gate across every golden file and all 11 benchmark packages.

## License

Copyright (C) 2026 Soundheart Studios.

vex is free software under the [GNU Affero General Public License v3.0](LICENSE) (AGPL-3.0-only).
You can use, study, modify and share it; if you distribute vex, or let others use a modified
version over a network, you must release your source under the same license.

**Commercial licenses** are available for using vex without the AGPL's obligations: embedding it in
closed-source products, shipping modified builds, or hosting it as a service. Contact
[dev@soundheart.io](mailto:dev@soundheart.io).

Contributions are welcome under a contributor license agreement; see
[CONTRIBUTING.md](CONTRIBUTING.md).

vex builds on open-source components (tree-sitter grammars, ONNX Runtime, SQLite, sqlite-vec and
many Rust crates); their licenses are collected in [THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md).
