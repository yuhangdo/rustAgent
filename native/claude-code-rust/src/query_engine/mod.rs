pub mod budget;
pub mod cost;
pub mod file_history;
pub mod session;
pub mod transcript;

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::Value;

use crate::agent_runtime::{
    AgentCancellation, AgentEvent, AgentEventHandler, AgentExecutionOutcome, AgentExecutionRequest,
    AgentRuntime, AgentToolCallHook,
};
use crate::api::ChatMessage;
use crate::config::Settings;

pub use budget::{BudgetDecision, BudgetState, BudgetTracker};
pub use cost::{
    usage_record_for_model, usage_record_from_agent_usage, ModelUsage, SessionUsageTotals,
    UsageRecord,
};
pub use file_history::{FileHistoryStore, FileSnapshotRecord};
pub use session::{QueryRunStatus, QuerySessionMetadata, QuerySessionSnapshot};
pub use transcript::{TranscriptEnvelope, TranscriptEvent, TranscriptReplay, TranscriptStore};

pub struct QueryEngine {
    root: PathBuf,
    soft_budget_usd: Option<f64>,
    hard_budget_usd: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct QuerySubmitRequest {
    pub run_id: String,
    pub session_id: String,
    pub system_prompt: String,
    pub history: Vec<ChatMessage>,
    pub settings: Settings,
    pub workspace_root: PathBuf,
    pub max_iterations: usize,
}

impl QueryEngine {
    pub fn new() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        Self::with_root(
            home.join(".claude-code")
                .join("query-engine")
                .join("sessions"),
        )
    }

    pub fn with_root(root: PathBuf) -> Self {
        Self {
            root,
            soft_budget_usd: None,
            hard_budget_usd: None,
        }
    }

    pub fn for_tests(root: PathBuf) -> Self {
        Self::with_root(root)
    }

    pub fn with_budgets(
        mut self,
        soft_budget_usd: Option<f64>,
        hard_budget_usd: Option<f64>,
    ) -> Self {
        self.soft_budget_usd = soft_budget_usd;
        self.hard_budget_usd = hard_budget_usd;
        self
    }

    pub async fn create_session(
        &self,
        workspace_root: &str,
        model: &str,
    ) -> Result<QuerySessionSnapshot> {
        let session_id = uuid::Uuid::new_v4().to_string();
        self.create_session_with_id(&session_id, workspace_root, model)
            .await
    }

    pub async fn submit_message(
        &self,
        request: QuerySubmitRequest,
        event_handler: &dyn AgentEventHandler,
        cancellation: &dyn AgentCancellation,
    ) -> Result<QuerySessionSnapshot> {
        self.ensure_session_exists(
            &request.session_id,
            &request.workspace_root,
            &request.settings.model,
        )
        .await?;

        let sync_report = self
            .sync_history(&request.session_id, &request.history)
            .await?;
        let mut session = self.resume_session(&request.session_id).await?;
        if session.budget_state.hard_limit_reached {
            return Err(anyhow!(
                "Session budget exhausted for {}",
                request.session_id
            ));
        }

        if !request.settings.model.trim().is_empty()
            && session.active_model != request.settings.model
        {
            session = self
                .switch_model(&request.session_id, &request.settings.model)
                .await?;
        }

        let current_user_event_id = sync_report
            .last_user_event_id
            .or_else(|| self.last_user_event_id(&request.session_id))
            .ok_or_else(|| anyhow!("QueryEngine submit requires at least one user message"))?;

        let runtime = AgentRuntime::new(request.settings.clone());
        let transcript_store = self.transcript_store(&request.session_id);
        let file_history = self.file_history_store();
        let turn_id = request.run_id.clone();
        let transcript_handler = PersistingEventHandler {
            transcript_store: transcript_store.clone(),
            inner: event_handler,
            turn_id: turn_id.clone(),
        };
        let tool_hook = SnapshotToolHook {
            transcript_store: transcript_store.clone(),
            file_history,
            session_id: request.session_id.clone(),
            boundary_event_id: current_user_event_id,
            turn_id: turn_id.clone(),
        };

        let outcome = runtime
            .execute_with_hook(
                AgentExecutionRequest {
                    system_prompt: request.system_prompt,
                    history: session.messages.clone(),
                    workspace_root: request.workspace_root.clone(),
                    max_iterations: request.max_iterations,
                },
                &transcript_handler,
                cancellation,
                &tool_hook,
            )
            .await;

        match outcome {
            Ok(AgentExecutionOutcome::Cancelled) => {
                transcript_store
                    .append_with_turn(
                        Some(turn_id),
                        &TranscriptEvent::RunCancelled {
                            reason: "Cancelled from the UI.".to_string(),
                        },
                    )
                    .await?;
            }
            Ok(AgentExecutionOutcome::Completed(result)) => {
                transcript_store
                    .append_with_turn(
                        Some(turn_id.clone()),
                        &TranscriptEvent::assistant_message(uuid::Uuid::new_v4().to_string(), result.answer),
                    )
                    .await?;

                let mut budget = BudgetTracker::from_state(session.budget_state.clone());
                for usage in &result.usage_records {
                    let usage_record =
                        usage_record_from_agent_usage(&session.active_model, usage);
                    transcript_store
                        .append_with_turn(
                            Some(turn_id.clone()),
                            &TranscriptEvent::usage_recorded(
                                usage_record.model.clone(),
                                usage_record.prompt_tokens,
                                usage_record.completion_tokens,
                                usage_record.total_tokens,
                                usage_record.cost_usd,
                                usage_record.usage_missing,
                            ),
                        )
                        .await?;

                    match budget.apply_cost(usage_record.cost_usd) {
                        BudgetDecision::None => {}
                        BudgetDecision::SoftWarning => {
                            if let Some(threshold) = budget.state().soft_budget_usd {
                                transcript_store
                                    .append_with_turn(
                                        Some(turn_id.clone()),
                                        &TranscriptEvent::BudgetWarning {
                                            total_cost_usd: budget.state().total_cost_usd,
                                            threshold_usd: threshold,
                                        },
                                    )
                                    .await?;
                            }
                        }
                        BudgetDecision::HardStop => {
                            if let Some(threshold) = budget.state().hard_budget_usd {
                                transcript_store
                                    .append_with_turn(
                                        Some(turn_id.clone()),
                                        &TranscriptEvent::BudgetExhausted {
                                            total_cost_usd: budget.state().total_cost_usd,
                                            threshold_usd: threshold,
                                        },
                                    )
                                    .await?;
                            }
                        }
                    }
                }
            }
            Err(error) => {
                transcript_store
                    .append_with_turn(
                        Some(turn_id),
                        &TranscriptEvent::RunFailed {
                            error_summary: error.to_string(),
                        },
                    )
                    .await?;
                return Err(error);
            }
        }

        self.resume_session(&request.session_id).await
    }

    pub async fn append_transcript_event(
        &self,
        session_id: &str,
        event: TranscriptEvent,
    ) -> Result<TranscriptEnvelope> {
        let metadata = self
            .load_metadata(session_id)
            .await?
            .ok_or_else(|| anyhow!("Unknown query session: {}", session_id))?;
        let envelope = self.transcript_store(session_id).append(&event).await?;
        let mut refreshed = metadata;
        refreshed.updated_at = envelope.timestamp;
        self.save_metadata(&refreshed).await?;
        Ok(envelope)
    }

    pub async fn resume_session(&self, session_id: &str) -> Result<QuerySessionSnapshot> {
        let replay = self.transcript_store(session_id).replay().await?;
        let mut metadata = match self.load_metadata(session_id).await? {
            Some(metadata) => metadata,
            None => {
                let workspace_root = replay
                    .workspace_root
                    .clone()
                    .unwrap_or_else(|| PathBuf::from("."));
                QuerySessionMetadata::new(
                    if replay.session_id.is_empty() {
                        session_id.to_string()
                    } else {
                        replay.session_id.clone()
                    },
                    workspace_root,
                    if replay.active_model.is_empty() {
                        "sonnet".to_string()
                    } else {
                        replay.active_model.clone()
                    },
                    BudgetTracker::new(self.soft_budget_usd, self.hard_budget_usd).state(),
                )
            }
        };

        if let Some(created_at) = replay.created_at {
            metadata.created_at = created_at;
        }
        if let Some(updated_at) = replay.updated_at {
            metadata.updated_at = updated_at;
        }
        if !replay.active_model.is_empty() {
            metadata.active_model = replay.active_model.clone();
        }
        if let Some(workspace_root) = replay.workspace_root.clone() {
            metadata.workspace_root = workspace_root;
        }

        metadata.total_tokens = replay.usage_totals.total_tokens;
        metadata.total_cost_usd = replay.usage_totals.total_cost_usd;
        metadata.model_usage = replay.usage_totals.clone();
        metadata.budget_state = merge_budget_state(
            BudgetTracker::new(self.soft_budget_usd, self.hard_budget_usd).state(),
            replay.budget_state.clone(),
        );
        metadata.last_run_status = replay.last_run_status.clone();
        metadata.last_error = replay.last_error.clone();

        self.save_metadata(&metadata).await?;

        Ok(QuerySessionSnapshot {
            session_id: metadata.session_id,
            workspace_root: metadata.workspace_root,
            active_model: metadata.active_model,
            created_at: metadata.created_at,
            updated_at: metadata.updated_at,
            total_tokens: metadata.total_tokens,
            total_cost_usd: metadata.total_cost_usd,
            model_usage: metadata.model_usage,
            budget_state: metadata.budget_state,
            last_run_status: metadata.last_run_status,
            last_error: metadata.last_error,
            messages: replay.messages,
        })
    }

    pub async fn switch_model(
        &self,
        session_id: &str,
        model: &str,
    ) -> Result<QuerySessionSnapshot> {
        self.append_transcript_event(
            session_id,
            TranscriptEvent::model_switched(uuid::Uuid::new_v4().to_string(), model.to_string()),
        )
        .await?;
        let mut snapshot = self.resume_session(session_id).await?;
        snapshot.active_model = model.to_string();
        Ok(snapshot)
    }

    pub async fn rewind_files_to_event(
        &self,
        session_id: &str,
        event_id: &str,
    ) -> Result<QuerySessionSnapshot> {
        let restored_paths = self
            .file_history_store()
            .rewind_to_event(session_id, event_id)
            .await?;
        self.transcript_store(session_id)
            .append(&TranscriptEvent::FilesRewound {
                target_event_id: event_id.to_string(),
                restored_paths,
            })
            .await?;
        self.resume_session(session_id).await
    }

    async fn create_session_with_id(
        &self,
        session_id: &str,
        workspace_root: &str,
        model: &str,
    ) -> Result<QuerySessionSnapshot> {
        let metadata = QuerySessionMetadata::new(
            session_id.to_string(),
            PathBuf::from(workspace_root),
            model.to_string(),
            BudgetTracker::new(self.soft_budget_usd, self.hard_budget_usd).state(),
        );

        tokio::fs::create_dir_all(self.session_root(session_id)).await?;
        self.save_metadata(&metadata).await?;
        self.transcript_store(session_id)
            .append(&TranscriptEvent::session_started(
                session_id.to_string(),
                model.to_string(),
                workspace_root.to_string(),
            ))
            .await?;

        Ok(QuerySessionSnapshot {
            session_id: metadata.session_id,
            workspace_root: metadata.workspace_root,
            active_model: metadata.active_model,
            created_at: metadata.created_at,
            updated_at: metadata.updated_at,
            total_tokens: metadata.total_tokens,
            total_cost_usd: metadata.total_cost_usd,
            model_usage: metadata.model_usage,
            budget_state: metadata.budget_state,
            last_run_status: metadata.last_run_status,
            last_error: metadata.last_error,
            messages: Vec::new(),
        })
    }

    async fn ensure_session_exists(
        &self,
        session_id: &str,
        workspace_root: &Path,
        model: &str,
    ) -> Result<()> {
        if self.load_metadata(session_id).await?.is_some() {
            return Ok(());
        }

        self.create_session_with_id(session_id, &workspace_root.display().to_string(), model)
            .await?;
        Ok(())
    }

    async fn sync_history(
        &self,
        session_id: &str,
        history: &[ChatMessage],
    ) -> Result<HistorySyncReport> {
        let persisted = self.transcript_store(session_id).replay().await?.messages;
        let persisted_visible = visible_messages(&persisted);
        if persisted_visible.len() > history.len() {
            return Err(anyhow!(
                "Provided history is shorter than persisted session history for {}",
                session_id
            ));
        }

        for (left, right) in persisted_visible.iter().zip(history.iter()) {
            if !messages_match(left, right) {
                return Err(anyhow!(
                    "Provided history diverges from persisted session history for {}",
                    session_id
                ));
            }
        }

        let mut last_user_event_id = None;
        for message in history.iter().skip(persisted_visible.len()) {
            match message.role.as_str() {
                "user" => {
                    let envelope = self
                        .append_transcript_event(
                            session_id,
                            TranscriptEvent::user_message(uuid::Uuid::new_v4().to_string(), message.content.clone().unwrap_or_default()),
                        )
                        .await?;
                    last_user_event_id = Some(envelope.event_id);
                }
                "assistant" => {
                    if let Some(reasoning) = message.reasoning_content.clone() {
                        if !reasoning.trim().is_empty() {
                            self.append_transcript_event(
                                session_id,
                                TranscriptEvent::assistant_reasoning(reasoning),
                            )
                            .await?;
                        }
                    }

                    if let Some(tool_calls) = &message.tool_calls {
                        for tool_call in tool_calls {
                            self.append_transcript_event(
                                session_id,
                                TranscriptEvent::ToolCallRequested {
                                    call_id: tool_call.id.clone(),
                                    tool_name: tool_call.function.name.clone(),
                                    arguments: tool_call.function.arguments.clone(),
                                },
                            )
                            .await?;
                        }
                    } else {
                        self.append_transcript_event(
                            session_id,
                            TranscriptEvent::assistant_message(
                                uuid::Uuid::new_v4().to_string(),
                                message.content.clone().unwrap_or_default(),
                            ),
                        )
                        .await?;
                    }
                }
                "tool" => {
                    self.append_transcript_event(
                        session_id,
                        TranscriptEvent::ToolCallCompleted {
                            call_id: message.tool_call_id.clone().unwrap_or_default(),
                            tool_name: "tool".to_string(),
                            output: message.content.clone().unwrap_or_default(),
                        },
                    )
                    .await?;
                }
                _ => {}
            }
        }

        Ok(HistorySyncReport { last_user_event_id })
    }

    async fn save_metadata(&self, metadata: &QuerySessionMetadata) -> Result<()> {
        tokio::fs::create_dir_all(self.session_root(&metadata.session_id)).await?;
        let serialized = serde_json::to_string_pretty(metadata)?;
        tokio::fs::write(self.metadata_path(&metadata.session_id), serialized).await?;
        Ok(())
    }

    async fn load_metadata(&self, session_id: &str) -> Result<Option<QuerySessionMetadata>> {
        let path = self.metadata_path(session_id);
        if !path.exists() {
            return Ok(None);
        }

        let content = tokio::fs::read_to_string(path).await?;
        let metadata = serde_json::from_str(&content)?;
        Ok(Some(metadata))
    }

    fn last_user_event_id(&self, session_id: &str) -> Option<String> {
        std::fs::read_to_string(self.transcript_store(session_id).transcript_path())
            .ok()
            .and_then(|content| {
                content.lines().rev().find_map(|line| {
                    let envelope: TranscriptEnvelope = serde_json::from_str(line).ok()?;
                    match envelope.event {
                        TranscriptEvent::UserMessage { .. } => Some(envelope.event_id),
                        _ => None,
                    }
                })
            })
    }

    pub fn session_root(&self, session_id: &str) -> PathBuf {
        self.root.join(session_id)
    }

    pub fn transcript_store(&self, session_id: &str) -> TranscriptStore {
        TranscriptStore::new(self.session_root(session_id))
    }

    pub fn file_history_store(&self) -> FileHistoryStore {
        FileHistoryStore::new(self.root.join("file-history"))
    }

    pub fn metadata_path(&self, session_id: &str) -> PathBuf {
        self.session_root(session_id).join("session.json")
    }

    pub fn base_root(&self) -> &Path {
        &self.root
    }
}

impl Default for QueryEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Default)]
struct HistorySyncReport {
    last_user_event_id: Option<String>,
}

struct PersistingEventHandler<'a> {
    transcript_store: TranscriptStore,
    inner: &'a dyn AgentEventHandler,
    turn_id: String,
}

#[async_trait]
impl AgentEventHandler for PersistingEventHandler<'_> {
    async fn on_event(&self, event: AgentEvent) {
        match &event {
            AgentEvent::Reasoning { full_text, .. } => {
                let _ = self
                    .transcript_store
                    .append_with_turn(
                        Some(self.turn_id.clone()),
                        &TranscriptEvent::assistant_reasoning(full_text.clone()),
                    )
                    .await;
            }
            AgentEvent::ToolCallRequested {
                tool_call_id,
                tool_name,
                input,
                ..
            } => {
                let _ = self
                    .transcript_store
                    .append_with_turn(
                        Some(self.turn_id.clone()),
                        &TranscriptEvent::ToolCallRequested {
                            call_id: tool_call_id.clone(),
                            tool_name: tool_name.clone(),
                            arguments: input.to_string(),
                        },
                    )
                    .await;
            }
            AgentEvent::ToolCallCompleted {
                tool_call_id,
                tool_name,
                output,
                ..
            } => {
                let _ = self
                    .transcript_store
                    .append_with_turn(
                        Some(self.turn_id.clone()),
                        &TranscriptEvent::ToolCallCompleted {
                            call_id: tool_call_id.clone(),
                            tool_name: tool_name.clone(),
                            output: output.clone(),
                        },
                    )
                    .await;
            }
            AgentEvent::ToolCallFailed {
                tool_call_id,
                tool_name,
                error_summary,
            } => {
                let _ = self
                    .transcript_store
                    .append_with_turn(
                        Some(self.turn_id.clone()),
                        &TranscriptEvent::ToolCallFailed {
                            call_id: tool_call_id.clone(),
                            tool_name: tool_name.clone(),
                            error_summary: error_summary.clone(),
                        },
                    )
                    .await;
            }
            AgentEvent::ReasoningDelta { .. }
            | AgentEvent::AnswerDelta { .. }
            | AgentEvent::FinalAnswer { .. } => {}
        }

        self.inner.on_event(event).await;
    }
}

struct SnapshotToolHook {
    transcript_store: TranscriptStore,
    file_history: FileHistoryStore,
    session_id: String,
    boundary_event_id: String,
    turn_id: String,
}

#[async_trait]
impl AgentToolCallHook for SnapshotToolHook {
    async fn before_tool_call(
        &self,
        _tool_call_id: &str,
        tool_name: &str,
        input: &Value,
    ) -> Result<()> {
        if !matches!(tool_name, "file_edit" | "file_write") {
            return Ok(());
        }

        let file_path = input
            .get("file_path")
            .and_then(Value::as_str)
            .or_else(|| input.get("path").and_then(Value::as_str))
            .ok_or_else(|| anyhow!("{} requires file_path for snapshotting", tool_name))?;
        let snapshot_id = self
            .file_history
            .snapshot(
                &self.session_id,
                &self.boundary_event_id,
                Path::new(file_path),
            )
            .await?;
        self.transcript_store
            .append_with_turn(
                Some(self.turn_id.clone()),
                &TranscriptEvent::FileSnapshotCreated {
                    path: file_path.to_string(),
                    snapshot_id,
                },
            )
            .await?;
        Ok(())
    }
}

fn merge_budget_state(defaults: BudgetState, replay: BudgetState) -> BudgetState {
    BudgetState {
        soft_budget_usd: replay.soft_budget_usd.or(defaults.soft_budget_usd),
        hard_budget_usd: replay.hard_budget_usd.or(defaults.hard_budget_usd),
        warning_emitted: replay.warning_emitted,
        hard_limit_reached: replay.hard_limit_reached,
        total_cost_usd: replay.total_cost_usd,
    }
}

fn messages_match(left: &ChatMessage, right: &ChatMessage) -> bool {
    left.role == right.role
        && left.content == right.content
        && left.reasoning_content == right.reasoning_content
}

fn visible_messages(messages: &[ChatMessage]) -> Vec<ChatMessage> {
    messages
        .iter()
        .filter(|message| message.role == "user" || message.role == "assistant")
        .filter(|message| {
            if message.role != "assistant" {
                return true;
            }

            message
                .tool_calls
                .as_ref()
                .map(|tool_calls| tool_calls.is_empty())
                .unwrap_or(true)
        })
        .cloned()
        .collect()
}
