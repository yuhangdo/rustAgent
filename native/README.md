# Native Layout

- `native/claude-code-rust/`
  The single retained upstream implementation we build on.
  We intentionally keep the Rust implementation at the repo root and remove the
  restored iteration directories that were bundled alongside it.

## Project Choice

For this repo we standardize on the root Rust implementation from
`lorryjovens-hub/claude-code-rust`.

We do not keep:

- `claude-code-main (2)`
- `claude-code-rev-main`

Those directories are treated as restoration/iteration artifacts, not the base
implementation for this project.

Our project-specific Android bridge now lives inside:

- `native/claude-code-rust/src/mobile_bridge/`
- `native/claude-code-rust/src/agent_runtime.rs`

Android build helper scripts live at:

- `scripts/build-android-rust-agent.ps1`
- `scripts/build-android-rust-agent.sh`

This vendored crate has also been trimmed so the repo keeps only the parts that
still matter to the Android integration path. The current module-level
architecture inventory lives at:

- `native/claude-code-rust/docs/2026-04-19-current-architecture-spec.md`

## Safety

Do not commit runtime secrets under `native/claude-code-rust/.env`.
Use `.env.example` or local untracked files instead.
