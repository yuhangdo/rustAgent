# Shared Streaming Runtime Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a shared SSE streaming core that powers `AgentRuntime`, `mobile_bridge`, and the existing Rust GUI streaming path.

**Architecture:** Introduce a native `streaming` module for SSE parsing and provider-specific delta assembly, then wire it into `AgentRuntime` so streaming output updates reasoning, answer text, and tool calls incrementally. `mobile_bridge` keeps snapshot polling but exposes continuously updated output content.

**Tech Stack:** Rust, Tokio, Reqwest streaming, serde_json, Android mobile bridge polling.

---

### Task 1: Shared streaming parser

**Files:**
- Create: `native/claude-code-rust/src/streaming/mod.rs`
- Modify: `native/claude-code-rust/src/lib.rs`

- [ ] Add failing parser tests for OpenAI-style text/reasoning/tool-call deltas and Anthropic-style `content_block_*` events.
- [ ] Implement incremental SSE frame parsing and provider-aware stream assembly.

### Task 2: Runtime streaming integration

**Files:**
- Modify: `native/claude-code-rust/src/agent_runtime.rs`
- Modify: `native/claude-code-rust/src/api/mod.rs`

- [ ] Add failing tests for incremental runtime output assembly.
- [ ] Integrate the shared streaming consumer into `AgentRuntime` when API streaming is enabled.
- [ ] Preserve the existing non-streaming loop as fallback.

### Task 3: Bridge and GUI adoption

**Files:**
- Modify: `native/claude-code-rust/src/mobile_bridge/mod.rs`
- Modify: `native/claude-code-rust/src/gui/app.rs`

- [ ] Update bridge event handling so reasoning and answer snapshots change during streaming without flooding run events.
- [ ] Replace the GUI’s ad-hoc SSE parsing loop with the shared parser.

### Task 4: Verification

**Files:**
- Test: `native/claude-code-rust/src/streaming/mod.rs`
- Test: `native/claude-code-rust/src/agent_runtime.rs`

- [ ] Run targeted Rust tests for streaming parser and runtime behavior.
- [ ] Run Android build verification to confirm mobile integration still compiles.
