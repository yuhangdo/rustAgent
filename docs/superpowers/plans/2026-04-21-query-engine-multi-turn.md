# Query Engine Multi-Turn Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a resumable multi-turn `QueryEngine` for the shared native runtime path with durable transcripts, usage/cost tracking, budget enforcement, model switching, file rewind, and mobile bridge integration.

**Architecture:** Add a new `query_engine` module above `AgentRuntime` to own session lifecycle and persistence. Keep `AgentRuntime` focused on runtime execution, then adapt its events and tool lifecycle into durable transcript events and session snapshots that `mobile_bridge` can poll.

**Tech Stack:** Rust, Tokio, Serde JSON/JSONL, existing native `AgentRuntime`, existing `mobile_bridge`, filesystem persistence under `~/.claude-code`.

---

### Task 1: Add QueryEngine transcript types and failing persistence tests

**Files:**
- Create: `D:/work/rustAgent/native/claude-code-rust/src/query_engine/mod.rs`
- Create: `D:/work/rustAgent/native/claude-code-rust/src/query_engine/transcript.rs`
- Test: `D:/work/rustAgent/native/claude-code-rust/src/query_engine/transcript.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn transcript_replay_rebuilds_messages_and_model_switches() {
    let temp = tempfile::tempdir().unwrap();
    let store = TranscriptStore::new(temp.path().to_path_buf());

    store.append(&TranscriptEvent::session_started("session-1", "sonnet")).await.unwrap();
    store.append(&TranscriptEvent::user_message("event-1", "hello")).await.unwrap();
    store.append(&TranscriptEvent::assistant_message("event-2", "hi there")).await.unwrap();
    store.append(&TranscriptEvent::model_switched("event-3", "opus")).await.unwrap();

    let replay = store.replay().await.unwrap();

    assert_eq!(replay.active_model, "opus");
    assert_eq!(replay.messages.len(), 2);
    assert_eq!(replay.messages[0].role, "user");
    assert_eq!(replay.messages[1].role, "assistant");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test transcript_replay_rebuilds_messages_and_model_switches --manifest-path native/claude-code-rust/Cargo.toml --lib --no-default-features --features mobile-bridge`
Expected: FAIL because `query_engine::transcript` types do not exist yet.

- [ ] **Step 3: Write minimal implementation**

```rust
pub mod transcript;

pub use transcript::{TranscriptEvent, TranscriptReplay, TranscriptStore};
```

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscriptEvent {
    SessionStarted { session_id: String, active_model: String },
    UserMessage { event_id: String, content: String },
    AssistantMessage { event_id: String, content: String },
    ModelSwitched { event_id: String, model: String },
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test transcript_replay_rebuilds_messages_and_model_switches --manifest-path native/claude-code-rust/Cargo.toml --lib --no-default-features --features mobile-bridge`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add native/claude-code-rust/src/query_engine/mod.rs native/claude-code-rust/src/query_engine/transcript.rs
git commit -m "feat: add query engine transcript primitives"
```

### Task 2: Add session metadata store and resume reconstruction

**Files:**
- Create: `D:/work/rustAgent/native/claude-code-rust/src/query_engine/session.rs`
- Modify: `D:/work/rustAgent/native/claude-code-rust/src/query_engine/mod.rs`
- Modify: `D:/work/rustAgent/native/claude-code-rust/src/query_engine/transcript.rs`
- Test: `D:/work/rustAgent/native/claude-code-rust/src/query_engine/session.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn session_resume_marks_interrupted_run_when_no_terminal_event_exists() {
    let temp = tempfile::tempdir().unwrap();
    let engine = QueryEngine::for_tests(temp.path().to_path_buf());

    let session = engine.create_session("workspace", "sonnet").await.unwrap();
    engine
        .append_transcript_event(&session.session_id, TranscriptEvent::user_message("u1", "hello"))
        .await
        .unwrap();

    let resumed = engine.resume_session(&session.session_id).await.unwrap();

    assert_eq!(resumed.last_run_status, QueryRunStatus::Interrupted);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test session_resume_marks_interrupted_run_when_no_terminal_event_exists --manifest-path native/claude-code-rust/Cargo.toml --lib --no-default-features --features mobile-bridge`
Expected: FAIL because `QueryEngine` session recovery is not implemented.

- [ ] **Step 3: Write minimal implementation**

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum QueryRunStatus {
    Idle,
    Running,
    Completed,
    Cancelled,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuerySessionMetadata {
    pub session_id: String,
    pub workspace_root: PathBuf,
    pub active_model: String,
    pub last_run_status: QueryRunStatus,
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test session_resume_marks_interrupted_run_when_no_terminal_event_exists --manifest-path native/claude-code-rust/Cargo.toml --lib --no-default-features --features mobile-bridge`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add native/claude-code-rust/src/query_engine/mod.rs native/claude-code-rust/src/query_engine/session.rs native/claude-code-rust/src/query_engine/transcript.rs
git commit -m "feat: add resumable query engine sessions"
```

### Task 3: Add usage and cost accounting

**Files:**
- Create: `D:/work/rustAgent/native/claude-code-rust/src/query_engine/cost.rs`
- Modify: `D:/work/rustAgent/native/claude-code-rust/src/query_engine/session.rs`
- Modify: `D:/work/rustAgent/native/claude-code-rust/src/agent_runtime.rs`
- Modify: `D:/work/rustAgent/native/claude-code-rust/src/api/mod.rs`
- Test: `D:/work/rustAgent/native/claude-code-rust/src/query_engine/cost.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn records_costs_per_model_and_session_total() {
    let mut usage = SessionUsageTotals::default();

    usage.record_call("sonnet", 1000, 200, 1200, 0.012);
    usage.record_call("opus", 500, 300, 800, 0.021);

    assert_eq!(usage.total_tokens, 2000);
    assert!((usage.total_cost_usd - 0.033).abs() < 0.0001);
    assert_eq!(usage.model_usage["sonnet"].call_count, 1);
    assert_eq!(usage.model_usage["opus"].total_tokens, 800);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test records_costs_per_model_and_session_total --manifest-path native/claude-code-rust/Cargo.toml --lib --no-default-features --features mobile-bridge`
Expected: FAIL because usage totals are not modeled yet.

- [ ] **Step 3: Write minimal implementation**

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelUsage {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub total_tokens: usize,
    pub total_cost_usd: f64,
    pub call_count: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionUsageTotals {
    pub total_tokens: usize,
    pub total_cost_usd: f64,
    pub model_usage: BTreeMap<String, ModelUsage>,
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test records_costs_per_model_and_session_total --manifest-path native/claude-code-rust/Cargo.toml --lib --no-default-features --features mobile-bridge`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add native/claude-code-rust/src/query_engine/cost.rs native/claude-code-rust/src/query_engine/session.rs native/claude-code-rust/src/agent_runtime.rs native/claude-code-rust/src/api/mod.rs
git commit -m "feat: add query engine usage and cost tracking"
```

### Task 4: Add session-level budget tracking

**Files:**
- Create: `D:/work/rustAgent/native/claude-code-rust/src/query_engine/budget.rs`
- Modify: `D:/work/rustAgent/native/claude-code-rust/src/query_engine/session.rs`
- Modify: `D:/work/rustAgent/native/claude-code-rust/src/query_engine/transcript.rs`
- Test: `D:/work/rustAgent/native/claude-code-rust/src/query_engine/budget.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn hard_budget_blocks_future_submissions_after_threshold() {
    let mut budget = BudgetTracker::new(Some(1.0), Some(2.0));

    assert_eq!(budget.apply_cost(1.2), BudgetDecision::SoftWarning);
    assert_eq!(budget.apply_cost(0.9), BudgetDecision::HardStop);
    assert!(budget.is_hard_stopped());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test hard_budget_blocks_future_submissions_after_threshold --manifest-path native/claude-code-rust/Cargo.toml --lib --no-default-features --features mobile-bridge`
Expected: FAIL because budget tracking is missing.

- [ ] **Step 3: Write minimal implementation**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetDecision {
    None,
    SoftWarning,
    HardStop,
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test hard_budget_blocks_future_submissions_after_threshold --manifest-path native/claude-code-rust/Cargo.toml --lib --no-default-features --features mobile-bridge`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add native/claude-code-rust/src/query_engine/budget.rs native/claude-code-rust/src/query_engine/session.rs native/claude-code-rust/src/query_engine/transcript.rs
git commit -m "feat: add query engine budget enforcement"
```

### Task 5: Add file snapshot and rewind support

**Files:**
- Create: `D:/work/rustAgent/native/claude-code-rust/src/query_engine/file_history.rs`
- Modify: `D:/work/rustAgent/native/claude-code-rust/src/query_engine/mod.rs`
- Modify: `D:/work/rustAgent/native/claude-code-rust/src/agent_runtime.rs`
- Modify: `D:/work/rustAgent/native/claude-code-rust/src/tools/file_edit.rs`
- Modify: `D:/work/rustAgent/native/claude-code-rust/src/tools/file_write.rs`
- Test: `D:/work/rustAgent/native/claude-code-rust/src/query_engine/file_history.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn rewind_restores_file_contents_for_prior_event_boundary() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("sample.txt");
    tokio::fs::write(&file, "before").await.unwrap();

    let history = FileHistoryStore::new(temp.path().join("file-history"));
    history.snapshot("session-1", "event-1", &file).await.unwrap();
    tokio::fs::write(&file, "after").await.unwrap();

    history.rewind_to_event("session-1", "event-1").await.unwrap();

    assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), "before");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test rewind_restores_file_contents_for_prior_event_boundary --manifest-path native/claude-code-rust/Cargo.toml --lib --no-default-features --features mobile-bridge`
Expected: FAIL because file history does not exist yet.

- [ ] **Step 3: Write minimal implementation**

```rust
pub struct FileHistoryStore {
    root: PathBuf,
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test rewind_restores_file_contents_for_prior_event_boundary --manifest-path native/claude-code-rust/Cargo.toml --lib --no-default-features --features mobile-bridge`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add native/claude-code-rust/src/query_engine/file_history.rs native/claude-code-rust/src/query_engine/mod.rs native/claude-code-rust/src/agent_runtime.rs native/claude-code-rust/src/tools/file_edit.rs native/claude-code-rust/src/tools/file_write.rs
git commit -m "feat: add query engine file rewind support"
```

### Task 6: Integrate QueryEngine with AgentRuntime and runtime events

**Files:**
- Modify: `D:/work/rustAgent/native/claude-code-rust/src/agent_runtime.rs`
- Modify: `D:/work/rustAgent/native/claude-code-rust/src/query_engine/mod.rs`
- Modify: `D:/work/rustAgent/native/claude-code-rust/src/query_engine/session.rs`
- Test: `D:/work/rustAgent/native/claude-code-rust/src/query_engine/mod.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn submit_message_persists_runtime_events_and_updates_session_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let engine = QueryEngine::for_tests(temp.path().to_path_buf());
    let session = engine.create_session("workspace", "sonnet").await.unwrap();

    let snapshot = engine
        .submit_message_for_tests(&session.session_id, "inspect repo")
        .await
        .unwrap();

    assert_eq!(snapshot.messages.last().unwrap().role, "assistant");
    assert!(snapshot.updated_at >= snapshot.created_at);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test submit_message_persists_runtime_events_and_updates_session_snapshot --manifest-path native/claude-code-rust/Cargo.toml --lib --no-default-features --features mobile-bridge`
Expected: FAIL because `QueryEngine::submit_message_for_tests` is not implemented.

- [ ] **Step 3: Write minimal implementation**

```rust
pub async fn submit_message(
    &self,
    session_id: &str,
    content: &str,
    event_handler: &dyn AgentEventHandler,
    cancellation: &dyn AgentCancellation,
) -> Result<QuerySessionSnapshot> { /* wire AgentRuntime and transcript append */ }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test submit_message_persists_runtime_events_and_updates_session_snapshot --manifest-path native/claude-code-rust/Cargo.toml --lib --no-default-features --features mobile-bridge`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add native/claude-code-rust/src/agent_runtime.rs native/claude-code-rust/src/query_engine/mod.rs native/claude-code-rust/src/query_engine/session.rs
git commit -m "feat: wire query engine to runtime execution"
```

### Task 7: Add model switching and transcript event replay coverage

**Files:**
- Modify: `D:/work/rustAgent/native/claude-code-rust/src/query_engine/mod.rs`
- Modify: `D:/work/rustAgent/native/claude-code-rust/src/query_engine/session.rs`
- Modify: `D:/work/rustAgent/native/claude-code-rust/src/query_engine/transcript.rs`
- Test: `D:/work/rustAgent/native/claude-code-rust/src/query_engine/mod.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn switch_model_keeps_history_and_changes_next_runtime_model() {
    let temp = tempfile::tempdir().unwrap();
    let engine = QueryEngine::for_tests(temp.path().to_path_buf());
    let session = engine.create_session("workspace", "sonnet").await.unwrap();

    engine.switch_model(&session.session_id, "opus").await.unwrap();
    let resumed = engine.resume_session(&session.session_id).await.unwrap();

    assert_eq!(resumed.active_model, "opus");
    assert!(resumed.messages.is_empty());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test switch_model_keeps_history_and_changes_next_runtime_model --manifest-path native/claude-code-rust/Cargo.toml --lib --no-default-features --features mobile-bridge`
Expected: FAIL because model switching is not persisted.

- [ ] **Step 3: Write minimal implementation**

```rust
pub async fn switch_model(&self, session_id: &str, model: &str) -> Result<QuerySessionSnapshot> {
    // update metadata, append transcript event, persist session snapshot
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test switch_model_keeps_history_and_changes_next_runtime_model --manifest-path native/claude-code-rust/Cargo.toml --lib --no-default-features --features mobile-bridge`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add native/claude-code-rust/src/query_engine/mod.rs native/claude-code-rust/src/query_engine/session.rs native/claude-code-rust/src/query_engine/transcript.rs
git commit -m "feat: persist query engine model switching"
```

### Task 8: Integrate QueryEngine into mobile_bridge

**Files:**
- Modify: `D:/work/rustAgent/native/claude-code-rust/src/mobile_bridge/mod.rs`
- Modify: `D:/work/rustAgent/native/claude-code-rust/src/lib.rs`
- Test: `D:/work/rustAgent/native/claude-code-rust/src/mobile_bridge/mod.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn bridge_snapshot_includes_session_totals_and_active_model() {
    let server = MobileBridgeServer::for_tests();
    let snapshot = server
        .snapshot_from_query_session_for_tests("run-1", "session-1", "opus", 1200, 0.014)
        .await;

    assert_eq!(snapshot.session_id, "session-1");
    assert_eq!(snapshot.active_model, "opus");
    assert_eq!(snapshot.total_tokens, 1200);
    assert!((snapshot.total_cost_usd - 0.014).abs() < 0.0001);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test bridge_snapshot_includes_session_totals_and_active_model --manifest-path native/claude-code-rust/Cargo.toml --lib --no-default-features --features mobile-bridge`
Expected: FAIL because bridge snapshots do not expose session fields.

- [ ] **Step 3: Write minimal implementation**

```rust
pub struct BridgeRunSnapshot {
    pub run_id: String,
    pub session_id: String,
    pub active_model: String,
    pub total_tokens: usize,
    pub total_cost_usd: f64,
    // existing fields remain
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test bridge_snapshot_includes_session_totals_and_active_model --manifest-path native/claude-code-rust/Cargo.toml --lib --no-default-features --features mobile-bridge`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add native/claude-code-rust/src/mobile_bridge/mod.rs native/claude-code-rust/src/lib.rs
git commit -m "feat: expose query sessions through mobile bridge"
```

### Task 9: Run end-to-end verification

**Files:**
- Modify: `D:/work/rustAgent/docs/superpowers/plans/2026-04-21-query-engine-multi-turn.md`

- [ ] **Step 1: Run focused Rust tests**

Run: `cargo test query_engine --manifest-path native/claude-code-rust/Cargo.toml --lib --no-default-features --features mobile-bridge`
Expected: PASS

- [ ] **Step 2: Run mobile bridge tests**

Run: `cargo test mobile_bridge --manifest-path native/claude-code-rust/Cargo.toml --lib --no-default-features --features mobile-bridge`
Expected: PASS

- [ ] **Step 3: Run Android-linked native build**

Run: `powershell -ExecutionPolicy Bypass -File .\\scripts\\build-android-rust-agent.ps1 -Target arm64-v8a`
Expected: `BUILD SUCCESSFUL` and native library copied into Android artifacts.

- [ ] **Step 4: Run app assemble**

Run: `.\\gradlew.bat assembleDebug`
Expected: `BUILD SUCCESSFUL`

- [ ] **Step 5: Commit**

```bash
git add native/claude-code-rust/src/query_engine native/claude-code-rust/src/agent_runtime.rs native/claude-code-rust/src/mobile_bridge/mod.rs native/claude-code-rust/src/tools/file_edit.rs native/claude-code-rust/src/tools/file_write.rs docs/superpowers/plans/2026-04-21-query-engine-multi-turn.md
git commit -m "feat: add multi-turn query engine runtime"
```
