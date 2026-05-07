//! Shared agent runtime with tool execution support.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::StreamExt;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::time::timeout;

use crate::api::{
    ApiClient, ApiPromptCacheScope, ApiPromptCacheTextBlock, ChatMessage, PromptCacheMetadata,
    ToolCall, ToolCallFunction, ToolDefinition, Usage,
};
use crate::auto_mode::{
    auto_mode_tool_denial_payload, AutoModeClassifierRunMode, AutoModeClassifierStage,
    AutoModeConfig, AutoModeDecisionBehavior, AutoModeSession, AutoModeToolCall,
};
use crate::compact::{
    full_compact, full_compact_with_summary, micro_compact_history, session_memory_compact,
    CompactDirection, CompactResult, CompactStrategy,
};
use crate::config::Settings;
use crate::fast_path::{
    ExecutionModeHint, QuickPathExecution, QuickPathExecutor, QuickPathRequest,
};
use crate::plan_mode::{
    is_tool_visible_for_mode, AllowedPrompt, PlanMode, PlanModeSession, PlanModeStatus,
    EXIT_PLAN_MODE_TOOL,
};
use crate::prompting::{
    ProjectMemorySelectionQuery, ProjectMemorySelector, PromptAssembly, PromptBudget,
    PromptBuildRequest, PromptBuilder, PromptCacheScope, PromptSection,
};
use crate::streaming::{StreamSnapshot, StreamUpdate, StreamingAssembler};
use crate::token_budget::{
    effective_budget, evaluate_budget_decision, resolve_context_window, rough_count_messages,
    rough_count_tools, BudgetSource, TokenBudgetDecision, TokenBudgetState,
    DEFAULT_MAX_OUTPUT_TOKENS, POST_COMPACT_TOKEN_BUDGET,
};
use crate::tools::{ToolError, ToolOutput, ToolRegistry};

const DEFAULT_MAX_ITERATIONS: usize = 8;
const MAX_ALLOWED_ITERATIONS: usize = 24;
const MAX_TOOL_PAYLOAD_CHARS: usize = 12_000;
const STREAM_POLL_INTERVAL_MILLIS: u64 = 250;
const PROJECT_MEMORY_SELECTOR_MODEL: &str = "sonnet";
const PROJECT_MEMORY_SELECTOR_MAX_TOKENS: usize = 512;
const PROJECT_MEMORY_SELECTOR_MAX_PATHS: usize = 5;
const DEFAULT_PROMPT_RECENT_MESSAGE_COUNT: usize = 8;
const FULL_COMPACT_SUMMARIZER_MODEL: &str = "sonnet";
const FULL_COMPACT_SUMMARIZER_MAX_TOKENS: usize = 1_024;
const FULL_COMPACT_SUMMARIZER_MAX_MESSAGES: usize = 80;
const FULL_COMPACT_SUMMARIZER_MAX_MESSAGE_CHARS: usize = 1_200;

#[derive(Debug, Clone)]
pub struct AgentExecutionRequest {
    pub system_prompt: String,
    pub history: Vec<ChatMessage>,
    pub workspace_root: PathBuf,
    pub already_surfaced_memory_paths: Vec<String>,
    pub max_iterations: usize,
    pub execution_mode_hint: ExecutionModeHint,
    pub token_budget_state: Option<TokenBudgetState>,
    pub additional_system_sections: Vec<PromptSection>,
    pub additional_user_context_sections: Vec<PromptSection>,
    pub allowed_tool_names: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct AgentExecutionResult {
    pub reasoning: String,
    pub answer: String,
    pub iterations: usize,
    pub usage_records: Vec<AgentUsageRecord>,
    pub token_budget_state: Option<TokenBudgetState>,
}

#[derive(Debug, Clone)]
pub enum AgentExecutionOutcome {
    Completed(AgentExecutionResult),
    Cancelled,
}

#[derive(Debug, Clone)]
pub enum AgentEvent {
    ReasoningDelta {
        full_text: String,
        delta: String,
    },
    Reasoning {
        full_text: String,
        summary: String,
    },
    AnswerDelta {
        full_text: String,
        delta: String,
    },
    ToolCallRequested {
        tool_call_id: String,
        tool_name: String,
        input: Value,
        input_preview: String,
    },
    ToolCallCompleted {
        tool_call_id: String,
        tool_name: String,
        output: String,
        output_preview: String,
    },
    ToolCallFailed {
        tool_call_id: String,
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
    FinalAnswer {
        answer: String,
    },
}

#[async_trait]
pub trait AgentEventHandler: Send + Sync {
    async fn on_event(&self, _event: AgentEvent) {}
}

pub trait AgentCancellation: Send + Sync {
    fn is_cancelled(&self) -> bool {
        false
    }
}

#[async_trait]
pub trait AgentToolCallHook: Send + Sync {
    async fn before_tool_call(
        &self,
        _tool_call_id: &str,
        _tool_name: &str,
        _input: &Value,
    ) -> Result<()> {
        Ok(())
    }
}

pub struct NoopAgentEventHandler;

#[async_trait]
impl AgentEventHandler for NoopAgentEventHandler {}

pub struct NoopAgentCancellation;

impl AgentCancellation for NoopAgentCancellation {}

pub struct NoopAgentToolCallHook;

#[async_trait]
impl AgentToolCallHook for NoopAgentToolCallHook {}

pub struct AgentRuntime {
    client: ApiClient,
    tool_registry: ToolRegistry,
    max_response_tokens: usize,
    working_dir: PathBuf,
    memory_enabled: bool,
    auto_memory_directory: Option<PathBuf>,
    plan_mode_session: PlanModeSession,
    auto_mode_session: AutoModeSession,
}

#[derive(Debug, Clone, Default)]
struct TurnResult {
    reasoning: String,
    answer: String,
    tool_calls: Vec<ToolCall>,
    usage: Option<Usage>,
}

#[derive(Debug, Clone, Default)]
struct TurnPromptState {
    history: Vec<ChatMessage>,
    additional_system_sections: Vec<PromptSection>,
    additional_user_context_sections: Vec<PromptSection>,
}

#[derive(Debug, Clone)]
struct PreparedTurn {
    history_messages: Vec<ChatMessage>,
    rendered_messages: Vec<ChatMessage>,
    prompt_cache_metadata: Option<PromptCacheMetadata>,
}

#[derive(Debug, Clone, Default)]
pub struct AgentUsageRecord {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub total_tokens: usize,
    pub usage_missing: bool,
}

impl AgentRuntime {
    pub fn new(settings: Settings) -> Self {
        Self::new_with_tool_registry(settings, ToolRegistry::new())
    }

    pub fn new_with_tool_registry(settings: Settings, mut tool_registry: ToolRegistry) -> Self {
        let max_response_tokens = settings.api.max_tokens;
        let working_dir = settings.working_dir.clone();
        let memory_enabled = settings.memory.enabled;
        let auto_memory_directory = settings.memory.auto_memory_directory.clone();
        let plan_mode_session = PlanModeSession::new(working_dir.clone());
        let auto_mode_session = AutoModeSession::new(
            working_dir.clone(),
            settings.model.clone(),
            auto_mode_config_from_settings(&settings),
        );
        tool_registry.register_plan_mode_tools(plan_mode_session.clone());
        Self {
            client: ApiClient::new(settings),
            tool_registry,
            max_response_tokens,
            working_dir,
            memory_enabled,
            auto_memory_directory,
            plan_mode_session,
            auto_mode_session,
        }
    }

    pub async fn execute(
        &self,
        request: AgentExecutionRequest,
        event_handler: &dyn AgentEventHandler,
        cancellation: &dyn AgentCancellation,
    ) -> Result<AgentExecutionOutcome> {
        self.execute_with_hook(request, event_handler, cancellation, &NoopAgentToolCallHook)
            .await
    }

    pub async fn execute_with_hook(
        &self,
        request: AgentExecutionRequest,
        event_handler: &dyn AgentEventHandler,
        cancellation: &dyn AgentCancellation,
        tool_call_hook: &dyn AgentToolCallHook,
    ) -> Result<AgentExecutionOutcome> {
        let AgentExecutionRequest {
            system_prompt,
            history,
            workspace_root,
            mut already_surfaced_memory_paths,
            max_iterations: requested_max_iterations,
            execution_mode_hint,
            token_budget_state,
            additional_system_sections,
            additional_user_context_sections,
            allowed_tool_names,
        } = request;
        self.plan_mode_session
            .set_workspace_root(workspace_root.clone())
            .await;
        self.auto_mode_session
            .set_workspace_root(workspace_root.clone())
            .await;
        let auto_mode_status = self.auto_mode_session.status().await;
        if auto_mode_status.active {
            event_handler
                .on_event(AgentEvent::AutoModeEntered {
                    previous_mode: auto_mode_status
                        .previous_mode
                        .clone()
                        .unwrap_or_else(|| "default".to_string()),
                    model: auto_mode_status.model.clone(),
                    stripped_dangerous_rules: auto_mode_status.stripped_dangerous_rules.clone(),
                })
                .await;
        }
        let mut latest_reasoning = String::new();
        let mut usage_records = Vec::new();
        let max_iterations = if requested_max_iterations == 0 {
            DEFAULT_MAX_ITERATIONS
        } else {
            requested_max_iterations.min(MAX_ALLOWED_ITERATIONS)
        };
        let allowed_tool_names = allowed_tool_names.map(|tools| {
            tools
                .into_iter()
                .map(|tool| tool.to_ascii_lowercase())
                .collect::<HashSet<_>>()
        });
        let memory_selector = ApiProjectMemorySelector {
            client: &self.client,
        };
        let mut prompt_state = TurnPromptState {
            history,
            additional_system_sections,
            additional_user_context_sections,
        };
        let mut token_budget_state = token_budget_state.unwrap_or_else(|| {
            let context_window_tokens = self.context_window_tokens();
            TokenBudgetState::new(context_window_tokens, self.max_response_tokens)
        });
        let quick_path_executor = QuickPathExecutor::new(&self.client, &self.tool_registry);

        let auto_mode_active_for_run = auto_mode_status.active;
        if !auto_mode_active_for_run
            && !matches!(
                execution_mode_hint,
                ExecutionModeHint::ForceSlow | ExecutionModeHint::PreferSlow
            )
        {
            let quick_path_outcome = quick_path_executor
                .execute(
                    QuickPathRequest {
                        system_prompt: &system_prompt,
                        hint: execution_mode_hint,
                        history: &prompt_state.history,
                        workspace_root: &workspace_root,
                        has_additional_context_sections: !prompt_state
                            .additional_system_sections
                            .is_empty()
                            || !prompt_state.additional_user_context_sections.is_empty(),
                    },
                    event_handler,
                    cancellation,
                )
                .await?;

            match quick_path_outcome {
                QuickPathExecution::Completed {
                    answer,
                    usage_records: quick_usage_records,
                } => {
                    usage_records.extend(
                        quick_usage_records
                            .into_iter()
                            .map(|usage| turn_usage_record(usage.as_ref())),
                    );
                    event_handler
                        .on_event(AgentEvent::FinalAnswer {
                            answer: answer.clone(),
                        })
                        .await;
                    return Ok(AgentExecutionOutcome::Completed(AgentExecutionResult {
                        reasoning: String::new(),
                        answer,
                        iterations: 1,
                        usage_records,
                        token_budget_state: Some(token_budget_state.clone()),
                    }));
                }
                QuickPathExecution::Downgraded {
                    appended_history,
                    usage_records: quick_usage_records,
                    ..
                } => {
                    usage_records.extend(
                        quick_usage_records
                            .into_iter()
                            .map(|usage| turn_usage_record(usage.as_ref())),
                    );
                    prompt_state.history.extend(appended_history);
                }
                QuickPathExecution::Skipped {
                    usage_records: quick_usage_records,
                    ..
                } => {
                    usage_records.extend(
                        quick_usage_records
                            .into_iter()
                            .map(|usage| turn_usage_record(usage.as_ref())),
                    );
                }
                QuickPathExecution::Cancelled => {
                    return Ok(AgentExecutionOutcome::Cancelled);
                }
            }
        }

        for iteration in 0..max_iterations {
            if cancellation.is_cancelled() {
                return Ok(AgentExecutionOutcome::Cancelled);
            }

            let plan_mode_status = self.plan_mode_session.status().await;
            let tool_definitions =
                self.tool_definitions(allowed_tool_names.as_ref(), &plan_mode_status);
            let auto_system_prompt = self
                .auto_mode_session
                .decorated_system_prompt(&system_prompt)
                .await;
            let turn_system_prompt = self
                .plan_mode_session
                .decorated_system_prompt(&auto_system_prompt)
                .await;
            let prepared_turn = self
                .prepare_turn_context(
                    &turn_system_prompt,
                    &workspace_root,
                    &tool_definitions,
                    &mut prompt_state,
                    &mut already_surfaced_memory_paths,
                    &mut token_budget_state,
                    &memory_selector,
                    event_handler,
                )
                .await?;

            let turn_result = if self.client.streaming_enabled() {
                match self
                    .execute_stream_turn(
                        &prepared_turn.history_messages,
                        &prepared_turn.rendered_messages,
                        prepared_turn.prompt_cache_metadata.as_ref(),
                        &tool_definitions,
                        event_handler,
                        cancellation,
                    )
                    .await
                {
                    Ok(Some(result)) => result,
                    Ok(None) => return Ok(AgentExecutionOutcome::Cancelled),
                    Err(error) if is_prompt_too_long_error(&error) => {
                        let compact_result = self
                            .perform_full_compact(
                                &prompt_state.history,
                                CompactDirection::UpTo,
                                None,
                                DEFAULT_PROMPT_RECENT_MESSAGE_COUNT,
                            )
                            .await;
                        event_handler
                            .on_event(AgentEvent::AutoCompactPerformed {
                                strategy: "prompt_too_long_retry".to_string(),
                                before_tokens: compact_result.before_tokens,
                                after_tokens: compact_result.after_tokens,
                                compacted_messages: compact_result.compacted_message_count,
                                preserved_messages: compact_result.preserved_message_count,
                            })
                            .await;
                        emit_session_compacted_event(event_handler, &compact_result).await;
                        prompt_state.history = compact_result.history;
                        prompt_state.additional_system_sections = compact_result.system_sections;
                        prompt_state.additional_user_context_sections =
                            compact_result.user_context_sections;
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            } else {
                match self
                    .execute_non_stream_turn(
                        &prepared_turn.history_messages,
                        &prepared_turn.rendered_messages,
                        prepared_turn.prompt_cache_metadata.as_ref(),
                        &tool_definitions,
                        event_handler,
                    )
                    .await
                {
                    Ok(result) => result,
                    Err(error) if is_prompt_too_long_error(&error) => {
                        let compact_result = self
                            .perform_full_compact(
                                &prompt_state.history,
                                CompactDirection::UpTo,
                                None,
                                DEFAULT_PROMPT_RECENT_MESSAGE_COUNT,
                            )
                            .await;
                        event_handler
                            .on_event(AgentEvent::AutoCompactPerformed {
                                strategy: "prompt_too_long_retry".to_string(),
                                before_tokens: compact_result.before_tokens,
                                after_tokens: compact_result.after_tokens,
                                compacted_messages: compact_result.compacted_message_count,
                                preserved_messages: compact_result.preserved_message_count,
                            })
                            .await;
                        emit_session_compacted_event(event_handler, &compact_result).await;
                        prompt_state.history = compact_result.history;
                        prompt_state.additional_system_sections = compact_result.system_sections;
                        prompt_state.additional_user_context_sections =
                            compact_result.user_context_sections;
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            };

            if !turn_result.reasoning.trim().is_empty() {
                latest_reasoning = turn_result.reasoning.clone();
            }

            usage_records.push(turn_usage_record(turn_result.usage.as_ref()));

            let tool_calls = turn_result.tool_calls;
            if !tool_calls.is_empty() {
                prompt_state
                    .history
                    .push(ChatMessage::assistant_with_tools(tool_calls.clone()));

                for tool_call in tool_calls {
                    if cancellation.is_cancelled() {
                        return Ok(AgentExecutionOutcome::Cancelled);
                    }

                    let tool_name = tool_call.function.name.clone();
                    let plan_mode_status = self.plan_mode_session.status().await;
                    if !self.tool_is_allowed_for_status(
                        &tool_name,
                        allowed_tool_names.as_ref(),
                        &plan_mode_status,
                    ) {
                        let error_payload = json!({
                            "success": false,
                            "error": format!("Tool not allowed in the current agent safety mode: {}", tool_name),
                            "code": if plan_mode_status.mode == PlanMode::Plan {
                                "plan_mode_tool_not_allowed"
                            } else {
                                "tool_not_allowed"
                            },
                        });
                        prompt_state.history.push(ChatMessage::tool(
                            tool_call.id.clone(),
                            error_payload.to_string(),
                        ));
                        event_handler
                            .on_event(AgentEvent::ToolCallFailed {
                                tool_call_id: tool_call.id.clone(),
                                tool_name,
                                error_summary: "tool_not_allowed".to_string(),
                            })
                            .await;
                        continue;
                    }
                    let parsed_arguments = parse_tool_arguments(&tool_call.function.arguments)?;
                    let normalized_arguments =
                        normalize_tool_input(&tool_name, parsed_arguments, &workspace_root);

                    if self.auto_mode_session.is_active().await {
                        let access = self
                            .tool_registry
                            .get(&tool_name)
                            .map(|tool| tool.access())
                            .unwrap_or(crate::tools::ToolAccess::Write);
                        let decision = self
                            .auto_mode_session
                            .classify_tool_call(AutoModeToolCall::new(
                                tool_name.clone(),
                                normalized_arguments.clone(),
                                access,
                                prompt_state.history.clone(),
                            ))
                            .await;
                        event_handler
                            .on_event(AgentEvent::AutoModeDecisionRecorded {
                                tool_name: tool_name.clone(),
                                behavior: decision.behavior,
                                reason: decision.reason.clone(),
                                stage: decision.stage,
                                unavailable: decision.unavailable,
                                transcript_too_long: decision.transcript_too_long,
                            })
                            .await;

                        if decision.behavior != AutoModeDecisionBehavior::Allow {
                            let error_payload =
                                auto_mode_tool_denial_payload(&tool_name, &decision);
                            prompt_state.history.push(ChatMessage::tool(
                                tool_call.id.clone(),
                                error_payload.to_string(),
                            ));
                            event_handler
                                .on_event(AgentEvent::ToolCallFailed {
                                    tool_call_id: tool_call.id.clone(),
                                    tool_name,
                                    error_summary: decision.reason,
                                })
                                .await;
                            continue;
                        }
                    }

                    tool_call_hook
                        .before_tool_call(&tool_call.id, &tool_name, &normalized_arguments)
                        .await?;

                    event_handler
                        .on_event(AgentEvent::ToolCallRequested {
                            tool_call_id: tool_call.id.clone(),
                            tool_name: tool_name.clone(),
                            input: normalized_arguments.clone(),
                            input_preview: summarize_text(&normalized_arguments.to_string(), 280),
                        })
                        .await;

                    match self
                        .tool_registry
                        .execute(&tool_name, normalized_arguments)
                        .await
                    {
                        Ok(output) => {
                            let tool_content = tool_output_message(&output);
                            let stop_for_plan_approval =
                                should_stop_for_plan_approval(&tool_name, &output);
                            self.emit_plan_mode_event_if_needed(&tool_name, &output, event_handler)
                                .await;
                            prompt_state.history.push(ChatMessage::tool(
                                tool_call.id.clone(),
                                tool_content.clone(),
                            ));
                            event_handler
                                .on_event(AgentEvent::ToolCallCompleted {
                                    tool_call_id: tool_call.id.clone(),
                                    tool_name,
                                    output: tool_content.clone(),
                                    output_preview: summarize_text(&tool_content, 400),
                                })
                                .await;
                            if stop_for_plan_approval {
                                let answer = plan_approval_answer(&output);
                                event_handler
                                    .on_event(AgentEvent::FinalAnswer {
                                        answer: answer.clone(),
                                    })
                                    .await;
                                return Ok(AgentExecutionOutcome::Completed(
                                    AgentExecutionResult {
                                        reasoning: latest_reasoning,
                                        answer,
                                        iterations: iteration + 1,
                                        usage_records,
                                        token_budget_state: Some(token_budget_state.clone()),
                                    },
                                ));
                            }
                        }
                        Err(error) => {
                            let error_payload = json!({
                                "success": false,
                                "error": error.message,
                                "code": error.code,
                            });
                            prompt_state.history.push(ChatMessage::tool(
                                tool_call.id.clone(),
                                error_payload.to_string(),
                            ));
                            event_handler
                                .on_event(AgentEvent::ToolCallFailed {
                                    tool_call_id: tool_call.id.clone(),
                                    tool_name,
                                    error_summary: format_tool_error(&error),
                                })
                                .await;
                        }
                    }
                }

                continue;
            }

            let answer = turn_result.answer.trim().to_string();
            let final_answer = if answer.is_empty() {
                "Provider returned an empty answer body.".to_string()
            } else {
                answer
            };
            prompt_state
                .history
                .push(ChatMessage::assistant(final_answer.clone()));
            event_handler
                .on_event(AgentEvent::FinalAnswer {
                    answer: final_answer.clone(),
                })
                .await;

            return Ok(AgentExecutionOutcome::Completed(AgentExecutionResult {
                reasoning: latest_reasoning,
                answer: final_answer,
                iterations: iteration + 1,
                usage_records,
                token_budget_state: Some(token_budget_state.clone()),
            }));
        }

        Err(anyhow!(
            "Agent loop reached the step limit ({} iterations).",
            max_iterations
        ))
    }

    fn tool_definitions(
        &self,
        allowed_tool_names: Option<&HashSet<String>>,
        plan_mode_status: &PlanModeStatus,
    ) -> Vec<ToolDefinition> {
        self.tool_registry
            .list()
            .into_iter()
            .filter(|tool| tool_is_allowed(tool.name(), allowed_tool_names))
            .filter(|tool| is_tool_visible_for_mode(tool.name(), tool.access(), plan_mode_status))
            .map(|tool| ToolDefinition::new(tool.name(), tool.description(), tool.input_schema()))
            .collect()
    }

    fn tool_is_allowed_for_status(
        &self,
        tool_name: &str,
        allowed_tool_names: Option<&HashSet<String>>,
        plan_mode_status: &PlanModeStatus,
    ) -> bool {
        if !tool_is_allowed(tool_name, allowed_tool_names) {
            return false;
        }

        let access = self
            .tool_registry
            .get(tool_name)
            .map(|tool| tool.access())
            .unwrap_or(crate::tools::ToolAccess::Write);
        is_tool_visible_for_mode(tool_name, access, plan_mode_status)
    }

    async fn emit_plan_mode_event_if_needed(
        &self,
        tool_name: &str,
        output: &ToolOutput,
        event_handler: &dyn AgentEventHandler,
    ) {
        let Some(action) = output
            .metadata
            .get("plan_mode_action")
            .and_then(Value::as_str)
        else {
            return;
        };

        match action {
            "entered" => {
                let previous_mode = output
                    .metadata
                    .get("previous_mode")
                    .and_then(Value::as_str)
                    .unwrap_or("default")
                    .to_string();
                event_handler
                    .on_event(AgentEvent::PlanModeEntered { previous_mode })
                    .await;
            }
            "exited" if tool_name == EXIT_PLAN_MODE_TOOL => {
                let plan_file_path = output
                    .metadata
                    .get("plan_file_path")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let awaiting_approval = output
                    .metadata
                    .get("awaiting_approval")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let plan_was_edited = output
                    .metadata
                    .get("plan_was_edited")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let allowed_prompts = output
                    .metadata
                    .get("allowed_prompts")
                    .and_then(|value| serde_json::from_value(value.clone()).ok())
                    .unwrap_or_default();
                event_handler
                    .on_event(AgentEvent::PlanModeExited {
                        plan_file_path,
                        allowed_prompts,
                        awaiting_approval,
                        plan_was_edited,
                    })
                    .await;
            }
            _ => {}
        }
    }

    fn context_window_tokens(&self) -> usize {
        resolve_context_window(
            self.client.get_model(),
            self.client.provider_kind(),
            self.client.beta_headers(),
        )
    }

    fn prompt_budget(&self, has_compaction_sections: bool) -> PromptBudget {
        let total_input_tokens = if has_compaction_sections {
            self.context_window_tokens().min(POST_COMPACT_TOKEN_BUDGET)
        } else {
            self.context_window_tokens()
        };

        PromptBudget {
            total_input_tokens,
            reserved_output_tokens: self.max_response_tokens.max(DEFAULT_MAX_OUTPUT_TOKENS),
            recent_message_count: DEFAULT_PROMPT_RECENT_MESSAGE_COUNT,
        }
    }

    async fn choose_compaction(
        &self,
        history: &[ChatMessage],
        recent_message_count: usize,
    ) -> CompactResult {
        let best = best_compaction_candidate(history, recent_message_count);
        if matches!(
            best.strategy,
            CompactStrategy::Full | CompactStrategy::PartialUpTo | CompactStrategy::PartialFrom
        ) {
            self.perform_full_compact(history, CompactDirection::UpTo, None, recent_message_count)
                .await
        } else {
            best
        }
    }

    async fn perform_full_compact(
        &self,
        history: &[ChatMessage],
        direction: CompactDirection,
        anchor_index: Option<usize>,
        preserve_recent_messages: usize,
    ) -> CompactResult {
        match self
            .request_full_compaction_summary(
                history,
                direction,
                anchor_index,
                preserve_recent_messages,
            )
            .await
        {
            Ok(summary) => full_compact_with_summary(
                history,
                direction,
                anchor_index,
                preserve_recent_messages,
                Some(summary.as_str()),
            ),
            Err(_) => full_compact(history, direction, anchor_index, preserve_recent_messages),
        }
    }

    async fn request_full_compaction_summary(
        &self,
        history: &[ChatMessage],
        direction: CompactDirection,
        anchor_index: Option<usize>,
        preserve_recent_messages: usize,
    ) -> Result<String> {
        let payload = build_full_compaction_summary_payload(
            history,
            direction,
            anchor_index,
            preserve_recent_messages,
        );
        let response = self
            .client
            .chat_with_overrides(
                build_full_compaction_summary_messages(payload),
                None,
                Some(FULL_COMPACT_SUMMARIZER_MODEL),
                Some(FULL_COMPACT_SUMMARIZER_MAX_TOKENS),
                Some(0.0),
            )
            .await?;
        let summary = response
            .choices
            .first()
            .and_then(|choice| choice.message.content.as_deref())
            .map(str::trim)
            .filter(|content| !content.is_empty())
            .ok_or_else(|| anyhow!("Full compact summarizer returned no message content"))?;

        Ok(summary.to_string())
    }

    async fn prepare_turn_context(
        &self,
        system_prompt: &str,
        workspace_root: &Path,
        tool_definitions: &[ToolDefinition],
        prompt_state: &mut TurnPromptState,
        already_surfaced_memory_paths: &mut Vec<String>,
        token_budget_state: &mut TokenBudgetState,
        memory_selector: &dyn ProjectMemorySelector,
        event_handler: &dyn AgentEventHandler,
    ) -> Result<PreparedTurn> {
        let auto_compact_enabled = auto_compact_enabled();
        let mut working_state = prompt_state.clone();
        let mut performed_compaction = false;

        loop {
            let budget = self.prompt_budget(
                !working_state.additional_system_sections.is_empty()
                    || !working_state.additional_user_context_sections.is_empty(),
            );
            token_budget_state.context_window_tokens = budget.total_input_tokens;
            token_budget_state.effective_budget_tokens =
                effective_budget(budget.total_input_tokens, budget.reserved_output_tokens);

            let prompt_request = PromptBuildRequest {
                base_system_prompt: system_prompt.to_string(),
                history: working_state.history.clone(),
                workspace_root: workspace_root.to_path_buf(),
                current_working_dir: Some(self.working_dir.clone()),
                tool_definitions: tool_definitions.to_vec(),
                budget,
                entrypoint: "rust-agent-runtime".to_string(),
                version_fingerprint: None,
                global_config_root: None,
                memory_enabled: self.memory_enabled,
                auto_memory_directory: self.auto_memory_directory.clone(),
                already_surfaced_memory_paths: already_surfaced_memory_paths.clone(),
                additional_system_sections: working_state.additional_system_sections.clone(),
                additional_user_context_sections: working_state
                    .additional_user_context_sections
                    .clone(),
            };
            let prompt_assembly =
                PromptBuilder::build_with_selector(prompt_request, Some(memory_selector)).await?;
            let surfaced_now = record_new_memory_paths(
                already_surfaced_memory_paths,
                prompt_assembly.surfaced_memory_paths.clone(),
            );
            if !surfaced_now.is_empty() {
                event_handler
                    .on_event(AgentEvent::MemorySurfaced {
                        paths: surfaced_now,
                    })
                    .await;
            }

            let rendered = prompt_assembly.render();
            let prompt_cache_metadata = prompt_cache_metadata_from_assembly(&prompt_assembly);
            let rough_count =
                rough_count_messages(&rendered.messages) + rough_count_tools(tool_definitions);
            let exact_count = self
                .client
                .count_tokens_with_metadata(
                    prompt_assembly.history_messages.clone(),
                    Some(tool_definitions.to_vec()),
                    None,
                    prompt_cache_metadata.as_ref(),
                )
                .await?;
            token_budget_state.latest_rough_count = rough_count;
            token_budget_state.latest_exact_count = exact_count;
            token_budget_state.blocked = false;

            let source = if performed_compaction {
                BudgetSource::Compact
            } else {
                BudgetSource::Normal
            };
            let decision =
                evaluate_budget_decision(token_budget_state, auto_compact_enabled, source);
            match decision {
                TokenBudgetDecision::Proceed => {
                    *prompt_state = working_state;
                    return Ok(PreparedTurn {
                        history_messages: prompt_assembly.history_messages,
                        rendered_messages: rendered.messages,
                        prompt_cache_metadata,
                    });
                }
                TokenBudgetDecision::Warn => {
                    emit_warning_event(token_budget_state, event_handler).await;
                    token_budget_state.warning_emitted = true;
                    *prompt_state = working_state;
                    return Ok(PreparedTurn {
                        history_messages: prompt_assembly.history_messages,
                        rendered_messages: rendered.messages,
                        prompt_cache_metadata,
                    });
                }
                TokenBudgetDecision::Block => {
                    token_budget_state.blocked = true;
                    emit_block_event(token_budget_state, event_handler).await;
                    return Err(anyhow!(
                        "Prompt token budget exceeded: {} tokens active against a {} token budget.",
                        token_budget_state.active_count(),
                        token_budget_state.effective_budget_tokens,
                    ));
                }
                TokenBudgetDecision::AutoCompact => {
                    let compact_result = self
                        .choose_compaction(&working_state.history, budget.recent_message_count)
                        .await;
                    if compact_result.after_tokens >= compact_result.before_tokens
                        || compact_result.compacted_message_count == 0
                    {
                        token_budget_state.consecutive_autocompact_failures += 1;
                        event_handler
                            .on_event(AgentEvent::AutoCompactFailed {
                                strategy: compact_strategy_label(compact_result.strategy)
                                    .to_string(),
                                error_summary: "Compaction did not reduce the prompt budget."
                                    .to_string(),
                                consecutive_failures: token_budget_state
                                    .consecutive_autocompact_failures,
                            })
                            .await;
                        return Err(anyhow!("Auto-compact could not reduce the prompt budget."));
                    }

                    emit_session_compacted_event(event_handler, &compact_result).await;
                    working_state.history = compact_result.history;
                    if !compact_result.system_sections.is_empty() {
                        working_state.additional_system_sections = compact_result.system_sections;
                    }
                    if !compact_result.user_context_sections.is_empty() {
                        working_state.additional_user_context_sections =
                            compact_result.user_context_sections;
                    }
                    token_budget_state.consecutive_autocompact_failures = 0;
                    performed_compaction = true;
                    event_handler
                        .on_event(AgentEvent::AutoCompactPerformed {
                            strategy: compact_strategy_label(compact_result.strategy).to_string(),
                            before_tokens: compact_result.before_tokens,
                            after_tokens: compact_result.after_tokens,
                            compacted_messages: compact_result.compacted_message_count,
                            preserved_messages: compact_result.preserved_message_count,
                        })
                        .await;
                }
            }
        }
    }

    async fn execute_non_stream_turn(
        &self,
        history_messages: &[ChatMessage],
        rendered_messages: &[ChatMessage],
        prompt_cache_metadata: Option<&PromptCacheMetadata>,
        tool_definitions: &[ToolDefinition],
        event_handler: &dyn AgentEventHandler,
    ) -> Result<TurnResult> {
        let request_messages =
            if self.client.provider_kind() == crate::token_budget::ProviderKind::AnthropicNative {
                history_messages.to_vec()
            } else {
                rendered_messages.to_vec()
            };
        let response = self
            .client
            .chat_with_slot_strategy_and_metadata(
                request_messages,
                Some(tool_definitions.to_vec()),
                None,
                None,
                prompt_cache_metadata,
            )
            .await?;

        let choice = response
            .response
            .choices
            .first()
            .cloned()
            .ok_or_else(|| anyhow!("API response did not include a choice"))?;

        let reasoning = choice.message.reasoning_content.clone().unwrap_or_default();
        if !reasoning.trim().is_empty() {
            event_handler
                .on_event(AgentEvent::Reasoning {
                    summary: summarize_text(&reasoning, 400),
                    full_text: reasoning.clone(),
                })
                .await;
        }

        Ok(TurnResult {
            reasoning,
            answer: choice
                .message
                .content
                .clone()
                .unwrap_or_default()
                .trim()
                .to_string(),
            tool_calls: choice.message.tool_calls.clone().unwrap_or_default(),
            usage: response.response.usage.clone(),
        })
    }

    async fn execute_stream_turn(
        &self,
        history_messages: &[ChatMessage],
        rendered_messages: &[ChatMessage],
        prompt_cache_metadata: Option<&PromptCacheMetadata>,
        tool_definitions: &[ToolDefinition],
        event_handler: &dyn AgentEventHandler,
        cancellation: &dyn AgentCancellation,
    ) -> Result<Option<TurnResult>> {
        let request_messages =
            if self.client.provider_kind() == crate::token_budget::ProviderKind::AnthropicNative {
                history_messages.to_vec()
            } else {
                rendered_messages.to_vec()
            };
        let response = self
            .client
            .chat_stream_with_metadata(
                request_messages,
                Some(tool_definitions.to_vec()),
                prompt_cache_metadata,
            )
            .await?;
        let mut byte_stream = response.bytes_stream();
        let mut assembler = StreamingAssembler::new();
        let mut stalled_for = Duration::ZERO;
        let poll_interval = Duration::from_millis(STREAM_POLL_INTERVAL_MILLIS);
        let stall_timeout = Duration::from_secs(self.client.timeout_seconds().clamp(5, 30));

        loop {
            match timeout(poll_interval, byte_stream.next()).await {
                Ok(Some(Ok(bytes))) => {
                    stalled_for = Duration::ZERO;
                    for update in assembler.push_bytes(&bytes)? {
                        match update {
                            StreamUpdate::ReasoningDelta { full_text, delta } => {
                                event_handler
                                    .on_event(AgentEvent::ReasoningDelta { full_text, delta })
                                    .await;
                            }
                            StreamUpdate::AnswerDelta { full_text, delta } => {
                                event_handler
                                    .on_event(AgentEvent::AnswerDelta { full_text, delta })
                                    .await;
                            }
                            StreamUpdate::ToolCallDelta { .. } | StreamUpdate::Finished { .. } => {}
                        }
                    }
                }
                Ok(Some(Err(error))) => {
                    return Err(anyhow!("Streaming response error: {}", error));
                }
                Ok(None) => break,
                Err(_) => {
                    if cancellation.is_cancelled() {
                        return Ok(None);
                    }

                    stalled_for += poll_interval;
                    if stalled_for >= stall_timeout {
                        return Err(anyhow!(
                            "Streaming response stalled for more than {} second(s).",
                            stall_timeout.as_secs()
                        ));
                    }
                }
            }

            if cancellation.is_cancelled() {
                return Ok(None);
            }
        }

        let snapshot = assembler.snapshot();
        if !snapshot.reasoning_text.trim().is_empty() {
            event_handler
                .on_event(AgentEvent::Reasoning {
                    summary: summarize_text(&snapshot.reasoning_text, 400),
                    full_text: snapshot.reasoning_text.clone(),
                })
                .await;
        }

        Ok(Some(stream_snapshot_into_turn_result(snapshot)?))
    }
}

#[derive(Debug, Clone, Serialize)]
struct FullCompactSummaryPayload {
    direction: String,
    anchor_index: Option<usize>,
    preserve_recent_messages: usize,
    compacted_message_count: usize,
    preserved_message_count: usize,
    compacted_messages: Vec<Value>,
    boundary_context: Vec<Value>,
}

fn best_compaction_candidate(
    history: &[ChatMessage],
    recent_message_count: usize,
) -> CompactResult {
    let candidates = [
        micro_compact_history(history, recent_message_count),
        session_memory_compact(history, recent_message_count),
        full_compact(history, CompactDirection::UpTo, None, recent_message_count),
    ];

    candidates
        .into_iter()
        .max_by_key(|result| result.before_tokens.saturating_sub(result.after_tokens))
        .unwrap_or_else(|| micro_compact_history(history, recent_message_count))
}

fn build_full_compaction_summary_payload(
    history: &[ChatMessage],
    direction: CompactDirection,
    anchor_index: Option<usize>,
    preserve_recent_messages: usize,
) -> FullCompactSummaryPayload {
    let split_index = compact_split_index(
        history.len(),
        direction,
        anchor_index,
        preserve_recent_messages,
    );
    let (compacted_slice, preserved_slice) = match direction {
        CompactDirection::UpTo => (&history[..split_index], &history[split_index..]),
        CompactDirection::From => (&history[split_index..], &history[..split_index]),
    };
    let boundary_start = split_index.saturating_sub(3);
    let boundary_end = history.len().min(split_index.saturating_add(3));
    let boundary_context = history[boundary_start..boundary_end]
        .iter()
        .map(serialize_compaction_message)
        .collect::<Vec<_>>();

    FullCompactSummaryPayload {
        direction: match direction {
            CompactDirection::UpTo => "up_to".to_string(),
            CompactDirection::From => "from".to_string(),
        },
        anchor_index,
        preserve_recent_messages,
        compacted_message_count: compacted_slice.len(),
        preserved_message_count: preserved_slice.len(),
        compacted_messages: compacted_slice
            .iter()
            .take(FULL_COMPACT_SUMMARIZER_MAX_MESSAGES)
            .map(serialize_compaction_message)
            .collect(),
        boundary_context,
    }
}

fn compact_split_index(
    history_len: usize,
    direction: CompactDirection,
    anchor_index: Option<usize>,
    preserve_recent_messages: usize,
) -> usize {
    match (direction, anchor_index) {
        (CompactDirection::UpTo, Some(anchor)) => anchor.min(history_len),
        (CompactDirection::From, Some(anchor)) => anchor.min(history_len),
        (CompactDirection::UpTo, None) => history_len.saturating_sub(preserve_recent_messages),
        (CompactDirection::From, None) => preserve_recent_messages.min(history_len),
    }
}

fn serialize_compaction_message(message: &ChatMessage) -> Value {
    let mut value = json!({
        "role": message.role,
    });
    if let Some(content) = &message.content {
        value["content"] = Value::String(summarize_text(
            content,
            FULL_COMPACT_SUMMARIZER_MAX_MESSAGE_CHARS,
        ));
    }
    if let Some(reasoning) = &message.reasoning_content {
        value["reasoning"] = Value::String(summarize_text(
            reasoning,
            FULL_COMPACT_SUMMARIZER_MAX_MESSAGE_CHARS,
        ));
    }
    if let Some(tool_calls) = &message.tool_calls {
        value["tool_calls"] = Value::Array(
            tool_calls
                .iter()
                .map(|tool_call| {
                    json!({
                        "id": tool_call.id,
                        "name": tool_call.function.name,
                        "arguments": summarize_text(
                            &tool_call.function.arguments,
                            FULL_COMPACT_SUMMARIZER_MAX_MESSAGE_CHARS,
                        ),
                    })
                })
                .collect(),
        );
    }
    if let Some(tool_call_id) = &message.tool_call_id {
        value["tool_call_id"] = Value::String(tool_call_id.clone());
    }
    value
}

fn build_full_compaction_summary_messages(payload: FullCompactSummaryPayload) -> Vec<ChatMessage> {
    let request = json!({
        "task": "Summarize a compacted conversation segment for another coding agent.",
        "constraints": {
            "return_format": "markdown_only",
            "preserve_file_paths": true,
            "preserve_commands": true,
            "preserve_open_questions": true,
            "preserve_tool_results": true,
            "no_code_fences": true,
        },
        "segment": payload,
    });

    vec![
        ChatMessage::system(
            "You compress conversation history for another coding agent. Return concise markdown only. Capture goals, confirmed facts, file paths, commands, tool outcomes, blockers, and unresolved questions. Do not invent details.",
        ),
        ChatMessage::user(
            serde_json::to_string_pretty(&request).unwrap_or_else(|_| request.to_string()),
        ),
    ]
}

async fn emit_session_compacted_event(
    event_handler: &dyn AgentEventHandler,
    compact_result: &CompactResult,
) {
    event_handler
        .on_event(AgentEvent::SessionCompacted {
            strategy: compact_strategy_label(compact_result.strategy).to_string(),
            history: compact_result.history.clone(),
            system_sections: compact_result.system_sections.clone(),
            user_context_sections: compact_result.user_context_sections.clone(),
            before_tokens: compact_result.before_tokens,
            after_tokens: compact_result.after_tokens,
            compacted_messages: compact_result.compacted_message_count,
            preserved_messages: compact_result.preserved_message_count,
        })
        .await;
}

fn record_new_memory_paths(
    existing_paths: &mut Vec<String>,
    surfaced_paths: Vec<String>,
) -> Vec<String> {
    let mut new_paths = Vec::new();
    for path in surfaced_paths {
        if existing_paths.contains(&path) {
            continue;
        }
        existing_paths.push(path.clone());
        new_paths.push(path);
    }
    new_paths
}

async fn emit_warning_event(
    token_budget_state: &TokenBudgetState,
    event_handler: &dyn AgentEventHandler,
) {
    let thresholds = token_budget_state.thresholds();
    event_handler
        .on_event(AgentEvent::TokenBudgetWarning {
            active_tokens: token_budget_state.active_count(),
            warning_threshold: thresholds.warning_tokens,
            effective_budget_tokens: token_budget_state.effective_budget_tokens,
        })
        .await;
}

async fn emit_block_event(
    token_budget_state: &TokenBudgetState,
    event_handler: &dyn AgentEventHandler,
) {
    let thresholds = token_budget_state.thresholds();
    event_handler
        .on_event(AgentEvent::TokenBudgetBlocked {
            active_tokens: token_budget_state.active_count(),
            blocking_threshold: thresholds.blocking_tokens,
            effective_budget_tokens: token_budget_state.effective_budget_tokens,
        })
        .await;
}

fn compact_strategy_label(strategy: CompactStrategy) -> &'static str {
    match strategy {
        CompactStrategy::Micro => "micro",
        CompactStrategy::SessionMemory => "session_memory",
        CompactStrategy::Full => "full",
        CompactStrategy::PartialUpTo => "partial_up_to",
        CompactStrategy::PartialFrom => "partial_from",
    }
}

fn auto_compact_enabled() -> bool {
    !std::env::var("CLAUDE_CODE_DISABLE_AUTOCOMPACT")
        .map(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

fn prompt_cache_metadata_from_assembly(assembly: &PromptAssembly) -> Option<PromptCacheMetadata> {
    let system_blocks = assembly
        .system_sections
        .iter()
        .map(|section| ApiPromptCacheTextBlock {
            text: section.content.clone(),
            cache_scope: map_prompt_cache_scope(section.cache_scope),
        })
        .collect::<Vec<_>>();
    let prepended_user_context_blocks = assembly
        .user_context_sections
        .iter()
        .map(|section| ApiPromptCacheTextBlock {
            text: section.content.clone(),
            cache_scope: map_prompt_cache_scope(section.cache_scope),
        })
        .collect::<Vec<_>>();

    if system_blocks.is_empty() && prepended_user_context_blocks.is_empty() {
        None
    } else {
        Some(PromptCacheMetadata {
            system_blocks,
            prepended_user_context_blocks,
            explicit_tool_cache_breakpoint: true,
            top_level_auto_cache: true,
        })
    }
}

fn map_prompt_cache_scope(scope: PromptCacheScope) -> ApiPromptCacheScope {
    match scope {
        PromptCacheScope::None => ApiPromptCacheScope::None,
        PromptCacheScope::Global => ApiPromptCacheScope::Global,
        PromptCacheScope::Org => ApiPromptCacheScope::Org,
    }
}

fn is_prompt_too_long_error(error: &anyhow::Error) -> bool {
    let lower = error.to_string().to_ascii_lowercase();
    lower.contains("prompt too long")
        || lower.contains("prompt is too long")
        || lower.contains("context length")
        || lower.contains("maximum context length")
}

struct ApiProjectMemorySelector<'a> {
    client: &'a ApiClient,
}

#[async_trait]
impl ProjectMemorySelector for ApiProjectMemorySelector<'_> {
    async fn select(&self, query: ProjectMemorySelectionQuery) -> Result<Vec<String>> {
        if query.candidates.is_empty() {
            return Ok(Vec::new());
        }

        let response = self
            .client
            .chat_with_overrides(
                build_project_memory_selector_messages(&query),
                None,
                Some(PROJECT_MEMORY_SELECTOR_MODEL),
                Some(PROJECT_MEMORY_SELECTOR_MAX_TOKENS),
                Some(0.0),
            )
            .await?;
        let content = response
            .choices
            .first()
            .and_then(|choice| choice.message.content.as_deref())
            .ok_or_else(|| anyhow!("Project memory selector returned no message content"))?;

        parse_project_memory_selector_paths(content)
    }
}

fn stream_snapshot_into_turn_result(snapshot: StreamSnapshot) -> Result<TurnResult> {
    let tool_calls = snapshot
        .tool_calls
        .into_iter()
        .map(stream_tool_call_into_api_tool_call)
        .collect::<Result<Vec<_>>>()?;

    Ok(TurnResult {
        reasoning: snapshot.reasoning_text,
        answer: snapshot.answer_text,
        tool_calls,
        usage: snapshot.usage,
    })
}

fn turn_usage_record(usage: Option<&Usage>) -> AgentUsageRecord {
    match usage {
        Some(usage) => AgentUsageRecord {
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
            usage_missing: false,
        },
        None => AgentUsageRecord {
            usage_missing: true,
            ..AgentUsageRecord::default()
        },
    }
}

fn stream_tool_call_into_api_tool_call(
    stream_tool_call: crate::streaming::StreamToolCall,
) -> Result<ToolCall> {
    let id = stream_tool_call
        .id
        .ok_or_else(|| anyhow!("Streaming tool call {} missing id", stream_tool_call.index))?;
    let name = stream_tool_call.name.ok_or_else(|| {
        anyhow!(
            "Streaming tool call {} missing name",
            stream_tool_call.index
        )
    })?;

    Ok(ToolCall {
        id,
        r#type: "function".to_string(),
        function: ToolCallFunction {
            name,
            arguments: stream_tool_call.arguments,
        },
    })
}

fn parse_tool_arguments(raw: &str) -> Result<Value> {
    if raw.trim().is_empty() {
        return Ok(json!({}));
    }

    serde_json::from_str(raw).map_err(|error| anyhow!("Invalid tool arguments JSON: {}", error))
}

fn tool_is_allowed(tool_name: &str, allowed_tool_names: Option<&HashSet<String>>) -> bool {
    allowed_tool_names
        .map(|allowed| allowed.contains(&tool_name.to_ascii_lowercase()))
        .unwrap_or(true)
}

fn auto_mode_config_from_settings(settings: &Settings) -> AutoModeConfig {
    let env_enabled = std::env::var("CLAUDE_CODE_ENABLE_AUTO_MODE")
        .map(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false);
    let env_disabled = std::env::var("CLAUDE_CODE_DISABLE_AUTO_MODE")
        .map(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false);
    let run_mode = match settings
        .safety
        .auto_mode_stage
        .to_ascii_lowercase()
        .as_str()
    {
        "fast" => AutoModeClassifierRunMode::Fast,
        "thinking" => AutoModeClassifierRunMode::Thinking,
        _ => AutoModeClassifierRunMode::Both,
    };

    AutoModeConfig {
        enabled: (settings.safety.auto_mode || env_enabled) && !env_disabled,
        run_mode,
        circuit_breaker_enabled: settings.safety.auto_mode_circuit_breaker,
        user_allow_rules: settings.safety.auto_mode_allow_rules.clone(),
        user_deny_rules: settings.safety.auto_mode_deny_rules.clone(),
        user_environment_rules: settings.safety.auto_mode_environment.clone(),
        ..AutoModeConfig::default()
    }
}

fn should_stop_for_plan_approval(tool_name: &str, output: &ToolOutput) -> bool {
    tool_name == EXIT_PLAN_MODE_TOOL
        && output
            .metadata
            .get("awaiting_approval")
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

fn plan_approval_answer(output: &ToolOutput) -> String {
    let plan_file_path = output
        .metadata
        .get("plan_file_path")
        .and_then(Value::as_str)
        .unwrap_or("the persisted plan file");
    format!(
        "Plan Mode is awaiting approval. Review the editable plan at `{}` and approve it before implementation continues.",
        plan_file_path
    )
}

fn normalize_tool_input(tool_name: &str, mut input: Value, workspace_root: &Path) -> Value {
    if !input.is_object() {
        return input;
    }

    let input_object = input.as_object_mut().expect("checked object above");
    let workspace_root_string = workspace_root.display().to_string();

    match tool_name {
        "list_files" | "search" => {
            input_object
                .entry("path".to_string())
                .or_insert_with(|| Value::String(workspace_root_string.clone()));
        }
        "execute_command" => {
            input_object
                .entry("cwd".to_string())
                .or_insert_with(|| Value::String(workspace_root_string.clone()));
        }
        "git_operations" => {
            input_object
                .entry("path".to_string())
                .or_insert_with(|| Value::String(workspace_root_string.clone()));
        }
        _ => {}
    }

    for key in ["path", "file_path", "cwd"] {
        if let Some(value) = input_object.get_mut(key) {
            if let Some(path) = value.as_str() {
                *value = Value::String(resolve_workspace_path(workspace_root, path));
            }
        }
    }

    input
}

fn resolve_workspace_path(workspace_root: &Path, raw: &str) -> String {
    let candidate = PathBuf::from(raw);
    if candidate.is_absolute() {
        candidate.display().to_string()
    } else {
        workspace_root.join(candidate).display().to_string()
    }
}

fn tool_output_message(output: &ToolOutput) -> String {
    summarize_text(&output.content, MAX_TOOL_PAYLOAD_CHARS)
}

fn format_tool_error(error: &ToolError) -> String {
    match &error.code {
        Some(code) => format!("{} ({})", error.message, code),
        None => error.message.clone(),
    }
}

fn summarize_text(value: &str, max_chars: usize) -> String {
    let normalized = value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string();

    if normalized.chars().count() <= max_chars {
        normalized
    } else {
        normalized.chars().take(max_chars).collect()
    }
}

fn build_project_memory_selector_messages(query: &ProjectMemorySelectionQuery) -> Vec<ChatMessage> {
    let payload = json!({
        "task": "Select the most relevant project memory files for the active user request.",
        "constraints": {
            "max_paths": PROJECT_MEMORY_SELECTOR_MAX_PATHS,
            "exclude_already_surfaced": true,
            "prefer_enduring_context": true,
            "ignore_recent_tool_reference_noise": true,
        },
        "user_query": query.query,
        "memory_index_excerpt": query.memory_index_excerpt,
        "recent_tools": query.recent_tools,
        "already_surfaced_memory_paths": query.already_surfaced_memory_paths,
        "candidates": query.candidates,
    });

    vec![
        ChatMessage::system(
            "You select relevant project memory files for another coding agent. Return JSON only with the shape {\"paths\":[\"path1.md\",\"path2.md\"]}. Choose at most 5 paths, omit already surfaced memories, and return an empty array when nothing is relevant.",
        ),
        ChatMessage::user(
            serde_json::to_string_pretty(&payload)
                .unwrap_or_else(|_| payload.to_string()),
        ),
    ]
}

fn parse_project_memory_selector_paths(raw: &str) -> Result<Vec<String>> {
    for candidate in project_memory_selector_json_candidates(raw) {
        if let Some(paths) = parse_project_memory_selector_paths_value(candidate) {
            return Ok(paths);
        }
    }

    Err(anyhow!(
        "Project memory selector returned invalid JSON payload: {}",
        summarize_text(raw, 200)
    ))
}

fn project_memory_selector_json_candidates(raw: &str) -> Vec<&str> {
    let trimmed = raw.trim();
    let mut candidates = vec![trimmed];
    if let Some(fenced_body) = trimmed
        .strip_prefix("```json")
        .and_then(|value| value.strip_suffix("```"))
        .map(str::trim)
    {
        candidates.push(fenced_body);
    } else if let Some(fenced_body) = trimmed
        .strip_prefix("```")
        .and_then(|value| value.strip_suffix("```"))
        .map(str::trim)
    {
        candidates.push(fenced_body);
    }
    candidates
}

fn parse_project_memory_selector_paths_value(raw: &str) -> Option<Vec<String>> {
    let value: Value = serde_json::from_str(raw).ok()?;
    let raw_paths = match value {
        Value::Array(paths) => paths,
        Value::Object(object) => object
            .get("paths")
            .or_else(|| object.get("selected_paths"))?
            .as_array()?
            .clone(),
        _ => return None,
    };

    let mut deduped = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for value in raw_paths {
        let path = value.as_str()?.trim();
        if path.is_empty() || !seen.insert(path.to_string()) {
            continue;
        }
        deduped.push(path.to_string());
        if deduped.len() >= PROJECT_MEMORY_SELECTOR_MAX_PATHS {
            break;
        }
    }

    Some(deduped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_snapshot_without_tool_calls_becomes_final_answer() {
        let result = stream_snapshot_into_turn_result(crate::streaming::StreamSnapshot {
            answer_text: "hello from stream".to_string(),
            reasoning_text: "thinking".to_string(),
            tool_calls: Vec::new(),
            usage: None,
            finish_reason: Some("stop".to_string()),
            completed: true,
        })
        .expect("stream result");

        assert_eq!(result.answer, "hello from stream");
        assert_eq!(result.reasoning, "thinking");
        assert!(result.tool_calls.is_empty());
    }

    #[test]
    fn stream_snapshot_with_tool_calls_restores_runtime_tool_calls() {
        let result = stream_snapshot_into_turn_result(crate::streaming::StreamSnapshot {
            answer_text: String::new(),
            reasoning_text: String::new(),
            tool_calls: vec![crate::streaming::StreamToolCall {
                index: 0,
                id: Some("call_9".to_string()),
                name: Some("search".to_string()),
                arguments: "{\"path\":\".\",\"pattern\":\"streaming\"}".to_string(),
            }],
            usage: None,
            finish_reason: Some("tool_calls".to_string()),
            completed: true,
        })
        .expect("stream result");

        assert!(result.answer.is_empty());
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].id, "call_9");
        assert_eq!(result.tool_calls[0].function.name, "search");
        assert_eq!(
            result.tool_calls[0].function.arguments,
            "{\"path\":\".\",\"pattern\":\"streaming\"}"
        );
    }

    #[test]
    fn project_memory_selector_accepts_object_payloads() {
        let paths = parse_project_memory_selector_paths(
            r#"{"paths":["notes/auth.md","notes/auth.md","plans/release.md"]}"#,
        )
        .expect("selector paths");

        assert_eq!(paths, vec!["notes/auth.md", "plans/release.md"]);
    }

    #[test]
    fn project_memory_selector_accepts_fenced_json_arrays() {
        let paths = parse_project_memory_selector_paths(
            "```json\n[\"notes/auth.md\",\"runbooks/deploy.md\"]\n```",
        )
        .expect("selector paths");

        assert_eq!(paths, vec!["notes/auth.md", "runbooks/deploy.md"]);
    }

    #[test]
    fn choose_compaction_prefers_reduction_over_noop() {
        let history = vec![
            ChatMessage::assistant_with_tools(vec![ToolCall {
                id: "call_1".to_string(),
                r#type: "function".to_string(),
                function: ToolCallFunction {
                    name: "search".to_string(),
                    arguments: r#"{"path":"src","pattern":"auth"}"#.to_string(),
                },
            }]),
            ChatMessage::tool("call_1", "search results ".repeat(300)),
            ChatMessage::user("latest question"),
        ];

        let result = best_compaction_candidate(&history, 1);

        assert!(result.after_tokens < result.before_tokens);
    }

    #[test]
    fn full_compact_summary_payload_respects_direction_and_boundary() {
        let history = vec![
            ChatMessage::user("prefix"),
            ChatMessage::assistant("stable context"),
            ChatMessage::user("tail"),
            ChatMessage::assistant("details"),
        ];

        let payload =
            build_full_compaction_summary_payload(&history, CompactDirection::From, Some(2), 1);

        assert_eq!(payload.direction, "from");
        assert_eq!(payload.compacted_message_count, 2);
        assert_eq!(payload.preserved_message_count, 2);
        assert!(!payload.boundary_context.is_empty());
    }

    #[test]
    fn prompt_too_long_error_detector_matches_common_provider_messages() {
        let error = anyhow!("API error (400): prompt is too long for this model");
        let other = anyhow!("network timeout");

        assert!(is_prompt_too_long_error(&error));
        assert!(!is_prompt_too_long_error(&other));
    }
}
