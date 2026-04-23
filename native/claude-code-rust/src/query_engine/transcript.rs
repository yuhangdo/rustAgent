use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use crate::api::{ChatMessage, ToolCall, ToolCallFunction};
use crate::query_engine::budget::BudgetState;
use crate::query_engine::cost::SessionUsageTotals;
use crate::query_engine::session::QueryRunStatus;

const GLOBAL_TURN_KEY: &str = "__global__";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscriptEvent {
    SessionStarted {
        session_id: String,
        workspace_root: String,
        active_model: String,
    },
    UserMessage {
        content: String,
    },
    AssistantReasoning {
        content: String,
    },
    AssistantMessage {
        content: String,
    },
    ToolCallRequested {
        call_id: String,
        tool_name: String,
        arguments: String,
    },
    ToolCallCompleted {
        call_id: String,
        tool_name: String,
        output: String,
    },
    ToolCallFailed {
        call_id: String,
        tool_name: String,
        error_summary: String,
    },
    UsageRecorded {
        model: String,
        prompt_tokens: usize,
        completion_tokens: usize,
        total_tokens: usize,
        cost_usd: f64,
        usage_missing: bool,
    },
    ModelSwitched {
        model: String,
    },
    BudgetWarning {
        total_cost_usd: f64,
        threshold_usd: f64,
    },
    BudgetExhausted {
        total_cost_usd: f64,
        threshold_usd: f64,
    },
    RunCancelled {
        reason: String,
    },
    RunFailed {
        error_summary: String,
    },
    FileSnapshotCreated {
        path: String,
        snapshot_id: String,
    },
    FilesRewound {
        target_event_id: String,
        restored_paths: Vec<String>,
    },
}

impl TranscriptEvent {
    pub fn session_started(
        session_id: impl Into<String>,
        active_model: impl Into<String>,
        workspace_root: impl Into<String>,
    ) -> Self {
        Self::SessionStarted {
            session_id: session_id.into(),
            workspace_root: workspace_root.into(),
            active_model: active_model.into(),
        }
    }

    pub fn user_message(_event_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self::UserMessage {
            content: content.into(),
        }
    }

    pub fn assistant_message(_event_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self::AssistantMessage {
            content: content.into(),
        }
    }

    pub fn assistant_reasoning(content: impl Into<String>) -> Self {
        Self::AssistantReasoning {
            content: content.into(),
        }
    }

    pub fn model_switched(_event_id: impl Into<String>, model: impl Into<String>) -> Self {
        Self::ModelSwitched {
            model: model.into(),
        }
    }

    pub fn usage_recorded(
        model: impl Into<String>,
        prompt_tokens: usize,
        completion_tokens: usize,
        total_tokens: usize,
        cost_usd: f64,
        usage_missing: bool,
    ) -> Self {
        Self::UsageRecorded {
            model: model.into(),
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cost_usd,
            usage_missing,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptEnvelope {
    pub event_id: String,
    pub turn_id: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub event: TranscriptEvent,
}

#[derive(Debug, Clone)]
pub struct TranscriptReplay {
    pub session_id: String,
    pub workspace_root: Option<PathBuf>,
    pub active_model: String,
    pub messages: Vec<ChatMessage>,
    pub usage_totals: SessionUsageTotals,
    pub budget_state: BudgetState,
    pub last_run_status: QueryRunStatus,
    pub last_error: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    pub event_count: usize,
}

impl Default for TranscriptReplay {
    fn default() -> Self {
        Self {
            session_id: String::new(),
            workspace_root: None,
            active_model: String::new(),
            messages: Vec::new(),
            usage_totals: SessionUsageTotals::default(),
            budget_state: BudgetState::default(),
            last_run_status: QueryRunStatus::Idle,
            last_error: None,
            created_at: None,
            updated_at: None,
            event_count: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TranscriptStore {
    root: PathBuf,
}

impl TranscriptStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn transcript_path(&self) -> PathBuf {
        self.root.join("transcript.jsonl")
    }

    pub async fn append(&self, event: &TranscriptEvent) -> Result<TranscriptEnvelope> {
        self.append_with_turn(None, event).await
    }

    pub async fn append_with_turn(
        &self,
        turn_id: Option<String>,
        event: &TranscriptEvent,
    ) -> Result<TranscriptEnvelope> {
        tokio::fs::create_dir_all(&self.root).await?;

        let envelope = TranscriptEnvelope {
            event_id: uuid::Uuid::new_v4().to_string(),
            turn_id,
            timestamp: Utc::now(),
            event: event.clone(),
        };

        let serialized = serde_json::to_string(&envelope)?;
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.transcript_path())
            .await?;
        file.write_all(serialized.as_bytes()).await?;
        file.write_all(b"\n").await?;

        Ok(envelope)
    }

    pub async fn read_all(&self) -> Result<Vec<TranscriptEnvelope>> {
        if !self.transcript_path().exists() {
            return Ok(Vec::new());
        }

        let content = tokio::fs::read_to_string(self.transcript_path()).await?;
        let mut events = Vec::new();
        for (index, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let envelope: TranscriptEnvelope = serde_json::from_str(trimmed).map_err(|error| {
                anyhow!("Failed to parse transcript line {}: {}", index + 1, error)
            })?;
            events.push(envelope);
        }

        Ok(events)
    }

    pub async fn replay(&self) -> Result<TranscriptReplay> {
        let events = self.read_all().await?;
        let mut replay = TranscriptReplay::default();
        let mut pending_reasoning: HashMap<String, String> = HashMap::new();
        let mut pending_run = false;

        for envelope in &events {
            replay.event_count += 1;
            if replay.created_at.is_none() {
                replay.created_at = Some(envelope.timestamp);
            }
            replay.updated_at = Some(envelope.timestamp);
            let turn_key = envelope
                .turn_id
                .clone()
                .unwrap_or_else(|| GLOBAL_TURN_KEY.to_string());

            match &envelope.event {
                TranscriptEvent::SessionStarted {
                    session_id,
                    workspace_root,
                    active_model,
                } => {
                    replay.session_id = session_id.clone();
                    replay.workspace_root = Some(PathBuf::from(workspace_root));
                    replay.active_model = active_model.clone();
                }
                TranscriptEvent::UserMessage { content } => {
                    replay.messages.push(ChatMessage::user(content.clone()));
                    replay.last_run_status = QueryRunStatus::Running;
                    pending_run = true;
                }
                TranscriptEvent::AssistantReasoning { content } => {
                    pending_reasoning.insert(turn_key, content.clone());
                }
                TranscriptEvent::AssistantMessage { content } => {
                    let reasoning = pending_reasoning.remove(&turn_key).unwrap_or_default();
                    let mut message = ChatMessage::assistant(content.clone());
                    if !reasoning.trim().is_empty() {
                        message.reasoning_content = Some(reasoning);
                    }
                    replay.messages.push(message);
                    replay.last_run_status = QueryRunStatus::Completed;
                    replay.last_error = None;
                    pending_run = false;
                }
                TranscriptEvent::ToolCallRequested {
                    call_id,
                    tool_name,
                    arguments,
                } => {
                    replay
                        .messages
                        .push(ChatMessage::assistant_with_tools(vec![ToolCall {
                            id: call_id.clone(),
                            r#type: "function".to_string(),
                            function: ToolCallFunction {
                                name: tool_name.clone(),
                                arguments: arguments.clone(),
                            },
                        }]));
                    replay.last_run_status = QueryRunStatus::Running;
                    pending_run = true;
                }
                TranscriptEvent::ToolCallCompleted {
                    call_id, output, ..
                } => {
                    replay
                        .messages
                        .push(ChatMessage::tool(call_id.clone(), output.clone()));
                    replay.last_run_status = QueryRunStatus::Running;
                    pending_run = true;
                }
                TranscriptEvent::ToolCallFailed {
                    call_id,
                    error_summary,
                    ..
                } => {
                    let payload = serde_json::json!({
                        "success": false,
                        "error": error_summary,
                    });
                    replay
                        .messages
                        .push(ChatMessage::tool(call_id.clone(), payload.to_string()));
                    replay.last_run_status = QueryRunStatus::Running;
                    replay.last_error = Some(error_summary.clone());
                    pending_run = true;
                }
                TranscriptEvent::UsageRecorded {
                    model,
                    prompt_tokens,
                    completion_tokens,
                    total_tokens,
                    cost_usd,
                    ..
                } => {
                    replay.usage_totals.record_call(
                        model,
                        *prompt_tokens,
                        *completion_tokens,
                        *total_tokens,
                        *cost_usd,
                    );
                }
                TranscriptEvent::ModelSwitched { model } => {
                    replay.active_model = model.clone();
                }
                TranscriptEvent::BudgetWarning {
                    total_cost_usd,
                    threshold_usd,
                } => {
                    replay.budget_state.warning_emitted = true;
                    replay.budget_state.soft_budget_usd = Some(*threshold_usd);
                    replay.budget_state.total_cost_usd = *total_cost_usd;
                }
                TranscriptEvent::BudgetExhausted {
                    total_cost_usd,
                    threshold_usd,
                } => {
                    replay.budget_state.warning_emitted = true;
                    replay.budget_state.hard_limit_reached = true;
                    replay.budget_state.hard_budget_usd = Some(*threshold_usd);
                    replay.budget_state.total_cost_usd = *total_cost_usd;
                }
                TranscriptEvent::RunCancelled { reason } => {
                    replay.last_run_status = QueryRunStatus::Cancelled;
                    replay.last_error = Some(reason.clone());
                    pending_run = false;
                }
                TranscriptEvent::RunFailed { error_summary } => {
                    replay.last_run_status = QueryRunStatus::Failed;
                    replay.last_error = Some(error_summary.clone());
                    pending_run = false;
                }
                TranscriptEvent::FileSnapshotCreated { .. }
                | TranscriptEvent::FilesRewound { .. } => {}
            }
        }

        if pending_run {
            replay.last_run_status = QueryRunStatus::Interrupted;
        }

        Ok(replay)
    }
}

#[cfg(test)]
mod tests {
    use super::{TranscriptEvent, TranscriptStore};

    #[tokio::test]
    async fn transcript_replay_rebuilds_messages_and_model_switches() {
        let temp = tempfile::tempdir().unwrap();
        let store = TranscriptStore::new(temp.path().to_path_buf());

        store
            .append(&TranscriptEvent::session_started(
                "session-1",
                "sonnet",
                temp.path().display().to_string(),
            ))
            .await
            .unwrap();
        store
            .append(&TranscriptEvent::user_message("event-1", "hello"))
            .await
            .unwrap();
        store
            .append(&TranscriptEvent::assistant_message("event-2", "hi there"))
            .await
            .unwrap();
        store
            .append(&TranscriptEvent::model_switched("event-3", "opus"))
            .await
            .unwrap();

        let replay = store.replay().await.unwrap();

        assert_eq!(replay.active_model, "opus");
        assert_eq!(replay.messages.len(), 2);
        assert_eq!(replay.messages[0].role, "user");
        assert_eq!(replay.messages[1].role, "assistant");
    }
}
