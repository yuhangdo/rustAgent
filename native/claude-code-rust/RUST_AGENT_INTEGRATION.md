# Rust Agent Integration Notes

This project keeps the root Rust implementation in this directory as the
upstream agent base.

Selected implementation:

- `src/`
- `Cargo.toml`
- the root Rust CLI/library layout

Removed restoration artifacts:

- `claude-code-main (2)`
- `claude-code-rev-main`

Local project work should treat this directory as the vendored upstream base,
while `src/mobile_bridge/` holds the app-specific Android and local bridge
integration code we add on top of that base.

What is now app-specific in this tree:

- `src/agent_runtime.rs`
  Shared tool-aware agent loop used by the mobile bridge and the CLI agent
  service.
- `src/mobile_bridge/`
  Localhost bridge for Android, including run snapshots, cancellation, and
  JNI startup.

Build the local bridge server directly from this tree with:

```bash
cargo run --features mobile-bridge --bin claude-code-mobile-bridge
```

Build the Android native library from this same tree with:

```bash
cargo ndk -t arm64-v8a -o ../../app/src/main/jniLibs \
  build --release --no-default-features --features mobile-bridge
```

Or from the repo root on Windows:

```powershell
powershell -ExecutionPolicy Bypass -File scripts/build-android-rust-agent.ps1
```

Or through Gradle:

```bash
./gradlew :app:buildRustAgentAndroidArm64
```

The Android app loads `libclaude_code_rs.so` through
`RustEmbeddedRuntimeBridge` and then talks to the embedded bridge over
`http://127.0.0.1:<port>/api`.

Bridge capabilities currently exposed to the APK:

- `POST /api/runs`
- `GET /api/runs/{runId}`
- `POST /api/runs/{runId}/cancel`
- `GET /api/health`

The bridge now runs a tool-aware agent loop and can emit reasoning, tool
execution, completion, failure, and cancellation events back to the UI.
