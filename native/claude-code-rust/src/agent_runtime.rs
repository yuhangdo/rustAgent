//! Shared agent runtime with tool execution support.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{json, Value};
use tokio::time::timeout;

use crate::api::{ApiClient, ChatMessage, ToolCall, ToolCallFunction, ToolDefinition, Usage};
use crate::config::Settings;
use crate::prompting::{
    ProjectMemorySelectionQuery, ProjectMemorySelector, PromptBudget, PromptBuildRequest,
    PromptBuilder,
};
use crate::streaming::{StreamSnapshot, StreamUpdate, StreamingAssembler};
use crate::tools::{ToolError, ToolOutput, ToolRegistry};

const DEFAULT_MAX_ITERATIONS: usize = 8;
const MAX_ALLOWED_ITERATIONS: usize = 24;
const MAX_TOOL_PAYLOAD_CHARS: usize = 12_000;
const STREAM_POLL_INTERVAL_MILLIS: u64 = 250;
const PROJECT_MEMORY_SELECTOR_MODEL: &str = "sonnet";
const PROJECT_MEMORY_SELECTOR_MAX_TOKENS: usize = 512;
const PROJECT_MEMORY_SELECTOR_MAX_PATHS: usize = 5;

#[derive(Debug, Clone)]
pub struct AgentExecutionRequest {
    pub system_prompt: String,
    pub history: Vec<ChatMessage>,
    pub workspace_root: PathBuf,
    pub already_surfaced_memory_paths: Vec<String>,
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
    MemorySurfaced {
        paths: Vec<String>,
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
        let working_dir = settings.working_dir.clone();
        let memory_enabled = settings.memory.enabled;
        let auto_memory_directory = settings.memory.auto_memory_directory.clone();
        Self {
            client: ApiClient::new(settings),
            tool_registry: ToolRegistry::new(),
            max_response_tokens,
            working_dir,
            memory_enabled,
            auto_memory_directory,
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
            already_surfaced_memory_paths,
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
        let prompt_request = PromptBuildRequest {
            base_system_prompt: system_prompt,
            history,
            workspace_root: workspace_root.clone(),
            current_working_dir: Some(self.working_dir.clone()),
            tool_definitions: tool_definitions.clone(),
            budget: PromptBudget::default_for(self.max_response_tokens),
            entrypoint: "rust-agent-runtime".to_string(),
            version_fingerprint: None,
            global_config_root: None,
            memory_enabled: self.memory_enabled,
            auto_memory_directory: self.auto_memory_directory.clone(),
            already_surfaced_memory_paths,
        };
        let memory_selector = ApiProjectMemorySelector {
            client: &self.client,
        };
        let prompt_assembly =
            PromptBuilder::build_with_selector(prompt_request, Some(&memory_selector)).await?;
        if !prompt_assembly.surfaced_memory_paths.is_empty() {
            event_handler
                .on_event(AgentEvent::MemorySurfaced {
                    paths: prompt_assembly.surfaced_memory_paths.clone(),
                })
                .await;
        }
        let mut messages = prompt_assembly.render().messages;

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
}
