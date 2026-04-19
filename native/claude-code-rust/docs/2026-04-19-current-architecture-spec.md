# Claude Code Rust Current Architecture Spec

## 1. Purpose

This document describes the current `native/claude-code-rust` layout as it
exists inside this Android project after trimming unused distribution assets.
It is a descriptive spec for maintainers, not a refactor plan. The goal is to
explain what each `src` module does today, how it is implemented at a high
level, and whether it matters to the current Android integration path.

## 2. Scope And Cleanup Rules

- `src/` was left intact on purpose.
- `locales/` was kept because `src/i18n/loader.rs` embeds that folder at build
  time.
- `tests/` was kept because it is still useful for Rust-side verification.
- Standalone install scripts, Docker assets, release workflows, and upstream
  release documents were removed because the current project builds the crate
  only as a vendored dependency.

## 3. Current Build Modes

### 3.1 Android build used by this repo

The app currently builds the Rust agent with:

```bash
cargo build --lib --target aarch64-linux-android --no-default-features --features mobile-bridge
```

This means:

- `mobile_bridge` is enabled.
- default features such as `gui-egui` and `i18n` are disabled for the Android
  library build.
- many modules are still compiled anyway, because they are declared
  unconditionally in `src/lib.rs`.

### 3.2 Important consequence

The current crate is broader than the Android app really needs. Even though the
runtime path is narrow, the library surface still includes CLI, memory,
services, plugin, MCP, and terminal code. That is why this cleanup removes
only repository assets and does not try to shrink `src/`.

## 4. Current Android Execution Path

The path that matters to the Android APK today is:

1. Kotlin side loads `libclaude_code_rs.so` through `RustEmbeddedRuntimeBridge`.
2. The native library exposes the `mobile_bridge` feature entry.
3. `src/mobile_bridge/mod.rs` starts an in-process Axum server on localhost.
4. The Android app calls `/api/runs`, `/api/runs/{id}`, `/api/runs/{id}/cancel`,
   and `/api/health`.
5. `MobileBridgeServer` converts request payloads into `AgentExecutionRequest`.
6. `src/agent_runtime.rs` runs the tool-aware agent loop.
7. `src/api/mod.rs` sends chat requests to the configured provider.
8. `src/tools/` executes local tools when the model requests tool calls.
9. Bridge snapshots are sent back to the UI as reasoning text, tool events,
   answer text, error state, or cancellation state.

## 5. Root Source Files

| Path | Role | Current Notes |
| --- | --- | --- |
| `src/lib.rs` | crate root and module registry | Declares most modules unconditionally, feature-gates only `mobile_bridge`, `wasm`, `gui`, `web`, and `i18n`. |
| `src/main.rs` | CLI executable entry | Loads settings, creates `AppState`, and delegates to the CLI runtime. Not used by the Android app. |
| `src/agent_runtime.rs` | shared agent loop | Core runtime for model calls, tool execution, reasoning capture, cancellation, and final answer assembly. This is part of the Android path. |

## 6. Top-Level Module Inventory

| Module | Main Files | Responsibility | Current Implementation | Android Path |
| --- | --- | --- | --- | --- |
| `advanced` | `mod.rs`, `ssh.rs`, `remote.rs`, `project_init.rs` | SSH, remote execution, project bootstrap helpers | Implemented as generic upstream power-user features | No |
| `api` | `mod.rs` | Provider client and response models | OpenAI-compatible HTTP client with tool-call payload support and reasoning field passthrough | Yes |
| `cli` | `mod.rs`, `args.rs`, `commands.rs`, `repl.rs`, `ui.rs` | CLI command parsing, REPL, text UI helpers | Large command surface kept from upstream-style agent product | No |
| `config` | `mod.rs`, `api_config.rs`, `mcp_config.rs`, `settings.rs` | Settings model and config persistence | Shared settings layer used by CLI and runtime construction | Partial |
| `gui` | `mod.rs`, `app.rs`, `chat.rs`, `sidebar.rs`, `settings.rs`, `theme.rs`, `syntax_highlight.rs`, `tool_calls.rs`, `main.rs` | egui desktop app | Feature-gated desktop UI surface | No |
| `i18n` | `mod.rs`, `loader.rs`, `locales.rs`, `translator.rs` | translation loading and locale switching | Feature-gated and backed by `locales/` resources | No in current Android build |
| `mcp` | `mod.rs`, `tools.rs`, `resources.rs`, `prompts.rs`, `sampling.rs`, `server.rs`, `transport.rs` | MCP protocol primitives and server support | Fairly complete MCP scaffold kept in crate root | No |
| `memory` | `mod.rs`, `session.rs`, `history.rs`, `context.rs`, `storage.rs`, `consolidation.rs` | memory/session persistence subsystem | Generic memory system with consolidation and storage abstractions | No |
| `mobile_bridge` | `mod.rs`, `main.rs` | local HTTP bridge used by Android embedding | Axum bridge exposing run lifecycle endpoints and snapshots | Yes |
| `plugins` | `mod.rs`, `commands.rs`, `hooks.rs`, `loader.rs`, `isolation.rs`, `registry.rs` | plugin loading and sandbox model | Upstream plugin subsystem retained as-is | No |
| `services` | `mod.rs`, `agents.rs`, `auto_dream.rs`, `voice.rs`, `magic_docs.rs`, `team_memory_sync.rs`, `plugin_marketplace.rs`, `stress_tests.rs` | higher-level background/product services | Broad service layer for non-Android product features | No |
| `session` | `mod.rs` | session-level abstractions | Thin generic session support outside `memory/` | No |
| `skills` | `mod.rs`, `registry.rs`, `executor.rs`, `builtin.rs` | skill registry and built-in skills | Generic command skill system, not Android-specific | No |
| `state` | `mod.rs` | shared in-memory app state | CLI-oriented runtime state object with conversation and tool registry state | No |
| `terminal` | `mod.rs` | ratatui terminal shell | Simple terminal app wrapper | No |
| `tools` | `mod.rs`, `file_read.rs`, `file_edit.rs`, `file_write.rs`, `execute_command.rs`, `search.rs`, `list_files.rs`, `git_operations.rs`, `task_management.rs`, `note_edit.rs` | local tool execution | Core tool registry used directly by `agent_runtime` | Yes |
| `utils` | `mod.rs`, `project.rs` | shared utility helpers | Path, directory, and project helpers | Indirect |
| `voice` | `mod.rs` | voice input façade | Placeholder implementation with no real recognition backend yet | No |
| `wasm` | `mod.rs`, `client.rs`, `storage.rs`, `bridge.rs` | browser/WebAssembly surface | Feature-gated browser runtime and JS bridge | No |
| `web` | `mod.rs`, `server.rs`, `routes.rs`, `handlers.rs`, `models.rs`, `templates.rs`, `main.rs` | plugin marketplace web app | Feature-gated Axum server and large HTML template layer | No |

## 7. Module-By-Module Notes

### 7.1 `advanced`

- Groups together SSH access, remote execution, and project templating.
- The implementation is product-oriented and independent from Android.
- Nothing in the current Android bridge calls this module.

### 7.2 `api`

- Provides `ApiClient`, `ChatMessage`, `ToolDefinition`, and response models.
- Uses an OpenAI-compatible `/v1/chat/completions` request shape.
- Supports non-streaming and streaming calls.
- Preserves `reasoning_content` on assistant messages, which is important for
  the current Android "deep thinking" UI.

### 7.3 `cli`

- `args.rs` defines a broad command tree including config, MCP, plugins,
  memory, services, agents, skills, and stress-test commands.
- `mod.rs` wires these commands into the runtime.
- `repl.rs` and `ui.rs` implement interactive command-line behaviors.
- This code is not used by the Android app, but it is still part of the crate
  because `lib.rs` exports it unconditionally.

### 7.4 `config`

- `Settings` is the shared configuration object for both CLI and runtime code.
- `api_config.rs` and `mcp_config.rs` define narrower sub-config structures.
- In the Android path, `mobile_bridge` builds an in-memory `Settings` object
  from bridge request payloads before creating `AgentRuntime`.

### 7.5 `gui`

- Desktop GUI surface built with `egui` and `eframe`.
- Contains a full desktop chat shell, settings UI, sidebar, theme, syntax
  highlighting, and tool-call rendering.
- Feature-gated and not compiled in the Android embedded build because default
  features are disabled.

### 7.6 `i18n`

- Uses `rust-embed` to load Fluent locale bundles from `locales/`.
- Provides translation loading, fallback locale selection, and translator
  switching.
- The Android library build currently disables this feature, but the folder
  must stay if future desktop/default builds are still expected.

### 7.7 `mcp`

- Large MCP scaffold containing tools, resources, prompts, sampling, server,
  and transport abstractions.
- The transport layer already models stdio and websocket-style transport
  concepts.
- The Android app does not route through MCP today; direct tool execution is
  used instead.

### 7.8 `memory`

- Implements session memory, history, context accumulation, storage, and
  consolidation flows.
- This subsystem is richer than what the current Android app uses.
- No current bridge endpoint surfaces this memory layer directly.

### 7.9 `mobile_bridge`

- The Android-specific entry point for the native library.
- Defines request and snapshot DTOs such as `BridgeRunRequest`,
  `BridgeRunSnapshot`, `BridgeRunEvent`, and `BridgeRunStatus`.
- Runs an Axum server, tracks active runs in a `DashMap`, supports
  cancellation, and converts runtime events into UI-friendly snapshots.
- This is the most important module for the APK integration.

### 7.10 `plugins`

- Keeps plugin registry, command hooks, loaders, isolation logic, and metadata.
- It reflects the upstream "agent platform" ambition more than the current
  mobile runtime needs.
- Android does not currently expose plugin management or plugin loading.

### 7.11 `services`

- A product-service layer for agents, auto-dream, voice, magic docs, team
  memory sync, plugin marketplace, and stress tests.
- These services are not on the Android embedded path today.
- Because they are unconditional modules, they still contribute to crate
  surface area and compile scope.

### 7.12 `session`

- Thin standalone session abstraction that sits outside the richer `memory/`
  subsystem.
- Presently not central to the Android app integration.

### 7.13 `skills`

- Implements the skill abstraction, registry, executor, and a set of built-in
  skills.
- This subsystem looks designed for CLI or agent-product workflows rather than
  the current APK runtime.
- Not called by `mobile_bridge` or `agent_runtime` in the present path.

### 7.14 `state`

- Defines `AppState`, conversation state, tool registry state, and memory
  bookkeeping.
- `src/main.rs` uses it for the CLI executable.
- The Android app does not use this state object directly.

### 7.15 `terminal`

- Minimal Ratatui terminal application wrapper.
- Handles alternate-screen mode, key events, and a placeholder terminal UI.
- Not part of the Android embedded path.

### 7.16 `tools`

- Core local tool layer used by the agent loop.
- `ToolRegistry` registers file read/edit/write, shell command execution,
  search, file listing, git operations, task management, and note editing.
- `agent_runtime` converts model tool calls into tool registry executions and
  feeds normalized results back into the chat loop.
- This is the second most important module for the Android runtime after
  `mobile_bridge` and `agent_runtime`.

### 7.17 `utils`

- Small helper module for home/config/data directories, directory creation, and
  formatting helpers.
- `project.rs` adds project-level path utilities.
- Mostly indirect support code.

### 7.18 `voice`

- Very small façade with placeholder behavior.
- Prints mode information but does not implement actual speech recognition.
- Effectively dormant for the current Android path.

### 7.19 `wasm`

- Browser-oriented feature set wrapping API calls, browser storage, and JS
  bridge helpers.
- Not built for the current Android embedded library.

### 7.20 `web`

- A full Axum web application for plugin marketplace scenarios.
- Includes routing, handlers, data models, and a very large template file.
- Feature-gated and unrelated to the current mobile runtime integration.

## 8. Implementation Observations

### 8.1 Modules that materially matter today

The current Android path depends mainly on:

- `agent_runtime`
- `mobile_bridge`
- `api`
- `config`
- `tools`
- smaller shared helpers from `utils`

### 8.2 Modules that are present but not on the Android hot path

The following groups are currently carried as upstream breadth rather than app
needs:

- `cli`
- `terminal`
- `gui`
- `web`
- `wasm`
- `plugins`
- `services`
- `memory`
- `skills`
- `advanced`
- `voice`
- large parts of `mcp`

### 8.3 Current architectural trade-off

The crate favors "single broad product crate" over "minimal Android runtime
crate". That is workable for now, but it means the Android embedded build still
compiles many capabilities that the APK never calls.

### 8.4 Why the cleanup stopped at repository assets

The current request explicitly avoided changes under `src/`. Because of that,
the safe cleanup boundary was:

- delete unused repo/distribution files
- keep runtime code intact
- document the actual module design so future slimming can happen from a clear
  baseline

## 9. Retained Files Outside `src`

These non-`src` areas remain intentionally:

- `Cargo.toml` and `Cargo.lock` for Rust dependency and feature definition
- `locales/` for i18n embedding support
- `tests/` for Rust-side verification
- `LICENSE` for upstream license continuity
- `.env.example` because the vendored runtime still has provider-config shaped
  workflows even though the Android app injects settings at runtime
- `RUST_AGENT_INTEGRATION.md` for project-specific bridge notes

## 10. Suggested Reading Order

If someone needs to understand only the Android-relevant path, read in this
order:

1. `src/mobile_bridge/mod.rs`
2. `src/agent_runtime.rs`
3. `src/api/mod.rs`
4. `src/tools/mod.rs`
5. `src/config/mod.rs`
6. `RUST_AGENT_INTEGRATION.md`

If someone wants the full crate inventory, start at `src/lib.rs` and then use
the module table in this document.
