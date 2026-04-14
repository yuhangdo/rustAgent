# Android AI Agent Console Design

## Goal

Evolve the current Android chat MVP into a developer-focused AI Agent console.

The app should still support conversational interaction, but its primary value should be:

- showing each run as an inspectable execution
- separating user-visible answers from internal diagnostic events
- making provider failures, retries, and request context easy to inspect
- supporting future tool-calling and richer runtime telemetry without redesigning the data model again

## Current State

The current Android app already provides:

- a `Chat` screen for sending prompts and reading replies
- a `Sessions` screen for switching conversations
- a `Settings` screen for choosing a fake or OpenAI-compatible provider
- persistent Room-backed messages and sessions
- separate `reasoningContent` and `answerContent` fields on assistant messages

This is a good MVP for validating persistence and basic provider integration, but it is still a chat app first.

The current design has two limits for a debugging console:

1. reasoning is stored as a message field instead of a run timeline
2. reasoning text is currently eligible to flow back into later provider requests, which mixes diagnostic output with prompt context

## Product Direction

The next version should be a mobile AI Agent console, not a general-purpose consumer chat app.

That means the app should optimize for:

- run visibility over visual polish
- traceability over conversational minimalism
- provider debugging over end-user simplicity

## Proposed Approach

Use a dual-layer model:

- keep `ChatMessage` for the conversation transcript
- introduce `AgentRun` and `RunEvent` for execution diagnostics

This keeps the UI familiar while creating a clean foundation for debugging features.

## Architecture

### Conversation Layer

The conversation layer remains responsible for:

- user input
- assistant-visible answer output
- session grouping
- chat transcript persistence

This is the layer users read as the final interaction result.

### Execution Layer

The execution layer represents one concrete processing attempt for a user message.

Each send action creates one `AgentRun`.

Each run owns a timeline of `RunEvent` entries such as:

- `Started`
- `RequestBuilt`
- `ProviderSelected`
- `ReasoningSummary`
- `AnswerReceived`
- `Completed`
- `Failed`

This layer is for diagnostics, timing, debugging, and future tooling.

## Data Model

### ChatMessage

Keep the existing transcript model with these responsibilities:

- record user messages
- record assistant final answer text
- keep a lightweight reasoning preview only if needed for backwards compatibility

Long term, `ChatMessage.reasoningContent` should no longer be the source of truth for internal execution state.

### AgentRun

Add a new entity representing one run:

- `id`
- `sessionId`
- `userMessageId`
- `assistantMessageId`
- `status`
- `providerType`
- `model`
- `baseUrlSnapshot`
- `startedAt`
- `completedAt`
- `durationMs`
- `errorSummary`

Status values should include:

- `RUNNING`
- `COMPLETED`
- `FAILED`

### RunEvent

Add a new entity representing step-level execution events:

- `id`
- `runId`
- `type`
- `title`
- `details`
- `createdAt`
- `orderIndex`

This structure must be append-only during a run except where a single event is intentionally updated in place.

## UI Design

### Top-Level Navigation

Keep the existing three-tab shell for now:

- `Chat`
- `Sessions`
- `Settings`

This avoids unnecessary navigation churn while the console features are introduced.

### Chat Screen

The chat screen remains the primary working surface, but it gains debugging affordances:

- final answers remain inline in the transcript
- assistant cards show compact run state badges such as `Running`, `Done`, or `Failed`
- a "View Run" affordance opens detailed diagnostics for the associated run
- reasoning is collapsed by default and shown as a summary, not raw hidden chain-of-thought dump

The empty state should explain that this app is an agent console and not just a chat client.

### Run Inspector

Selecting a run should open a diagnostic detail surface.

For the first iteration this can be implemented as a full-screen detail route or a bottom sheet. The exact container is less important than the information structure.

The run inspector should show:

- provider and model used
- start time and duration
- high-level status
- event timeline in chronological order
- error summary when failures occur
- retry action

### Sessions Screen

The session list should show execution health, not only message previews.

Each session row should add:

- last run status
- last run duration if available
- a clearer preview that prefers final answer text over temporary placeholders

### Settings Screen

The settings page should move from raw input fields toward provider profiles.

First iteration:

- keep existing fields
- add saved profile support later
- expose a fake provider scenario selector for testing console states

## Execution Flow

Each user send should follow this flow:

1. User submits a message
2. Create user `ChatMessage`
3. Create assistant placeholder `ChatMessage`
4. Create `AgentRun` with `RUNNING`
5. Append `RunEvent.Started`
6. Append `RunEvent.RequestBuilt`
7. Resolve provider and append `RunEvent.ProviderSelected`
8. Execute provider request
9. When reasoning summary exists, append `RunEvent.ReasoningSummary`
10. When final answer exists, update assistant message answer content
11. Append `RunEvent.AnswerReceived`
12. Mark run `COMPLETED` and append `RunEvent.Completed`
13. If any failure occurs, update assistant placeholder with a failure-facing answer, mark run `FAILED`, and append `RunEvent.Failed`

## Reasoning Policy

The app should not treat raw reasoning as transcript content by default.

Policy:

- final answer belongs in `ChatMessage.answerContent`
- internal diagnostic reasoning belongs in `RunEvent`
- any reasoning shown in the chat transcript should be a short operator-facing summary, not the canonical trace
- previous run reasoning must not be automatically fed back into later provider requests

This is the most important logic correction relative to the current implementation.

## Provider Behavior

### Fake Provider

Expand the fake provider into scenario-based responses for console testing:

- success with reasoning plus answer
- success with answer only
- empty response
- delayed response
- provider error

This enables realistic UI validation without external credentials.

### OpenAI-Compatible Provider

Keep the current provider integration but adapt it to the new execution model:

- request metadata should be snapshotted into the run
- non-success HTTP responses should produce both a failed run and a readable assistant-facing error answer
- reasoning extraction remains best-effort and should not be required for success

Streaming transport can remain deferred if needed, but the model should support incremental events later.

## Error Handling

Errors should surface in two places:

- user-facing concise failure text in the assistant message
- diagnostic detail in the run timeline

Typical cases:

- missing credentials
- HTTP error
- malformed provider payload
- empty answer body
- timeout or connectivity failure

The run inspector should be the canonical place for detailed failure analysis.

## Testing Strategy

Add tests at three levels.

### Repository Tests

Verify:

- run creation and completion
- failed run state transitions
- event ordering
- session preview selection after success and failure

### ViewModel Tests

Verify:

- sending creates both transcript messages and a run
- failed providers create failed run events
- retry creates a new run instead of mutating the old one
- chat UI state reflects active run status correctly

### UI Tests

Verify:

- chat screen shows run status badges
- clicking run details opens the inspector
- failed runs render readable diagnostics
- fake provider scenarios produce the expected UI states

## Scope for First Implementation

Include in the first implementation:

- `AgentRun` persistence
- `RunEvent` persistence
- run creation and completion flow
- run detail UI
- fake provider scenarios
- reasoning no longer being re-injected into prompt context

Defer for later:

- tool-calling events
- profile management
- multi-run comparison
- export or share logs
- live token usage and cost analytics

## Migration Notes

Existing sessions and messages should remain valid.

Migration strategy:

- add new tables for runs and run events
- preserve current messages as-is
- stop using old assistant reasoning text as the canonical execution record

No destructive migration should be required for this iteration.

## Risks

- if run state is mixed too tightly into message state, the architecture will regress back into chat-first design
- if raw reasoning is shown too prominently, the UI can become noisy and hard to scan
- if failures only show in banners, debugging value will stay weak

The implementation should protect against these by keeping transcript and execution layers distinct.

## Recommendation

Implement the agent console incrementally on top of the current app shell.

Do not replace the chat app with a brand-new navigation model yet.

Use the existing tabs, add the execution layer underneath, and make run inspection the first major console feature.
