//! Shared agent runtime with tool execution support.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::api::{ApiClient, ChatMessage, ToolDefinition};
use crate::config::Settings;
use crate::tools::{ToolError, ToolOutput, ToolRegistry};

const DEFAULT_MAX_ITERATIONS: usize = 8;
const MAX_ALLOWED_ITERATIONS: usize = 24;
const MAX_TOOL_PAYLOAD_CHARS: usize = 12_000;

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
}

#[derive(Debug, Clone)]
pub enum AgentExecutionOutcome {
    Completed(AgentExecutionResult),
    Cancelled,
}

#[derive(Debug, Clone)]
pub enum AgentEvent {
    Reasoning {
        full_text: String,
        summary: String,
    },
    ToolCallRequested {
        tool_name: String,
        input_preview: String,
    },
    ToolCallCompleted {
        tool_name: String,
        output_preview: String,
    },
    ToolCallFailed {
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

pub struct NoopAgentEventHandler;

#[async_trait]
impl AgentEventHandler for NoopAgentEventHandler {}

pub struct NoopAgentCancellation;

impl AgentCancellation for NoopAgentCancellation {}

pub struct AgentRuntime {
    client: ApiClient,
    tool_registry: ToolRegistry,
}

impl AgentRuntime {
    pub fn new(settings: Settings) -> Self {
        Self {
            client: ApiClient::new(settings),
            tool_registry: ToolRegistry::new(),
        }
    }

    pub async fn execute(
        &self,
        request: AgentExecutionRequest,
        event_handler: &dyn AgentEventHandler,
        cancellation: &dyn AgentCancellation,
    ) -> Result<AgentExecutionOutcome> {
        let mut messages = build_initial_messages(request.system_prompt, request.history);
        let mut latest_reasoning = String::new();
        let max_iterations = if request.max_iterations == 0 {
            DEFAULT_MAX_ITERATIONS
        } else {
            request.max_iterations.min(MAX_ALLOWED_ITERATIONS)
        };
        let tool_definitions = self.tool_definitions();

        for iteration in 0..max_iterations {
            if cancellation.is_cancelled() {
                return Ok(AgentExecutionOutcome::Cancelled);
            }

            let response = self
                .client
                .chat(messages.clone(), Some(tool_definitions.clone()))
                .await?;

            let choice = response
                .choices
                .first()
                .cloned()
                .ok_or_else(|| anyhow!("API response did not include a choice"))?;

            if let Some(reasoning) = choice.message.reasoning_content.clone() {
                if !reasoning.trim().is_empty() {
                    latest_reasoning = reasoning.clone();
                    event_handler
                        .on_event(AgentEvent::Reasoning {
                            summary: summarize_text(&reasoning, 400),
                            full_text: reasoning,
                        })
                        .await;
                }
            }

            let tool_calls = choice.message.tool_calls.clone().unwrap_or_default();
            if !tool_calls.is_empty() {
                messages.push(ChatMessage::assistant_with_tools(tool_calls.clone()));

                for tool_call in tool_calls {
                    if cancellation.is_cancelled() {
                        return Ok(AgentExecutionOutcome::Cancelled);
                    }

                    let tool_name = tool_call.function.name.clone();
                    let parsed_arguments = parse_tool_arguments(&tool_call.function.arguments)?;
                    let normalized_arguments =
                        normalize_tool_input(&tool_name, parsed_arguments, &request.workspace_root);

                    event_handler
                        .on_event(AgentEvent::ToolCallRequested {
                            tool_name: tool_name.clone(),
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
                            messages.push(ChatMessage::tool(tool_call.id.clone(), tool_content.clone()));
                            event_handler
                                .on_event(AgentEvent::ToolCallCompleted {
                                    tool_name,
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
                                    tool_name,
                                    error_summary: format_tool_error(&error),
                                })
                                .await;
                        }
                    }
                }

                continue;
            }

            let answer = choice
                .message
                .content
                .clone()
                .unwrap_or_default()
                .trim()
                .to_string();
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
            .map(|tool| {
                ToolDefinition::new(tool.name(), tool.description(), tool.input_schema())
            })
            .collect()
    }
}

fn build_initial_messages(system_prompt: String, history: Vec<ChatMessage>) -> Vec<ChatMessage> {
    let mut messages = Vec::with_capacity(history.len() + 1);
    if !system_prompt.trim().is_empty() {
        messages.push(ChatMessage::system(system_prompt));
    }
    messages.extend(history);
    messages
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
