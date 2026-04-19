# Claude Code Rust Vendor

This directory keeps a trimmed vendored copy of the Rust agent implementation
used by the Android app in this repository.

## Scope

We keep the parts that are still useful for local development and Android
embedding:

- `src/`
- `locales/`
- `tests/`
- `docs/`
- `Cargo.toml`
- `Cargo.lock`
- `LICENSE`
- a small amount of repo metadata such as `.gitignore` and `.env.example`

We intentionally removed upstream-style distribution assets that are not part
of the current app workflow, including:

- standalone install scripts
- Docker assets
- GitHub workflow files
- release, migration, benchmark, and deployment documents
- duplicate localized marketing docs

## Current Build Path

The Android app builds this crate as an embedded library with:

```bash
cargo build --lib --target aarch64-linux-android --no-default-features --features mobile-bridge
```

From the repo root you normally use one of these entry points instead:

```powershell
powershell -ExecutionPolicy Bypass -File scripts/build-android-rust-agent.ps1
```

```powershell
.\script\run-android-debug.cmd
```

## Architecture Notes

The current codebase is still much broader than the Android app needs. The
mobile app mainly depends on:

- `src/agent_runtime.rs`
- `src/mobile_bridge/`
- `src/api/`
- `src/tools/`
- `src/config/`

The full descriptive architecture inventory lives in:

- `docs/2026-04-19-current-architecture-spec.md`

That document explains every top-level `src` module, what it contains, and
whether it is on the current Android execution path.
