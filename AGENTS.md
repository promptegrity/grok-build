# AGENTS.md

## Cursor Cloud specific instructions

This repository is **Grok Build** (`grok`), xAI's terminal-based AI coding
agent. It is a single large Rust workspace (edition 2024, toolchain pinned by
`rust-toolchain.toml`). The shipped binary artifact is `xai-grok-pager`
(installed as `grok`); its composition-root package is `xai-grok-pager-bin`.
See `README.md` for the crate map and the canonical build/dev commands, and
`crates/codegen/xai-grok-pager/docs/user-guide/` for user-facing docs.

### Environment / toolchain notes

- The Rust toolchain (1.94.0) is installed automatically by `rustup` on the
  first `cargo` invocation — no manual step needed.
- Proto codegen resolves `bin/protoc`, which is a **DotSlash** wrapper. The
  update script ensures `dotslash` is on `PATH` (`cargo install dotslash`) so
  `bin/protoc` can download and run protoc. Without `dotslash`, builds of any
  crate in the proto closure fail with a "protoc --version failed, likely
  dotslash is missing" error. A system `protoc` on `PATH` or `$PROTOC` also
  works as a fallback.
- The root `Cargo.toml` is generated — treat it as read-only; edit per-crate
  `Cargo.toml` files instead.

### Building / linting / testing (per-crate is the norm)

- Full-workspace builds are **slow** (a clean `cargo build -p xai-grok-pager-bin`
  takes ~5-7 min on a 4-core VM; the scripted-scenario test target adds another
  ~7 min the first time). Always scope with `-p <crate>` (and `--test <name>`)
  as the README advises; avoid whole-workspace `cargo build`/`clippy`/`test`
  unless truly necessary.
- Lint: `cargo fmt --all --check` and `cargo clippy -p <crate>` (clippy config
  is `clippy.toml` at the repo root).
- Test a crate: `cargo test -p <crate>` (e.g. `cargo test -p xai-grok-config`).

### Running the app / offline end-to-end testing

- `./target/debug/xai-grok-pager --version` (or `--help`) works with no
  credentials.
- Doing real agent work (headless `grok -p "..."` or the interactive TUI)
  requires xAI credentials: set `XAI_API_KEY`, or run `grok login`. These are
  **not** present in the cloud VM by default.
- For **offline** end-to-end verification of the agent loop (prompt → model →
  rendered response) **without any secret**, use the scripted TUI scenario
  harness. It runs the real pager binary in a PTY against a bundled
  `MockInferenceServer` (from `xai-grok-test-support`). These tests are
  `#[ignore]`d, so pass `--ignored`. Set `PAGER_BINARY` to a pre-built binary
  to skip an internal release rebuild:

  ```sh
  PAGER_BINARY=$PWD/target/debug/xai-grok-pager \
    cargo test -p xai-grok-pager --test scripted_scenarios \
    scripted_mock_response -- --ignored --nocapture
  ```

  Scenario definitions live in
  `crates/codegen/xai-grok-pager/tests/scenarios/*.yaml`; each run writes
  screenshots (`.svg`/`.html`/`.txt`) and a `report.json` under
  `$CARGO_TARGET_TMPDIR/scripted-scenarios/` (falls back to
  `/tmp/scripted-scenarios/`).
