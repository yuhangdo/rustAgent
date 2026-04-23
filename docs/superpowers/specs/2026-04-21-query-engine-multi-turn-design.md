# Query Engine Multi-Turn Design

Goal: implement the multi-turn conversation mechanisms described in the QueryEngine reference page for the shared native runtime path, centered on `agent_runtime + session/history/persistence + mobile_bridge`.

Reference:
- [多轮对话管理 - QueryEngine 会话编排与持久化](https://ccb.agent-aura.top/docs/conversation/multi-turn)

Scope:
- Add a dedicated `QueryEngine` orchestration layer above `AgentRuntime`.
- Persist sessions as resumable transcripts instead of transient in-memory runs.
- Track per-turn usage and per-session cost, including per-model breakdowns.
- Support budget warning and hard budget stop semantics at the session level.
- Support hot model switching without losing the conversation history.
- Add file snapshot and rewind support for tool-driven file mutations.
- Surface session-aware state through `mobile_bridge` for Android polling clients.

Non-goals:
- Rebuild GUI, WASM, or CLI entry points around the new engine in this pass.
- Replace existing generic memory/history subsystems across the entire crate.
- Introduce provider-specific pricing discovery from remote APIs.
- Rework Android UI contracts beyond what is needed to expose session-aware snapshots.

## Architecture

The implementation introduces a `query_engine` module that becomes the multi-turn state machine for the shared runtime path.

Component boundaries:
- `AgentRuntime`
  - Remains responsible for a single submission execution cycle: prompt assembly, provider call, streaming assembly, tool execution, and final answer/tool events.
  - Stops owning session persistence, accumulated costs, budget policy, model switching, and file rewind logic.
- `QueryEngine`
  - Owns session lifecycle, transcript append, resume/rebuild, model selection, usage aggregation, budget enforcement, and file snapshot orchestration.
  - Adapts `AgentRuntime` events into durable transcript events.
- `mobile_bridge`
  - Stops orchestrating ad-hoc one-off runs directly through `AgentRuntime`.
  - Delegates to `QueryEngine`, then reads session snapshots for polling clients.
- `tools`
  - File mutation tools remain the writers.
  - `QueryEngine` wraps tool execution with pre-write snapshots so rewind can restore workspace files to a prior event boundary.

## Session Storage

Sessions become directory-based instead of single flat JSON blobs.

Base path:
- `~/.claude-code/query-engine/sessions/<session_id>/`

Files:
- `session.json`
  - Stable session metadata and denormalized summary fields.
- `transcript.jsonl`
  - Append-only event stream and source of truth.
- `file-history/`
  - File snapshot payloads plus snapshot index metadata.

`session.json` contains:
- `session_id`
- `workspace_root`
- `created_at`
- `updated_at`
- `active_model`
- `total_tokens`
- `total_cost_usd`
- `model_usage`
- `budget_state`
- `last_run_status`
- `last_user_message_id`
- `last_assistant_message_id`

`transcript.jsonl` contains one JSON object per event. Required event types:
- `session_started`
- `user_message`
- `assistant_reasoning`
- `assistant_message`
- `tool_call_requested`
- `tool_call_completed`
- `tool_call_failed`
- `usage_recorded`
- `model_switched`
- `budget_warning`
- `budget_exhausted`
- `run_cancelled`
- `run_failed`
- `file_snapshot_created`
- `files_rewound`

Rules:
- `transcript.jsonl` is append-only.
- `session.json` is a derived snapshot and may be rebuilt from transcript if needed.
- Resume must not rely on live in-memory state alone.

## Query Engine State Model

`QueryEngine` owns an in-memory `QuerySessionState` rebuilt from disk.

Required state:
- `session_id`
- `workspace_root`
- `active_model`
- `messages`
- `transcript_offset`
- `total_tokens`
- `total_cost_usd`
- `model_usage: HashMap<String, ModelUsage>`
- `budget_state`
- `pending_run`
- `last_error`

`ModelUsage` contains:
- `prompt_tokens`
- `completion_tokens`
- `total_tokens`
- `total_cost_usd`
- `call_count`

`BudgetState` contains:
- `soft_budget_usd`
- `hard_budget_usd`
- `warning_emitted`
- `hard_limit_reached`

## Data Flow

On session start or resume:
1. Create or load the session directory.
2. Replay `transcript.jsonl`.
3. Rebuild message history, active model, usage totals, budget flags, and file history index.
4. Expose a `QuerySessionSnapshot` to callers.

On user submission:
1. Append a `user_message` event.
2. Materialize current runtime request from rebuilt session messages and current model.
3. Execute through `AgentRuntime`.
4. Capture runtime events and append transcript events as they happen.
5. If file mutation tools are invoked, snapshot affected files before the write.
6. When the turn finishes, record usage and costs, update session summary fields, and persist `session.json`.

On resume after restart:
1. Load `session.json` if available.
2. Replay transcript to validate or rebuild derived fields.
3. If the last run ended mid-turn, mark it as interrupted instead of pretending it completed.

## Cost Tracking

Each model call produces a usage record.

Behavior:
- Read `usage.prompt_tokens`, `usage.completion_tokens`, and `usage.total_tokens` from provider responses.
- Convert usage to cost using local model pricing configuration.
- Accumulate into:
  - session total
  - per-model totals
  - latest run summary
- Write a `usage_recorded` transcript event for every model call.

If a provider response does not include usage:
- Record zero-token usage for that call.
- Mark the event metadata as `usage_missing: true`.
- Do not fabricate fake token counts.

## Budget Handling

Session-level budget behavior follows a two-threshold model:

- Soft budget
  - Emits a `budget_warning` event once when crossed.
  - Does not interrupt the in-flight call.
  - Allows future submissions unless hard budget is also reached.
- Hard budget
  - Emits a `budget_exhausted` event once when crossed.
  - Blocks new submissions for the session.
  - Leaves transcript and session snapshot readable.

Rules:
- Budget checks happen after each usage record is written.
- A session already above hard budget may still be resumed for inspection and rewind, but not for new query submission.

## Hot Model Switching

Model switching must preserve the full conversation history.

Behavior:
- `QueryEngine::switch_model(session_id, model)` updates only session-level model selection.
- A `model_switched` transcript event is appended immediately.
- The next user submission rebuilds the runtime request using:
  - full prior messages
  - new model id
  - recomputed context limits for that model

This keeps history stable while allowing model capabilities and token limits to change between turns.

## File Snapshot and Rewind

The engine adds a file history layer for tool-driven file mutations.

Snapshot behavior:
- Trigger only for file mutation tools, initially `file_edit` and `file_write`.
- Before the tool writes, capture the current file contents and metadata.
- Bind each snapshot to the transcript event that caused the mutation.
- Append `file_snapshot_created` to transcript.

Rewind behavior:
- `QueryEngine::rewind_files_to_event(session_id, event_id)` restores files affected after the chosen event boundary.
- Restore only files tracked by snapshots for that session.
- Append a `files_rewound` transcript event after successful restore.

Failure semantics:
- Missing current file but existing snapshot is restorable.
- Missing snapshot content is a hard rewind failure.
- Rewind does not alter transcript history retroactively; it appends a new corrective event.

## Mobile Bridge Integration

`mobile_bridge` becomes session-aware.

Required request fields:
- `run_id`
- `session_id`
- `settings`
- `workspace_root`
- `history` for session bootstrap only when creating a new session
- optional `model_override`

Required snapshot fields:
- `run_id`
- `session_id`
- `status`
- `active_model`
- `reasoning_content`
- `answer_content`
- `total_tokens`
- `total_cost_usd`
- `budget_state`
- `events`

Bridge behavior:
- Existing polling remains unchanged at the transport level.
- Snapshot data is now derived from the active query session instead of transient runtime-only state.

## Error Handling

Persisted error behavior must be explicit.

Cases:
- Provider failure
  - Append `run_failed` with provider error summary.
- Cancellation
  - Append `run_cancelled`.
- Interrupted process during run
  - Rebuild session as `interrupted` on next resume.
- Transcript corruption
  - Fail session load with a descriptive error.
  - Do not silently drop corrupted events.
- Snapshot write failure
  - Abort the file mutation tool before it changes the file.

## Testing

Required tests:
- Query engine replay rebuilds message history, active model, and totals from `transcript.jsonl`.
- Session resume marks interrupted runs correctly.
- Usage records accumulate total cost and per-model cost independently.
- Soft budget emits warning once and does not block the current run.
- Hard budget blocks new submissions after crossing the limit.
- Model switch preserves message history and updates the active model for subsequent runs.
- File snapshot is created before `file_edit` and `file_write`.
- Rewind restores prior file contents for a chosen event boundary.
- Mobile bridge snapshots expose session-aware totals and active model.

## Implementation Notes

Recommended module layout:
- `native/claude-code-rust/src/query_engine/mod.rs`
- `native/claude-code-rust/src/query_engine/session.rs`
- `native/claude-code-rust/src/query_engine/transcript.rs`
- `native/claude-code-rust/src/query_engine/budget.rs`
- `native/claude-code-rust/src/query_engine/cost.rs`
- `native/claude-code-rust/src/query_engine/file_history.rs`

Integration points:
- Extend `api::ChatResponse` handling so runtime callers can retain `usage`.
- Extend runtime turn results and events so `QueryEngine` can persist tool calls, answers, reasoning, and usage without re-parsing provider responses.
- Wrap tool execution in a hookable path so file snapshots occur before mutations.

## Acceptance Criteria

The design is complete when:
- Shared runtime path can create, persist, resume, and continue a multi-turn session.
- Session transcript is durable across process restart.
- Session costs and per-model usage are visible in persisted state and mobile snapshots.
- Hard budget blocks new submissions without destroying session history.
- Switching model does not lose prior messages.
- File mutation tools create rewindable snapshots tied to transcript events.
- Android-facing mobile bridge can read live multi-turn session state without requiring a transport rewrite.
