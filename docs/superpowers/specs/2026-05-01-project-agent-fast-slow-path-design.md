# Project Agent Fast/Slow Path Design

## Goal

Give the project agent two execution paths:

- A strict read-only quick path for simple retrieval and summarization tasks
- The existing multi-turn agent loop for everything else

The quick path should be faster, safer, and easier to downgrade. The slow path should remain the default escape hatch whenever confidence or safety drops.

## Why This Exists

The existing runtime always enters the normal agent loop, even when the user only needs a small number of read-only lookups. That adds latency and makes simple “find, read, summarize” work more expensive than necessary.

The new design keeps the current runtime intact, but adds a front-loaded quick path that can:

1. Detect safe read-only requests
2. Plan a tiny tool run
3. Execute up to three read-only tools in batched parallel steps
4. Produce a final answer with one LLM pass
5. Downgrade to the normal agent loop whenever anything looks uncertain

## Non-Goals

- No write-capable fast path
- No force-fast override
- No open-ended quick-path loop
- No generic tool DAG engine
- No replacement of the current `AgentRuntime`

## Architecture

### Execution Layers

The runtime now has two execution layers:

- `QuickPathExecutor`
- `AgentRuntime` slow loop

The quick path is attached inside `AgentRuntime`, before the normal loop starts. This keeps the public runtime surface stable while still allowing a conservative fast-path attempt.

### Core Modules

- `native/claude-code-rust/src/fast_path/mod.rs`
  - Routing hints
  - Hard-rule routing
  - Optional LLM route classification
  - Read-only tool planning
  - Read-only command validation
  - Batch planning and parallel execution
  - Finalizer / downgrade logic
- `native/claude-code-rust/src/agent_runtime.rs`
  - Integrates the quick path ahead of the existing multi-turn loop
  - Reuses prompt assembly for the slow loop when the quick path is skipped
  - Rebuilds slow-loop history when the quick path executed tools but downgraded
- `native/claude-code-rust/src/query_engine/*`
  - Threads execution hints through session submission
  - Persists quick-path events into transcript state
- `native/claude-code-rust/src/mobile_bridge/mod.rs`
  - Accepts execution hints from upper layers
  - Surfaces quick-path selection and downgrade events

## Request Surface

The following request types now carry an execution hint:

- `AgentExecutionRequest.execution_mode_hint`
- `QuerySubmitRequest.execution_mode_hint`
- `BridgeRunRequest.execution_mode_hint`

The hint enum is:

- `auto`
- `prefer_fast`
- `prefer_slow`
- `force_slow`

There is intentionally no `force_fast`. Safety always wins.

## Routing Strategy

### Hard Rules First

The quick path is rejected immediately when any of these are true:

- The caller set `prefer_slow` or `force_slow`
- The current conversation already contains tool turns or reasoning turns
- Extra compacted / replayed context sections are active
- The visible history is already too long
- The latest user message looks like write work
- The latest user message looks like a deep multi-step task

Examples of hard-rule slow-path requests:

- “edit this file”
- “implement the feature”
- “plan a refactor”
- “do a root-cause analysis”

### Read-Only Quick Candidates

Requests become direct quick-path candidates when they clearly look like:

- find / search
- list / show
- read / inspect
- summarize / explain
- git status / diff / log style inspection

### LLM Classifier As Fallback

If hard rules cannot safely classify the request, the runtime runs one lightweight classifier call with a strict JSON schema. The classifier must choose `slow` whenever it is unsure.

The classifier output includes:

- `route`
- `confidence`
- `reason`
- `candidate_tools`
- `has_dependencies`

The runtime only accepts a quick-path classification when:

- `route == quick`
- confidence clears the configured threshold
- every suggested tool is whitelisted
- dependency complexity still fits the quick-path limits

## Quick Path Execution Model

### Step 1: Plan

The planner returns strict JSON describing a read-only plan:

- `goal`
- `steps`

Each step includes:

- `id`
- `tool`
- `input`
- `depends_on`
- `read_only`
- `reason`

If the request is not safely solvable in the quick path, the planner must return a slow-path handoff instead of a plan.

### Step 2: Validate

The runtime rejects any plan that violates these rules:

- zero steps
- more than three steps
- non-unique step ids
- non-read-only step
- unsupported tool
- invalid tool input shape
- invalid dependency references
- too many dependency batches

### Step 3: Execute

Execution is “batch parallel, batch serial”:

- Independent steps in the same batch run concurrently
- Dependent steps run in later batches
- The maximum supported depth is two batches

This keeps the quick path fast without turning it into another full agent loop.

### Step 4: Finalize

After tools finish, a finalizer model returns one of:

- `{"status":"answer","answer":"..."}`
- `{"status":"slow","reason":"..."}`

The finalizer must choose the slow path when the evidence is incomplete or conflicting.

## Safety Model

### Read-Only Tool Allowlist

The quick path only permits:

- `search`
- `list_files`
- `file_read`
- `execute_command`

All other tools are rejected at plan validation time.

### Read-Only Command Allowlist

`execute_command` is only available through a strict whitelist plus shell-control filtering.

Guardrails include:

- Reject shell chaining and redirection
- Reject unmatched quoting
- Allow only known read-only commands

Platform-aware command sets:

- Windows:
  - `git ...`
  - `rg ...`
  - `dir ...`
  - `type ...`
  - `where ...`
- Non-Windows:
  - `git ...`
  - `rg ...`
  - `ls ...`
  - `cat ...`
  - `pwd`
  - `which ...`

`git` is also limited to read-only subcommands such as:

- `status`
- `diff`
- `log`
- `show`
- `rev-parse`
- `grep`
- safe `branch` inspection

Mutating subcommands or mutation flags are rejected.

## Downgrade Rules

The quick path downgrades to the slow path when:

- Hard rules reject it
- The classifier rejects it
- The planner rejects it
- Plan validation fails
- Tool batching is invalid
- Any tool call fails
- Tool results are empty
- The finalizer rejects the evidence
- Any quick-path side LLM call errors out

This downgrade behavior is deliberate. Quick-path failures should not fail the overall run.

## Slow-Path Reuse

When the quick path is skipped before tool execution:

- The prompt prepared for the quick-path attempt is reused by the slow loop

When the quick path already executed read-only tools and then downgraded:

- Synthetic assistant tool-call history and tool outputs are appended
- The slow loop rebuilds prompt context from that augmented history

This preserves useful read-only evidence instead of throwing it away.

## Events and Observability

Two new runtime events are emitted:

- `QuickPathSelected`
- `QuickPathDowngraded`

These events are:

- surfaced through `AgentEvent`
- persisted in transcript events
- forwarded through the mobile bridge event stream

The existing tool call events are also reused during quick-path execution, so transcript replay still sees concrete read-only evidence.

## Testing Strategy

Initial automated coverage focuses on the deterministic core:

- Hard-rule routing
- Read-only command validation
- Plan validation
- Execution batch construction

This coverage lives in:

- `native/claude-code-rust/tests/fast_path_test.rs`

These tests lock down the highest-risk correctness boundaries without needing live provider calls.

## Tradeoffs

### Why Not Make the Quick Path Fully Generic

A generic DAG executor or multi-turn fast-path loop would collapse back into another agent runtime and erase the main benefit: predictable, safe latency for simple tasks.

### Why Keep Conservative Downgrade

False negatives are cheaper than false positives here. A simple task that falls back to the slow path is acceptable. A complex or unsafe task that stays on the quick path is not.

## Future Extensions

If the current design works well, the next reasonable extensions are:

- user-visible mode controls in more frontends
- richer quick-path telemetry
- provider-specific fast auxiliary model selection
- better transcript summaries for downgraded quick-path runs

The current design intentionally stops short of write-capable fast execution.
