//! Shared agent runtime with tool execution support.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{json, Value};
use tokio::time::timeout;

use crate::api::{ApiClient, ChatMessage, ToolCall, ToolCallFunction, ToolDefinition, Usage};
use crate::config::Settings;
use crate::streaming::{StreamSnapshot, StreamUpdate, StreamingAssembler};
use crate::tools::{ToolError, ToolOutput, ToolRegistry};

const DEFAULT_MAX_ITERATIONS: usize = 8;
const MAX_ALLOWED_ITERATIONS: usize = 24;
const MAX_TOOL_PAYLOAD_CHARS: usize = 12_000;
const DEFAULT_CONTEXT_WINDOW_TOKENS: usize = 32_000;
const DEFAULT_RECENT_MESSAGE_COUNT: usize = 8;
const MAX_PROJECT_CONTEXT_DOC_CHARS: usize = 4_000;
const MAX_COMPACTED_TOOL_MESSAGE_CHARS: usize = 640;
const MAX_COMPACTED_TEXT_MESSAGE_CHARS: usize = 960;
const STREAM_POLL_INTERVAL_MILLIS: u64 = 250;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContextDocumentKind {
    Instruction,
    Memory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemoryPolicy {
    Use,
    Ignore,
}

#[derive(Debug, Clone)]
pub struct AgentExecutionRequest {
    pub system_prompt: String,
    pub history: Vec<ChatMessage>,
    pub workspace_root: PathBuf,
    pub max_iterations: usize,
}

#[derive(Debug, Clone)]
pub struct AgentExecutionResult {
    pub reasoning: String,
    pub answer: String,
    pub iterations: usize,
    pub usage_records: Vec<AgentUsageRecord>,
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
}

#[derive(Debug, Clone, Default)]
struct TurnResult {
    reasoning: String,
    answer: String,
    tool_calls: Vec<ToolCall>,
    usage: Option<Usage>,
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
        let max_response_tokens = settings.api.max_tokens;
        Self {
            client: ApiClient::new(settings),
            tool_registry: ToolRegistry::new(),
            max_response_tokens,
        }
    }

    pub async fn execute(
        &self,
        request: AgentExecutionRequest,
        event_handler: &dyn AgentEventHandler,
        cancellation: &dyn AgentCancellation,
    ) -> Result<AgentExecutionOutcome> {
        self.execute_with_hook(
            request,
            event_handler,
            cancellation,
            &NoopAgentToolCallHook,
        )
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
            max_iterations: requested_max_iterations,
        } = request;
        let mut latest_reasoning = String::new();
        let mut usage_records = Vec::new();
        let max_iterations = if requested_max_iterations == 0 {
            DEFAULT_MAX_ITERATIONS
        } else {
            requested_max_iterations.min(MAX_ALLOWED_ITERATIONS)
        };
        let tool_definitions = self.tool_definitions();
        let mut messages = build_context_messages_with_budget(
            &system_prompt,
            history,
            &workspace_root,
            &tool_definitions,
            ContextBudget::default_for(self.max_response_tokens),
        );

        for iteration in 0..max_iterations {
            if cancellation.is_cancelled() {
                return Ok(AgentExecutionOutcome::Cancelled);
            }

            let turn_result = if self.client.streaming_enabled() {
                match self
                    .execute_stream_turn(&messages, &tool_definitions, event_handler, cancellation)
                    .await?
                {
                    Some(result) => result,
                    None => return Ok(AgentExecutionOutcome::Cancelled),
                }
            } else {
                self.execute_non_stream_turn(&messages, &tool_definitions, event_handler)
                    .await?
            };

            if !turn_result.reasoning.trim().is_empty() {
                latest_reasoning = turn_result.reasoning.clone();
            }

            usage_records.push(turn_usage_record(turn_result.usage.as_ref()));

            let tool_calls = turn_result.tool_calls;
            if !tool_calls.is_empty() {
                messages.push(ChatMessage::assistant_with_tools(tool_calls.clone()));

                for tool_call in tool_calls {
                    if cancellation.is_cancelled() {
                        return Ok(AgentExecutionOutcome::Cancelled);
                    }

                    let tool_name = tool_call.function.name.clone();
                    let parsed_arguments = parse_tool_arguments(&tool_call.function.arguments)?;
                    let normalized_arguments =
                        normalize_tool_input(&tool_name, parsed_arguments, &workspace_root);
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
                            messages.push(ChatMessage::tool(
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
                        }
                        Err(error) => {
                            let error_payload = json!({
                                "success": false,
                                "error": error.message,
                                "code": error.code,
                            });
                            messages.push(ChatMessage::tool(
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
            messages.push(ChatMessage::assistant(final_answer.clone()));
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
            }));
        }

        Err(anyhow!(
            "Agent loop reached the step limit ({} iterations).",
            max_iterations
        ))
    }

    fn tool_definitions(&self) -> Vec<ToolDefinition> {
        self.tool_registry
            .list()
            .into_iter()
            .map(|tool| ToolDefinition::new(tool.name(), tool.description(), tool.input_schema()))
            .collect()
    }

    async fn execute_non_stream_turn(
        &self,
        messages: &[ChatMessage],
        tool_definitions: &[ToolDefinition],
        event_handler: &dyn AgentEventHandler,
    ) -> Result<TurnResult> {
        let response = self
            .client
            .chat(messages.to_vec(), Some(tool_definitions.to_vec()))
            .await?;

        let choice = response
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
            usage: response.usage.clone(),
        })
    }

    async fn execute_stream_turn(
        &self,
        messages: &[ChatMessage],
        tool_definitions: &[ToolDefinition],
        event_handler: &dyn AgentEventHandler,
        cancellation: &dyn AgentCancellation,
    ) -> Result<Option<TurnResult>> {
        let response = self
            .client
            .chat_stream(messages.to_vec(), Some(tool_definitions.to_vec()))
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

#[derive(Debug, Clone, Copy)]
struct ContextBudget {
    total_input_tokens: usize,
    reserved_output_tokens: usize,
    recent_message_count: usize,
}

impl ContextBudget {
    fn default_for(max_response_tokens: usize) -> Self {
        Self {
            total_input_tokens: DEFAULT_CONTEXT_WINDOW_TOKENS,
            reserved_output_tokens: max_response_tokens.max(512),
            recent_message_count: DEFAULT_RECENT_MESSAGE_COUNT,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct ContextTrimReport {
    dropped_message_count: usize,
    compacted_tool_message_count: usize,
    compacted_text_message_count: usize,
}

impl ContextTrimReport {
    fn merge(&mut self, other: Self) {
        self.dropped_message_count += other.dropped_message_count;
        self.compacted_tool_message_count += other.compacted_tool_message_count;
        self.compacted_text_message_count += other.compacted_text_message_count;
    }

    fn has_changes(&self) -> bool {
        self.dropped_message_count > 0
            || self.compacted_tool_message_count > 0
            || self.compacted_text_message_count > 0
    }
}

#[derive(Debug, Clone)]
struct ContextDocument {
    kind: ContextDocumentKind,
    label: &'static str,
    path: String,
    content: String,
}

#[derive(Debug, Clone)]
struct ContextUnit {
    messages: Vec<ChatMessage>,
    token_estimate: usize,
    protected: bool,
}

#[derive(Debug, Clone, Default)]
struct TrimHistoryResult {
    history: Vec<ChatMessage>,
    report: ContextTrimReport,
    dropped_messages: Vec<ChatMessage>,
}

fn build_context_messages_with_budget(
    system_prompt: &str,
    history: Vec<ChatMessage>,
    workspace_root: &Path,
    tool_definitions: &[ToolDefinition],
    budget: ContextBudget,
) -> Vec<ChatMessage> {
    let memory_policy = resolve_memory_policy(&history);
    let project_memory_message = if memory_policy == MemoryPolicy::Use {
        build_project_memory_message(&load_memory_context_documents(workspace_root))
    } else {
        None
    };
    let tool_definition_tokens = estimate_tool_definition_tokens(tool_definitions);
    let project_memory_tokens = project_memory_message
        .as_ref()
        .map(estimate_message_tokens)
        .unwrap_or_default();

    let (mut trimmed_history, mut report) =
        compact_history_messages(history, budget.recent_message_count);
    let mut dropped_messages = Vec::new();
    let stabilization_passes = trimmed_history.len().saturating_add(2).max(2);

    for _ in 0..stabilization_passes {
        let current_system_prompt = compose_system_prompt(system_prompt, workspace_root, &report);
        let session_memory_message = if memory_policy == MemoryPolicy::Use {
            build_session_memory_message(&dropped_messages, &report, trimmed_history.len())
        } else {
            None
        };
        let history_budget = available_history_tokens(
            budget,
            estimate_text_tokens(&current_system_prompt),
            tool_definition_tokens,
            project_memory_tokens
                + session_memory_message
                    .as_ref()
                    .map(estimate_message_tokens)
                    .unwrap_or_default(),
        );
        let trim_result =
            trim_history_to_budget(trimmed_history, history_budget, budget.recent_message_count);

        if trim_result.report.dropped_message_count == 0 {
            trimmed_history = trim_result.history;

            let mut messages = Vec::with_capacity(trimmed_history.len() + 3);
            messages.push(ChatMessage::system(current_system_prompt));
            if let Some(project_memory_message) = project_memory_message.clone() {
                messages.push(project_memory_message);
            }
            if let Some(session_memory_message) = session_memory_message {
                messages.push(session_memory_message);
            }
            messages.extend(trimmed_history);
            return messages;
        }

        report.merge(trim_result.report);
        dropped_messages.extend(trim_result.dropped_messages);
        trimmed_history = trim_result.history;
    }

    let final_system_prompt = compose_system_prompt(system_prompt, workspace_root, &report);
    let final_session_memory_message = if memory_policy == MemoryPolicy::Use {
        build_session_memory_message(&dropped_messages, &report, trimmed_history.len())
    } else {
        None
    };
    let mut messages = Vec::with_capacity(trimmed_history.len() + 3);
    messages.push(ChatMessage::system(final_system_prompt));
    if let Some(project_memory_message) = project_memory_message {
        messages.push(project_memory_message);
    }
    if let Some(session_memory_message) = final_session_memory_message {
        messages.push(session_memory_message);
    }
    messages.extend(trimmed_history);
    messages
}

fn compose_system_prompt(
    base_system_prompt: &str,
    workspace_root: &Path,
    trim_report: &ContextTrimReport,
) -> String {
    let mut sections = Vec::new();

    if !base_system_prompt.trim().is_empty() {
        sections.push(base_system_prompt.trim().to_string());
    }

    sections.push(format!(
        "## Runtime Context\n- Workspace Root: {}",
        workspace_root.display()
    ));

    sections.push(
        "## Context Reliability Rules\n- Treat recalled project context and session memory as hints, not ground truth.\n- Verify file paths, symbols, commands, and repository state against the current workspace before acting on them.\n- If recent user instructions conflict with older memory, follow the recent user instructions."
            .to_string(),
    );

    let instruction_context_docs = load_instruction_context_documents(workspace_root);
    if !instruction_context_docs.is_empty() {
        let docs_section = instruction_context_docs
            .into_iter()
            .map(|document| {
                format!(
                    "### {} ({})\n{}",
                    document.label, document.path, document.content
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        sections.push(format!("## Project Instruction Files\n{}", docs_section));
    }

    if trim_report.has_changes() {
        sections.push(format!(
            "## Context Trim Notice\n- Older messages omitted: {}\n- Older tool results compacted: {}\n- Older text messages compacted: {}",
            trim_report.dropped_message_count,
            trim_report.compacted_tool_message_count,
            trim_report.compacted_text_message_count,
        ));
    }

    sections.join("\n\n")
}

fn load_project_context_documents(workspace_root: &Path) -> Vec<ContextDocument> {
    let mut candidates = Vec::new();

    if let Some(home_dir) = dirs::home_dir() {
        candidates.push((
            ContextDocumentKind::Instruction,
            "User Global Instructions",
            home_dir.join(".claude").join("CLAUDE.md"),
            "~/.claude/CLAUDE.md".to_string(),
        ));
        candidates.push((
            ContextDocumentKind::Memory,
            "User Global Memory",
            home_dir.join(".claude").join("MEMORY.md"),
            "~/.claude/MEMORY.md".to_string(),
        ));
    }

    candidates.extend([
        (
            ContextDocumentKind::Instruction,
            "Project Instructions",
            workspace_root.join("CLAUDE.md"),
            "CLAUDE.md".to_string(),
        ),
        (
            ContextDocumentKind::Instruction,
            "Agent Instructions",
            workspace_root.join("AGENTS.md"),
            "AGENTS.md".to_string(),
        ),
        (
            ContextDocumentKind::Memory,
            "Project Memory",
            workspace_root.join("MEMORY.md"),
            "MEMORY.md".to_string(),
        ),
        (
            ContextDocumentKind::Instruction,
            "Project Instructions",
            workspace_root.join(".claude").join("CLAUDE.md"),
            ".claude/CLAUDE.md".to_string(),
        ),
        (
            ContextDocumentKind::Memory,
            "Project Memory",
            workspace_root.join(".claude").join("MEMORY.md"),
            ".claude/MEMORY.md".to_string(),
        ),
    ]);

    candidates
        .into_iter()
        .filter_map(|(kind, label, full_path, display_path)| {
            if !full_path.is_file() {
                return None;
            }

            let content = std::fs::read_to_string(&full_path).ok()?;
            let summarized = summarize_text(&content, MAX_PROJECT_CONTEXT_DOC_CHARS);
            if summarized.is_empty() {
                return None;
            }

            Some(ContextDocument {
                kind,
                label,
                path: display_path,
                content: summarized,
            })
        })
        .collect()
}

fn load_instruction_context_documents(workspace_root: &Path) -> Vec<ContextDocument> {
    load_project_context_documents(workspace_root)
        .into_iter()
        .filter(|document| document.kind == ContextDocumentKind::Instruction)
        .collect()
}

fn load_memory_context_documents(workspace_root: &Path) -> Vec<ContextDocument> {
    load_project_context_documents(workspace_root)
        .into_iter()
        .filter(|document| document.kind == ContextDocumentKind::Memory)
        .collect()
}

fn available_history_tokens(
    budget: ContextBudget,
    system_prompt_tokens: usize,
    tool_definition_tokens: usize,
    extra_context_tokens: usize,
) -> usize {
    budget
        .total_input_tokens
        .saturating_sub(budget.reserved_output_tokens)
        .saturating_sub(system_prompt_tokens)
        .saturating_sub(tool_definition_tokens)
        .saturating_sub(extra_context_tokens)
        .max(96)
}

fn compact_history_messages(
    history: Vec<ChatMessage>,
    recent_message_count: usize,
) -> (Vec<ChatMessage>, ContextTrimReport) {
    let mut report = ContextTrimReport::default();
    let recent_start = history.len().saturating_sub(recent_message_count);

    let compacted = history
        .into_iter()
        .enumerate()
        .map(|(index, mut message)| {
            if index >= recent_start {
                return message;
            }

            if let Some(content) = message.content.clone() {
                if message.role == "tool" {
                    let compacted = compact_tool_message(&content);
                    if compacted != content {
                        message.content = Some(compacted);
                        report.compacted_tool_message_count += 1;
                    }
                } else {
                    let compacted = compact_text_message(&content);
                    if compacted != content {
                        message.content = Some(compacted);
                        report.compacted_text_message_count += 1;
                    }
                }
            }

            message
        })
        .collect();

    (compacted, report)
}

fn trim_history_to_budget(
    history: Vec<ChatMessage>,
    history_budget_tokens: usize,
    recent_message_count: usize,
) -> TrimHistoryResult {
    let mut units = group_history_into_units(&history, recent_message_count);
    let mut result = TrimHistoryResult::default();
    let mut total_tokens = units.iter().map(|unit| unit.token_estimate).sum::<usize>();

    if total_tokens <= history_budget_tokens {
        result.history = flatten_context_units(units);
        return result;
    }

    for index in 0..units.len() {
        if total_tokens <= history_budget_tokens {
            break;
        }

        if units[index].protected {
            continue;
        }

        total_tokens = total_tokens.saturating_sub(units[index].token_estimate);
        result.report.dropped_message_count += units[index].messages.len();
        result
            .dropped_messages
            .extend(units[index].messages.drain(..));
        units[index].token_estimate = 0;
    }

    if total_tokens > history_budget_tokens {
        for index in 0..units.len().saturating_sub(1) {
            if total_tokens <= history_budget_tokens {
                break;
            }

            if units[index].messages.is_empty() {
                continue;
            }

            total_tokens = total_tokens.saturating_sub(units[index].token_estimate);
            result.report.dropped_message_count += units[index].messages.len();
            result
                .dropped_messages
                .extend(units[index].messages.drain(..));
            units[index].token_estimate = 0;
        }
    }

    result.history = flatten_context_units(units);
    result
}

fn group_history_into_units(
    history: &[ChatMessage],
    recent_message_count: usize,
) -> Vec<ContextUnit> {
    let recent_start = history.len().saturating_sub(recent_message_count);
    let mut units = Vec::new();
    let mut index = 0;

    while index < history.len() {
        let mut messages = vec![history[index].clone()];
        let mut protected = index >= recent_start;

        if history[index].role == "assistant"
            && history[index]
                .tool_calls
                .as_ref()
                .map(|tool_calls| !tool_calls.is_empty())
                .unwrap_or(false)
        {
            let mut cursor = index + 1;
            while cursor < history.len() && history[cursor].role == "tool" {
                protected |= cursor >= recent_start;
                messages.push(history[cursor].clone());
                cursor += 1;
            }
            index = cursor;
        } else {
            index += 1;
        }

        let token_estimate = messages.iter().map(estimate_message_tokens).sum();
        units.push(ContextUnit {
            messages,
            token_estimate,
            protected,
        });
    }

    units
}

fn flatten_context_units(units: Vec<ContextUnit>) -> Vec<ChatMessage> {
    units
        .into_iter()
        .flat_map(|unit| unit.messages.into_iter())
        .collect()
}

fn estimate_tool_definition_tokens(tool_definitions: &[ToolDefinition]) -> usize {
    tool_definitions
        .iter()
        .map(|tool_definition| {
            estimate_text_tokens(&serde_json::to_string(tool_definition).unwrap_or_default())
        })
        .sum()
}

fn estimate_message_tokens(message: &ChatMessage) -> usize {
    let mut total = 6;

    if let Some(content) = &message.content {
        total += estimate_text_tokens(content);
    }

    if let Some(reasoning_content) = &message.reasoning_content {
        total += estimate_text_tokens(reasoning_content);
    }

    if let Some(tool_calls) = &message.tool_calls {
        total += estimate_text_tokens(&serde_json::to_string(tool_calls).unwrap_or_default());
    }

    if let Some(tool_call_id) = &message.tool_call_id {
        total += estimate_text_tokens(tool_call_id);
    }

    total
}

fn estimate_text_tokens(value: &str) -> usize {
    let char_count = value.chars().count();
    (char_count / 4).max(1) + 1
}

fn build_project_memory_message(memory_documents: &[ContextDocument]) -> Option<ChatMessage> {
    if memory_documents.is_empty() {
        return None;
    }

    let memory_sections = memory_documents
        .iter()
        .map(|document| {
            format!(
                "### {} ({})\n{}",
                document.label, document.path, document.content
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    Some(ChatMessage::user(format!(
        "<system-reminder>\nProject Memory (persistent file context)\n\n{}\n\nBefore recommending from memory:\n- If memory names a file path, verify the file exists in the current workspace.\n- If memory names a function, flag, or symbol, search the current workspace before relying on it.\n- If memory conflicts with recent user instructions or repository state, trust the recent instructions and repository state.\n</system-reminder>",
        memory_sections
    )))
}

fn build_session_memory_message(
    dropped_messages: &[ChatMessage],
    trim_report: &ContextTrimReport,
    preserved_message_count: usize,
) -> Option<ChatMessage> {
    if dropped_messages.is_empty() {
        return None;
    }

    let user_snippets = dropped_messages
        .iter()
        .filter(|message| message.role == "user")
        .filter_map(|message| message.content.as_deref())
        .map(|content| summarize_text(content, 160))
        .filter(|content| !content.is_empty())
        .take(3)
        .collect::<Vec<_>>();

    let assistant_snippets = dropped_messages
        .iter()
        .filter(|message| message.role == "assistant")
        .filter_map(|message| message.content.as_deref())
        .map(|content| summarize_text(content, 160))
        .filter(|content| !content.is_empty())
        .take(3)
        .collect::<Vec<_>>();

    let tool_names = dropped_messages
        .iter()
        .filter_map(|message| message.tool_calls.as_ref())
        .flat_map(|tool_calls| {
            tool_calls
                .iter()
                .map(|tool_call| tool_call.function.name.clone())
        })
        .collect::<BTreeSet<_>>();

    let tool_result_count = dropped_messages
        .iter()
        .filter(|message| message.role == "tool")
        .count();

    let mut sections = vec![
        "<system-reminder>".to_string(),
        "Session Memory (auto-generated from trimmed earlier context)".to_string(),
        format!(
            "Compact Boundary\n- Type: auto_session_memory\n- Older messages omitted: {}\n- Older tool results compacted before trimming: {}\n- Older text messages compacted before trimming: {}\n- Preserved Segment after boundary: {} message(s)",
            trim_report.dropped_message_count,
            trim_report.compacted_tool_message_count,
            trim_report.compacted_text_message_count,
            preserved_message_count,
        ),
    ];

    if !user_snippets.is_empty() {
        sections.push(format!(
            "Earlier user requests:\n- {}",
            user_snippets.join("\n- ")
        ));
    }

    if !assistant_snippets.is_empty() {
        sections.push(format!(
            "Earlier assistant progress:\n- {}",
            assistant_snippets.join("\n- ")
        ));
    }

    if !tool_names.is_empty() || tool_result_count > 0 {
        let mut tool_section = String::new();
        if !tool_names.is_empty() {
            tool_section.push_str(&format!(
                "Earlier tools used:\n- {}",
                tool_names.into_iter().collect::<Vec<_>>().join("\n- ")
            ));
        }

        if tool_result_count > 0 {
            if !tool_section.is_empty() {
                tool_section.push('\n');
            }
            tool_section.push_str(&format!(
                "Earlier tool result messages: {}",
                tool_result_count
            ));
        }

        sections.push(tool_section);
    }

    sections.push(
        "Use this session memory as soft recall only. Re-check files, symbols, commands, and current repository state before relying on it."
            .to_string(),
    );
    sections.push("</system-reminder>".to_string());

    Some(ChatMessage::user(sections.join("\n\n")))
}

fn resolve_memory_policy(history: &[ChatMessage]) -> MemoryPolicy {
    for content in history
        .iter()
        .rev()
        .filter(|message| message.role == "user")
        .filter_map(|message| message.content.as_deref())
    {
        let normalized = content.to_ascii_lowercase();
        if normalized.contains("ignore memory")
            || normalized.contains("don't use memory")
            || normalized.contains("do not use memory")
            || normalized.contains("without memory")
            || content.contains("\u{5ffd}\u{7565}\u{8bb0}\u{5fc6}")
            || content.contains("\u{4e0d}\u{8981}\u{7528}\u{8bb0}\u{5fc6}")
            || content.contains("\u{4e0d}\u{8981}\u{4f7f}\u{7528}\u{8bb0}\u{5fc6}")
            || content.contains("\u{522b}\u{7528}\u{8bb0}\u{5fc6}")
        {
            return MemoryPolicy::Ignore;
        }

        if normalized.contains("use memory")
            || normalized.contains("you can use memory")
            || normalized.contains("feel free to use memory")
            || content.contains("\u{4f7f}\u{7528}\u{8bb0}\u{5fc6}")
            || content.contains("\u{53ef}\u{4ee5}\u{7528}\u{8bb0}\u{5fc6}")
        {
            return MemoryPolicy::Use;
        }
    }

    MemoryPolicy::Use
}

fn compact_tool_message(content: &str) -> String {
    if content.chars().count() <= MAX_COMPACTED_TOOL_MESSAGE_CHARS {
        return content.to_string();
    }

    let summarized = summarize_text(content, MAX_COMPACTED_TOOL_MESSAGE_CHARS);
    format!("[compacted tool result] {}", summarized)
}

fn compact_text_message(content: &str) -> String {
    if content.chars().count() <= MAX_COMPACTED_TEXT_MESSAGE_CHARS {
        return content.to_string();
    }

    let summarized = summarize_text(content, MAX_COMPACTED_TEXT_MESSAGE_CHARS);
    format!("[compacted message] {}", summarized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn tool_definition(name: &str) -> ToolDefinition {
        ToolDefinition::new(
            name,
            format!("tool {}", name),
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                }
            }),
        )
    }

    #[test]
    fn compose_system_prompt_includes_workspace_and_instruction_files() {
        let temp_dir = tempdir().expect("temp dir");
        std::fs::write(
            temp_dir.path().join("CLAUDE.md"),
            "# Project Guide\nAlways prefer safe edits.",
        )
        .expect("write claude doc");
        std::fs::write(
            temp_dir.path().join("MEMORY.md"),
            "# Team Memory\nThe app uses strict MVI.",
        )
        .expect("write memory doc");

        let composed = compose_system_prompt(
            "Base instructions",
            temp_dir.path(),
            &ContextTrimReport::default(),
        );

        assert!(composed.contains("Base instructions"));
        assert!(composed.contains("Workspace Root"));
        assert!(composed.contains("CLAUDE.md"));
        assert!(composed.contains("Always prefer safe edits."));
        assert!(!composed.contains("MEMORY.md"));
    }

    #[test]
    fn build_context_messages_prunes_old_history_when_budget_is_tight() {
        let history = vec![
            ChatMessage::user("old user context ".repeat(200)),
            ChatMessage::assistant("old assistant context ".repeat(200)),
            ChatMessage::user("latest question"),
        ];

        let messages = build_context_messages_with_budget(
            "Base instructions",
            history,
            Path::new("."),
            &[tool_definition("file_read"), tool_definition("search")],
            ContextBudget {
                total_input_tokens: 220,
                reserved_output_tokens: 64,
                recent_message_count: 2,
            },
        );

        assert_eq!(messages.first().map(|m| m.role.as_str()), Some("system"));
        let combined = messages
            .iter()
            .filter_map(|message| message.content.clone())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(combined.contains("latest question"));
        assert!(combined.contains("Context Trim Notice"));
        assert!(combined.contains("Session Memory"));
        assert!(combined.contains("Earlier assistant progress"));
    }

    #[test]
    fn build_context_messages_compacts_old_tool_results_before_dropping_recent_turns() {
        let long_tool_output = "tool output ".repeat(500);
        let history = vec![
            ChatMessage::assistant_with_tools(vec![crate::api::ToolCall {
                id: "call_1".to_string(),
                r#type: "function".to_string(),
                function: crate::api::ToolCallFunction {
                    name: "search".to_string(),
                    arguments: "{\"path\":\".\"}".to_string(),
                },
            }]),
            ChatMessage::tool("call_1", long_tool_output.clone()),
            ChatMessage::user("recent question"),
            ChatMessage::assistant("recent answer"),
        ];

        let messages = build_context_messages_with_budget(
            "Base instructions",
            history,
            Path::new("."),
            &[tool_definition("search")],
            ContextBudget {
                total_input_tokens: 900,
                reserved_output_tokens: 64,
                recent_message_count: 2,
            },
        );

        let tool_message = messages
            .iter()
            .find(|message| message.role == "tool")
            .and_then(|message| message.content.clone())
            .expect("tool message");

        assert!(tool_message.contains("[compacted tool result]"));
        assert!(tool_message.len() < long_tool_output.len());
        assert!(messages
            .iter()
            .any(|message| { message.content.as_deref() == Some("recent question") }));
    }

    #[test]
    fn build_context_messages_reinjects_trimmed_history_as_session_memory() {
        let history = vec![
            ChatMessage::user("please inspect the auth flow and note that rollout starts Thursday"),
            ChatMessage::assistant(
                "I inspected the flow and found the auth rewrite depends on compliance.",
            ),
            ChatMessage::assistant_with_tools(vec![crate::api::ToolCall {
                id: "call_2".to_string(),
                r#type: "function".to_string(),
                function: crate::api::ToolCallFunction {
                    name: "search".to_string(),
                    arguments: "{\"path\":\".\",\"pattern\":\"auth\"}".to_string(),
                },
            }]),
            ChatMessage::tool("call_2", "search results ".repeat(300)),
            ChatMessage::user("latest question"),
        ];

        let messages = build_context_messages_with_budget(
            "Base instructions",
            history,
            Path::new("."),
            &[tool_definition("search")],
            ContextBudget {
                total_input_tokens: 240,
                reserved_output_tokens: 64,
                recent_message_count: 1,
            },
        );

        let reminder_message = messages
            .iter()
            .find(|message| {
                message.role == "user"
                    && message
                        .content
                        .as_deref()
                        .unwrap_or_default()
                        .contains("Session Memory")
            })
            .and_then(|message| message.content.clone())
            .expect("session memory reminder");

        assert!(reminder_message.contains("Earlier user requests"));
        assert!(reminder_message.contains("rollout starts Thursday"));
        assert!(reminder_message.contains("Earlier tools used"));
        assert!(reminder_message.contains("search"));
        assert!(messages
            .iter()
            .any(|message| { message.content.as_deref() == Some("latest question") }));
    }

    #[test]
    fn build_context_messages_injects_memory_docs_as_user_context() {
        let temp_dir = tempdir().expect("temp dir");
        std::fs::write(
            temp_dir.path().join("MEMORY.md"),
            "# Memory\nAuth rewrite is blocked by compliance review.",
        )
        .expect("write memory doc");

        let messages = build_context_messages_with_budget(
            "Base instructions",
            vec![ChatMessage::user("what should I know before editing auth?")],
            temp_dir.path(),
            &[tool_definition("search")],
            ContextBudget::default_for(4096),
        );

        let memory_message = messages
            .iter()
            .find(|message| {
                message.role == "user"
                    && message
                        .content
                        .as_deref()
                        .unwrap_or_default()
                        .contains("Project Memory")
            })
            .and_then(|message| message.content.clone())
            .expect("memory message");

        assert!(memory_message.contains("MEMORY.md"));
        assert!(memory_message.contains("Auth rewrite is blocked by compliance review."));
    }

    #[test]
    fn build_context_messages_respects_ignore_memory_request() {
        let temp_dir = tempdir().expect("temp dir");
        std::fs::write(
            temp_dir.path().join("MEMORY.md"),
            "# Memory\nThis should be ignored.",
        )
        .expect("write memory doc");

        let history = vec![
            ChatMessage::user(
                "Please ignore memory for this task and inspect the current repo only.",
            ),
            ChatMessage::assistant("Earlier answer ".repeat(120)),
            ChatMessage::user("latest question"),
        ];

        let messages = build_context_messages_with_budget(
            "Base instructions",
            history,
            temp_dir.path(),
            &[tool_definition("search")],
            ContextBudget {
                total_input_tokens: 220,
                reserved_output_tokens: 64,
                recent_message_count: 1,
            },
        );

        let combined = messages
            .iter()
            .filter_map(|message| message.content.clone())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(!combined.contains("Project Memory"));
        assert!(!combined.contains("Session Memory"));
        assert!(!combined.contains("This should be ignored."));
    }

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
}
