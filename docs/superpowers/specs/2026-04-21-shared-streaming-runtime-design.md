# Shared Streaming Runtime Design

Goal: add a shared native streaming core that parses SSE responses, assembles reasoning/text/tool deltas, and feeds both the Rust runtime and UI consumers.

Scope:
- Build a reusable streaming parser/state machine in native Rust.
- Integrate streaming execution into `AgentRuntime`.
- Surface incremental reasoning and answer snapshots through `mobile_bridge`.
- Reuse the shared parser from the existing GUI streaming path.

Non-goals:
- Rebuild Android transport around SSE.
- Rework provider configuration or persistence schemas.
- Implement the full multi-client refactor for every native frontend in one pass.

Architecture:
- `streaming` module: incremental SSE frame parser plus provider-aware stream assembler that understands OpenAI-style deltas and Anthropic-style `content_block_*` events.
- `AgentRuntime`: when API streaming is enabled, consume stream updates, emit incremental runtime events, finalize either assistant text or tool calls, then continue the existing tool loop.
- `mobile_bridge`: keep HTTP polling snapshots, but update `reasoningContent` and `answerContent` continuously from runtime deltas so Android receives streaming output without changing transport.
- `gui`: replace the ad-hoc SSE parsing loop with the shared streaming consumer.

Key behaviors:
- Support text, reasoning/thinking, and tool-call argument accumulation.
- Treat `[DONE]`, `message_stop`, and `content_block_stop` correctly.
- Ignore keepalive/ping frames.
- Detect stalled streams via idle timeout and fail fast.
- Fall back to non-streaming chat if streaming is disabled.
