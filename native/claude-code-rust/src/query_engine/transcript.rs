use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use crate::api::{ChatMessage, ToolCall, ToolCallFunction};
use crate::auto_mode::{
    AutoModeClassifierStage, AutoModeDecision, AutoModeDecisionBehavior, AutoModeStatus,
};
use crate::plan_mode::{AllowedPrompt, PlanMode, PlanModeStatus};
use crate::prompting::PromptSection;
use crate::query_engine::budget::BudgetState;
use crate::query_engine::cost::SessionUsageTotals;
use crate::query_engine::session::QueryRunStatus;
use crate::token_budget::TokenBudgetState;

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
    MemorySurfaced {
        paths: Vec<String>,
    },
    QuickPathSelected {
        reason: String,
        planned_tools: usize,
        batch_count: usize,
        used_classifier: bool,
    },
    QuickPathDowngraded {
        reason: String,
        executed_tools: usize,
    },
    AutoModeEntered {
        previous_mode: String,
        model: String,
        stripped_dangerous_rules: Vec<String>,
    },
    AutoModeExited {
        restored_dangerous_rules: Vec<String>,
    },
    AutoModeDecisionRecorded {
        tool_name: String,
        behavior: AutoModeDecisionBehavior,
        reason: String,
        stage: Option<AutoModeClassifierStage>,
        unavailable: bool,
        transcript_too_long: bool,
    },
    PlanModeEntered {
        previous_mode: String,
    },
    PlanModeExited {
        plan_file_path: String,
        allowed_prompts: Vec<AllowedPrompt>,
        awaiting_approval: bool,
        plan_was_edited: bool,
    },
    TokenBudgetWarning {
        active_tokens: usize,
        warning_threshold: usize,
        effective_budget_tokens: usize,
    },
    AutoCompactPerformed {
        strategy: String,
        before_tokens: usize,
        after_tokens: usize,
        compacted_messages: usize,
        preserved_messages: usize,
    },
    AutoCompactFailed {
        strategy: String,
        error_summary: String,
        consecutive_failures: usize,
    },
    SessionCompacted {
        strategy: String,
        history: Vec<ChatMessage>,
        system_sections: Vec<PromptSection>,
        user_context_sections: Vec<PromptSection>,
        before_tokens: usize,
        after_tokens: usize,
        compacted_messages: usize,
        preserved_messages: usize,
    },
    TokenBudgetBlocked {
        active_tokens: usize,
        blocking_threshold: usize,
        effective_budget_tokens: usize,
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
    pub token_budget_state: Option<TokenBudgetState>,
    pub additional_system_sections: Vec<PromptSection>,
    pub additional_user_context_sections: Vec<PromptSection>,
    pub plan_mode_status: PlanModeStatus,
    pub auto_mode_status: AutoModeStatus,
    pub auto_mode_decisions: Vec<AutoModeDecision>,
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
            token_budget_state: None,
            additional_system_sections: Vec::new(),
            additional_user_context_sections: Vec::new(),
            plan_mode_status: PlanModeStatus::default(),
            auto_mode_status: AutoModeStatus::default(),
            auto_mode_decisions: Vec::new(),
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

    pub async fn surfaced_memory_paths(&self) -> Result<Vec<String>> {
        let mut paths = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for envelope in self.read_all().await? {
            if let TranscriptEvent::MemorySurfaced {
                paths: surfaced_paths,
            } = envelope.event
            {
                for path in surfaced_paths {
                    if seen.insert(path.clone()) {
                        paths.push(path);
                    }
                }
            }
        }

        Ok(paths)
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
                TranscriptEvent::MemorySurfaced { .. }
                | TranscriptEvent::QuickPathSelected { .. }
                | TranscriptEvent::QuickPathDowngraded { .. } => {}
                TranscriptEvent::AutoModeEntered {
                    previous_mode,
                    model,
                    stripped_dangerous_rules,
                } => {
                    replay.auto_mode_status.active = true;
                    replay.auto_mode_status.previous_mode = Some(previous_mode.clone());
                    replay.auto_mode_status.model = model.clone();
                    replay.auto_mode_status.model_supported = true;
                    replay.auto_mode_status.circuit_broken = false;
                    replay.auto_mode_status.stripped_dangerous_rules =
                        stripped_dangerous_rules.clone();
                }
                TranscriptEvent::AutoModeExited { .. } => {
                    replay.auto_mode_status.active = false;
                }
                TranscriptEvent::AutoModeDecisionRecorded {
                    tool_name: _,
                    behavior,
                    reason,
                    stage,
                    unavailable,
                    transcript_too_long,
                } => {
                    replay.auto_mode_decisions.push(AutoModeDecision {
                        behavior: *behavior,
                        should_block: *behavior != AutoModeDecisionBehavior::Allow,
                        reason: reason.clone(),
                        unavailable: *unavailable,
                        transcript_too_long: *transcript_too_long,
                        model: replay.auto_mode_status.model.clone(),
                        stage: *stage,
                    });
                }
                TranscriptEvent::PlanModeEntered { previous_mode } => {
                    replay.plan_mode_status.mode = PlanMode::Plan;
                    replay.plan_mode_status.previous_mode = Some(previous_mode.clone());
                    replay.plan_mode_status.awaiting_approval = false;
                    replay.plan_mode_status.allowed_prompts.clear();
                    replay.plan_mode_status.plan_file_path = PathBuf::new();
                    replay.plan_mode_status.plan_was_edited = false;
                    replay.plan_mode_status.ultraplan = None;
                }
                TranscriptEvent::PlanModeExited {
                    plan_file_path,
                    allowed_prompts,
                    awaiting_approval,
                    plan_was_edited,
                } => {
                    replay.plan_mode_status.mode = if *awaiting_approval {
                        PlanMode::AwaitingApproval
                    } else {
                        PlanMode::Default
                    };
                    replay.plan_mode_status.plan_file_path = PathBuf::from(plan_file_path);
                    replay.plan_mode_status.allowed_prompts = allowed_prompts.clone();
                    replay.plan_mode_status.awaiting_approval = *awaiting_approval;
                    replay.plan_mode_status.plan_was_edited = *plan_was_edited;
                }
                TranscriptEvent::TokenBudgetWarning {
                    active_tokens,
                    effective_budget_tokens,
                    ..
                } => {
                    let mut state = replay
                        .token_budget_state
                        .clone()
                        .unwrap_or_else(|| TokenBudgetState::new(*effective_budget_tokens, 0));
                    state.warning_emitted = true;
                    state.blocked = false;
                    state.latest_exact_count = Some(*active_tokens);
                    state.latest_rough_count = *active_tokens;
                    state.effective_budget_tokens = *effective_budget_tokens;
                    state.context_window_tokens = *effective_budget_tokens;
                    replay.token_budget_state = Some(state);
                }
                TranscriptEvent::AutoCompactPerformed { after_tokens, .. } => {
                    let mut state = replay
                        .token_budget_state
                        .clone()
                        .unwrap_or_else(|| TokenBudgetState::new(*after_tokens, 0));
                    state.latest_exact_count = Some(*after_tokens);
                    state.latest_rough_count = *after_tokens;
                    state.blocked = false;
                    state.consecutive_autocompact_failures = 0;
                    replay.token_budget_state = Some(state);
                }
                TranscriptEvent::AutoCompactFailed {
                    consecutive_failures,
                    ..
                } => {
                    let mut state = replay
                        .token_budget_state
                        .clone()
                        .unwrap_or_else(|| TokenBudgetState::new(0, 0));
                    state.consecutive_autocompact_failures = *consecutive_failures;
                    replay.token_budget_state = Some(state);
                }
                TranscriptEvent::SessionCompacted {
                    history,
                    system_sections,
                    user_context_sections,
                    after_tokens,
                    ..
                } => {
                    replay.messages = history.clone();
                    replay.additional_system_sections = system_sections.clone();
                    replay.additional_user_context_sections = user_context_sections.clone();
                    let mut state = replay
                        .token_budget_state
                        .clone()
                        .unwrap_or_else(|| TokenBudgetState::new(*after_tokens, 0));
                    state.blocked = false;
                    state.warning_emitted = false;
                    state.latest_exact_count = Some(*after_tokens);
                    state.latest_rough_count = *after_tokens;
                    state.consecutive_autocompact_failures = 0;
                    replay.token_budget_state = Some(state);
                }
                TranscriptEvent::TokenBudgetBlocked {
                    active_tokens,
                    effective_budget_tokens,
                    ..
                } => {
                    let mut state = replay
                        .token_budget_state
                        .clone()
                        .unwrap_or_else(|| TokenBudgetState::new(*effective_budget_tokens, 0));
                    state.blocked = true;
                    state.warning_emitted = true;
                    state.latest_exact_count = Some(*active_tokens);
                    state.latest_rough_count = *active_tokens;
                    state.effective_budget_tokens = *effective_budget_tokens;
                    state.context_window_tokens = *effective_budget_tokens;
                    replay.token_budget_state = Some(state);
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
    use crate::api::ChatMessage;
    use crate::prompting::{
        PromptCacheScope, PromptSection, PromptSectionRole, PromptSectionSource,
    };

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

    #[tokio::test]
    async fn transcript_store_collects_unique_surfaced_memory_paths() {
        let temp = tempfile::tempdir().unwrap();
        let store = TranscriptStore::new(temp.path().to_path_buf());

        store
            .append(&TranscriptEvent::MemorySurfaced {
                paths: vec!["notes/auth.md".to_string(), "plans/release.md".to_string()],
            })
            .await
            .unwrap();
        store
            .append(&TranscriptEvent::MemorySurfaced {
                paths: vec![
                    "notes/auth.md".to_string(),
                    "runbooks/deploy.md".to_string(),
                ],
            })
            .await
            .unwrap();

        let paths = store.surfaced_memory_paths().await.unwrap();

        assert_eq!(
            paths,
            vec![
                "notes/auth.md".to_string(),
                "plans/release.md".to_string(),
                "runbooks/deploy.md".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn transcript_replay_restores_token_budget_warning_and_failures() {
        let temp = tempfile::tempdir().unwrap();
        let store = TranscriptStore::new(temp.path().to_path_buf());

        store
            .append(&TranscriptEvent::TokenBudgetWarning {
                active_tokens: 181_000,
                warning_threshold: 172_000,
                effective_budget_tokens: 192_000,
            })
            .await
            .unwrap();
        store
            .append(&TranscriptEvent::AutoCompactFailed {
                strategy: "full".to_string(),
                error_summary: "summary request failed".to_string(),
                consecutive_failures: 2,
            })
            .await
            .unwrap();

        let replay = store.replay().await.unwrap();
        let token_budget_state = replay.token_budget_state.expect("token budget state");

        assert!(token_budget_state.warning_emitted);
        assert_eq!(token_budget_state.active_count(), 181_000);
        assert_eq!(token_budget_state.consecutive_autocompact_failures, 2);
    }

    #[tokio::test]
    async fn transcript_replay_restores_session_compaction_state() {
        let temp = tempfile::tempdir().unwrap();
        let store = TranscriptStore::new(temp.path().to_path_buf());

        store
            .append(&TranscriptEvent::SessionCompacted {
                strategy: "full".to_string(),
                history: vec![ChatMessage::assistant("compacted answer")],
                system_sections: vec![PromptSection {
                    id: "compact_boundary".to_string(),
                    role: PromptSectionRole::System,
                    content: "boundary".to_string(),
                    cache_scope: PromptCacheScope::None,
                    is_dynamic: true,
                    source: PromptSectionSource::CompactBoundary,
                }],
                user_context_sections: vec![PromptSection {
                    id: "compact_summary".to_string(),
                    role: PromptSectionRole::User,
                    content: "summary".to_string(),
                    cache_scope: PromptCacheScope::None,
                    is_dynamic: true,
                    source: PromptSectionSource::CompactSummary,
                }],
                before_tokens: 9_000,
                after_tokens: 2_500,
                compacted_messages: 12,
                preserved_messages: 3,
            })
            .await
            .unwrap();

        let replay = store.replay().await.unwrap();

        assert_eq!(replay.messages.len(), 1);
        assert_eq!(replay.additional_system_sections.len(), 1);
        assert_eq!(replay.additional_user_context_sections.len(), 1);
        assert_eq!(
            replay
                .token_budget_state
                .expect("token budget state")
                .active_count(),
            2_500
        );
    }
}
