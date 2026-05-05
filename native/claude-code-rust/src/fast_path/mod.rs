use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use futures::future::join_all;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::agent_runtime::{AgentCancellation, AgentEvent, AgentEventHandler};
use crate::api::{ApiClient, ChatMessage, ToolCall, ToolCallFunction, Usage};
use crate::token_budget::ProviderKind;
use crate::tools::{ToolError, ToolOutput, ToolRegistry};

const MAX_QUICK_STEPS: usize = 3;
const MAX_QUICK_BATCHES: usize = 2;
const MAX_ROUTE_HISTORY_MESSAGES: usize = 8;
const MAX_PLAN_HISTORY_MESSAGES: usize = 6;
const MAX_VISIBLE_HISTORY_MESSAGES: usize = 20;
const MAX_TOOL_OUTPUT_CHARS: usize = 12_000;
const QUICK_PATH_ROUTER_MODEL: &str = "haiku";
const QUICK_PATH_ROUTER_MAX_TOKENS: usize = 256;
const QUICK_PATH_PLANNER_MODEL: &str = "haiku";
const QUICK_PATH_PLANNER_MAX_TOKENS: usize = 768;
const QUICK_PATH_FINALIZER_MAX_TOKENS: usize = 1_024;
const QUICK_PATH_CONFIDENCE_AUTO: f32 = 0.8;
const QUICK_PATH_CONFIDENCE_PREFER_FAST: f32 = 0.65;
const QUICK_PATH_CONFIDENCE_PREFER_SLOW: f32 = 0.95;

const QUICK_ALLOWED_TOOLS: [&str; 4] = ["search", "list_files", "file_read", "execute_command"];

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionModeHint {
    #[default]
    Auto,
    PreferFast,
    PreferSlow,
    ForceSlow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HardRouteDecision {
    ForceSlow,
    QuickCandidate,
    NeedClassifier,
}

#[derive(Debug, Clone)]
pub struct QuickRouteInput {
    pub hint: ExecutionModeHint,
    pub history: Vec<ChatMessage>,
    pub has_additional_context_sections: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuickToolPlan {
    pub goal: String,
    pub steps: Vec<QuickToolStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuickToolStep {
    pub id: String,
    pub tool: String,
    pub input: Value,
    #[serde(default)]
    pub depends_on: Vec<String>,
    pub read_only: bool,
    pub reason: String,
}

pub struct QuickPathRequest<'a> {
    pub system_prompt: &'a str,
    pub hint: ExecutionModeHint,
    pub history: &'a [ChatMessage],
    pub workspace_root: &'a Path,
    pub has_additional_context_sections: bool,
}

pub enum QuickPathExecution {
    Skipped {
        reason: String,
        usage_records: Vec<Option<Usage>>,
    },
    Downgraded {
        reason: String,
        appended_history: Vec<ChatMessage>,
        usage_records: Vec<Option<Usage>>,
    },
    Completed {
        answer: String,
        usage_records: Vec<Option<Usage>>,
    },
    Cancelled,
}

#[derive(Debug, Clone)]
struct RouteSelection {
    reason: String,
    used_classifier: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct RouteClassifierResponse {
    route: String,
    #[serde(default)]
    confidence: f32,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    candidate_tools: Vec<String>,
    #[serde(default)]
    has_dependencies: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct PlannerResponse {
    route: Option<String>,
    reason: Option<String>,
    goal: Option<String>,
    steps: Option<Vec<QuickToolStep>>,
}

#[derive(Debug, Clone, Deserialize)]
struct FinalizerResponse {
    status: String,
    answer: Option<String>,
    reason: Option<String>,
}

#[derive(Debug, Clone)]
struct ExecutedQuickStep {
    step: QuickToolStep,
    tool_call: ToolCall,
    normalized_input: Value,
    output_message: String,
    failed: bool,
}

pub struct QuickPathExecutor<'a> {
    client: &'a ApiClient,
    tool_registry: &'a ToolRegistry,
}

impl<'a> QuickPathExecutor<'a> {
    pub fn new(client: &'a ApiClient, tool_registry: &'a ToolRegistry) -> Self {
        Self {
            client,
            tool_registry,
        }
    }

    pub async fn execute(
        &self,
        request: QuickPathRequest<'_>,
        event_handler: &dyn AgentEventHandler,
        cancellation: &dyn AgentCancellation,
    ) -> Result<QuickPathExecution> {
        if cancellation.is_cancelled() {
            return Ok(QuickPathExecution::Cancelled);
        }

        let mut usage_records = Vec::new();
        let route = match self.determine_route(&request, &mut usage_records).await {
            Ok(Some(route)) => route,
            Ok(None) => {
                return Ok(QuickPathExecution::Skipped {
                    reason: "Request stayed on the slow path.".to_string(),
                    usage_records,
                });
            }
            Err(error) => {
                return Ok(QuickPathExecution::Skipped {
                    reason: format!(
                        "Quick-path routing fell back to the slow path: {}",
                        summarize_text(&error.to_string(), 220)
                    ),
                    usage_records,
                });
            }
        };

        if cancellation.is_cancelled() {
            return Ok(QuickPathExecution::Cancelled);
        }

        let plan_response = match self.request_tool_plan(&request).await {
            Ok((plan_response, usage)) => {
                usage_records.push(usage);
                plan_response
            }
            Err(error) => {
                return Ok(QuickPathExecution::Skipped {
                    reason: format!(
                        "Quick-path planning fell back to the slow path: {}",
                        summarize_text(&error.to_string(), 220)
                    ),
                    usage_records,
                });
            }
        };
        let planner_route = plan_response
            .route
            .as_deref()
            .map(|value| value.to_ascii_lowercase())
            .unwrap_or_else(|| "quick".to_string());
        if planner_route == "slow" {
            return Ok(QuickPathExecution::Skipped {
                reason: plan_response
                    .reason
                    .unwrap_or_else(|| "Planner sent the request to the slow path.".to_string()),
                usage_records,
            });
        }

        let plan = QuickToolPlan {
            goal: plan_response
                .goal
                .unwrap_or_else(|| latest_user_request(request.history)),
            steps: plan_response.steps.unwrap_or_default(),
        };
        if let Err(error) = validate_quick_plan_for_workspace(&plan, request.workspace_root) {
            return Ok(QuickPathExecution::Skipped {
                reason: format!(
                    "Quick-path planning produced an invalid read-only plan: {}",
                    summarize_text(&error.to_string(), 220)
                ),
                usage_records,
            });
        }
        let batches = match build_execution_batches(&plan) {
            Ok(batches) => batches,
            Err(error) => {
                return Ok(QuickPathExecution::Skipped {
                    reason: format!(
                        "Quick-path batching fell back to the slow path: {}",
                        summarize_text(&error.to_string(), 220)
                    ),
                    usage_records,
                });
            }
        };

        event_handler
            .on_event(AgentEvent::QuickPathSelected {
                reason: route.reason,
                planned_tools: plan.steps.len(),
                batch_count: batches.len(),
                used_classifier: route.used_classifier,
            })
            .await;

        let mut executed_steps = Vec::new();
        for batch in batches {
            if cancellation.is_cancelled() {
                return Ok(QuickPathExecution::Cancelled);
            }

            let results = join_all(batch.into_iter().map(|step| async {
                self.execute_step(step, request.workspace_root, event_handler)
                    .await
            }))
            .await;

            for result in results {
                executed_steps.push(result?);
            }
        }

        let appended_history = quick_path_history_messages(&executed_steps);
        if executed_steps.iter().any(|step| step.failed) {
            event_handler
                .on_event(AgentEvent::QuickPathDowngraded {
                    reason: "A read-only fast-path tool call failed.".to_string(),
                    executed_tools: executed_steps.len(),
                })
                .await;
            return Ok(QuickPathExecution::Downgraded {
                reason: "A read-only fast-path tool call failed.".to_string(),
                appended_history,
                usage_records,
            });
        }

        if executed_steps
            .iter()
            .all(|step| step.output_message.trim().is_empty())
        {
            event_handler
                .on_event(AgentEvent::QuickPathDowngraded {
                    reason: "Fast-path tool results were empty.".to_string(),
                    executed_tools: executed_steps.len(),
                })
                .await;
            return Ok(QuickPathExecution::Downgraded {
                reason: "Fast-path tool results were empty.".to_string(),
                appended_history,
                usage_records,
            });
        }

        if cancellation.is_cancelled() {
            return Ok(QuickPathExecution::Cancelled);
        }

        let finalizer = match self
            .request_final_answer(&request, &plan, &executed_steps)
            .await
        {
            Ok((finalizer, usage)) => {
                usage_records.push(usage);
                finalizer
            }
            Err(error) => {
                let reason = format!(
                    "Quick-path finalization fell back to the slow path: {}",
                    summarize_text(&error.to_string(), 220)
                );
                event_handler
                    .on_event(AgentEvent::QuickPathDowngraded {
                        reason: reason.clone(),
                        executed_tools: executed_steps.len(),
                    })
                    .await;
                return Ok(QuickPathExecution::Downgraded {
                    reason,
                    appended_history,
                    usage_records,
                });
            }
        };
        if finalizer.status.eq_ignore_ascii_case("answer") {
            let answer = finalizer.answer.unwrap_or_default().trim().to_string();
            if !answer.is_empty() {
                return Ok(QuickPathExecution::Completed {
                    answer,
                    usage_records,
                });
            }
        }

        let reason = finalizer
            .reason
            .unwrap_or_else(|| "Fast-path finalization requested the slow path.".to_string());
        event_handler
            .on_event(AgentEvent::QuickPathDowngraded {
                reason: reason.clone(),
                executed_tools: executed_steps.len(),
            })
            .await;
        Ok(QuickPathExecution::Downgraded {
            reason,
            appended_history,
            usage_records,
        })
    }

    async fn determine_route(
        &self,
        request: &QuickPathRequest<'_>,
        usage_records: &mut Vec<Option<Usage>>,
    ) -> Result<Option<RouteSelection>> {
        let hard_decision = hard_route_decision(&QuickRouteInput {
            hint: request.hint,
            history: request.history.to_vec(),
            has_additional_context_sections: request.has_additional_context_sections,
        });

        match hard_decision {
            HardRouteDecision::ForceSlow => return Ok(None),
            HardRouteDecision::QuickCandidate => {
                return Ok(Some(RouteSelection {
                    reason:
                        "Hard rules classified the request as a read-only quick-path candidate."
                            .to_string(),
                    used_classifier: false,
                }));
            }
            HardRouteDecision::NeedClassifier => {}
        }

        let (response, usage) = self.request_route_decision(request).await?;
        usage_records.push(usage);

        if !response.route.eq_ignore_ascii_case("quick") {
            return Ok(None);
        }
        if response.has_dependencies && request.hint != ExecutionModeHint::PreferFast {
            return Ok(None);
        }
        if !response
            .candidate_tools
            .iter()
            .all(|tool| QUICK_ALLOWED_TOOLS.contains(&tool.as_str()))
        {
            return Ok(None);
        }
        if response.confidence < confidence_threshold(request.hint) {
            return Ok(None);
        }

        Ok(Some(RouteSelection {
            reason: response.reason,
            used_classifier: true,
        }))
    }

    async fn request_route_decision(
        &self,
        request: &QuickPathRequest<'_>,
    ) -> Result<(RouteClassifierResponse, Option<Usage>)> {
        let response = self
            .client
            .chat_with_overrides(
                build_route_classifier_messages(request),
                None,
                auxiliary_model_override(self.client, QUICK_PATH_ROUTER_MODEL),
                Some(QUICK_PATH_ROUTER_MAX_TOKENS),
                Some(0.0),
            )
            .await?;
        let usage = response.usage.clone();
        let content = response
            .choices
            .first()
            .and_then(|choice| choice.message.content.as_deref())
            .ok_or_else(|| anyhow!("Quick-path classifier returned no message content"))?;

        Ok((parse_route_classifier_response(content)?, usage))
    }

    async fn request_tool_plan(
        &self,
        request: &QuickPathRequest<'_>,
    ) -> Result<(PlannerResponse, Option<Usage>)> {
        let response = self
            .client
            .chat_with_overrides(
                build_tool_plan_messages(request),
                None,
                auxiliary_model_override(self.client, QUICK_PATH_PLANNER_MODEL),
                Some(QUICK_PATH_PLANNER_MAX_TOKENS),
                Some(0.0),
            )
            .await?;
        let usage = response.usage.clone();
        let content = response
            .choices
            .first()
            .and_then(|choice| choice.message.content.as_deref())
            .ok_or_else(|| anyhow!("Quick-path planner returned no message content"))?;

        Ok((parse_planner_response(content)?, usage))
    }

    async fn request_final_answer(
        &self,
        request: &QuickPathRequest<'_>,
        plan: &QuickToolPlan,
        executed_steps: &[ExecutedQuickStep],
    ) -> Result<(FinalizerResponse, Option<Usage>)> {
        let response = self
            .client
            .chat_with_overrides(
                build_finalizer_messages(request, plan, executed_steps),
                None,
                None,
                Some(QUICK_PATH_FINALIZER_MAX_TOKENS),
                Some(0.0),
            )
            .await?;
        let usage = response.usage.clone();
        let content = response
            .choices
            .first()
            .and_then(|choice| choice.message.content.as_deref())
            .ok_or_else(|| anyhow!("Quick-path finalizer returned no message content"))?;

        Ok((parse_finalizer_response(content)?, usage))
    }

    async fn execute_step(
        &self,
        step: QuickToolStep,
        workspace_root: &Path,
        event_handler: &dyn AgentEventHandler,
    ) -> Result<ExecutedQuickStep> {
        let tool_call_id = format!("quick_{}", step.id);
        let normalized_input = normalize_tool_input(&step.tool, step.input.clone(), workspace_root);
        validate_step_input_for_workspace(&step, workspace_root)?;
        event_handler
            .on_event(AgentEvent::ToolCallRequested {
                tool_call_id: tool_call_id.clone(),
                tool_name: step.tool.clone(),
                input: normalized_input.clone(),
                input_preview: summarize_text(&normalized_input.to_string(), 280),
            })
            .await;

        let output = self
            .tool_registry
            .execute(&step.tool, normalized_input.clone())
            .await;
        let (output_message, failed) = match output {
            Ok(output) => {
                let content = tool_output_message(&output);
                event_handler
                    .on_event(AgentEvent::ToolCallCompleted {
                        tool_call_id: tool_call_id.clone(),
                        tool_name: step.tool.clone(),
                        output: content.clone(),
                        output_preview: summarize_text(&content, 400),
                    })
                    .await;
                (content, false)
            }
            Err(error) => {
                let payload = json!({
                    "success": false,
                    "error": error.message,
                    "code": error.code,
                });
                event_handler
                    .on_event(AgentEvent::ToolCallFailed {
                        tool_call_id: tool_call_id.clone(),
                        tool_name: step.tool.clone(),
                        error_summary: format_tool_error(&error),
                    })
                    .await;
                (payload.to_string(), true)
            }
        };

        Ok(ExecutedQuickStep {
            tool_call: ToolCall {
                id: tool_call_id.clone(),
                r#type: "function".to_string(),
                function: ToolCallFunction {
                    name: step.tool.clone(),
                    arguments: normalized_input.to_string(),
                },
            },
            step,
            normalized_input,
            output_message,
            failed,
        })
    }
}

pub fn hard_route_decision(input: &QuickRouteInput) -> HardRouteDecision {
    if matches!(
        input.hint,
        ExecutionModeHint::ForceSlow | ExecutionModeHint::PreferSlow
    ) {
        return HardRouteDecision::ForceSlow;
    }

    if input.has_additional_context_sections
        || visible_history_len(&input.history) > MAX_VISIBLE_HISTORY_MESSAGES
    {
        return HardRouteDecision::ForceSlow;
    }

    let latest_request = latest_user_request(&input.history);
    if latest_request.is_empty()
        || latest_request.chars().count() > 900
        || contains_write_intent(&latest_request)
        || contains_complex_intent(&latest_request)
    {
        return HardRouteDecision::ForceSlow;
    }

    if has_unsettled_complex_suffix(&input.history) {
        return HardRouteDecision::ForceSlow;
    }

    if contains_simple_read_intent(&latest_request) {
        HardRouteDecision::QuickCandidate
    } else {
        HardRouteDecision::NeedClassifier
    }
}

pub fn validate_quick_plan(plan: &QuickToolPlan) -> Result<()> {
    if plan.steps.is_empty() {
        return Err(anyhow!("Quick-path plan must contain at least one step."));
    }
    if plan.steps.len() > MAX_QUICK_STEPS {
        return Err(anyhow!(
            "Quick-path plan supports at most {} steps.",
            MAX_QUICK_STEPS
        ));
    }

    let mut seen_ids = HashSet::new();
    let known_ids = plan
        .steps
        .iter()
        .map(|step| step.id.as_str())
        .collect::<HashSet<_>>();
    for step in &plan.steps {
        if step.id.trim().is_empty() || !seen_ids.insert(step.id.clone()) {
            return Err(anyhow!("Quick-path step ids must be unique and non-empty."));
        }
        if !step.read_only {
            return Err(anyhow!("Quick-path steps must be explicitly read-only."));
        }
        if !QUICK_ALLOWED_TOOLS.contains(&step.tool.as_str()) {
            return Err(anyhow!(
                "Tool {} is not allowed on the quick path.",
                step.tool
            ));
        }
        if step
            .depends_on
            .iter()
            .any(|dependency| dependency == &step.id)
        {
            return Err(anyhow!(
                "Quick-path step {} cannot depend on itself.",
                step.id
            ));
        }
        for dependency in &step.depends_on {
            if !known_ids.contains(dependency.as_str()) {
                return Err(anyhow!(
                    "Quick-path step {} depends on unknown step {}.",
                    step.id,
                    dependency
                ));
            }
        }
        validate_step_shape(step)?;
    }

    Ok(())
}

pub fn validate_quick_plan_for_workspace(
    plan: &QuickToolPlan,
    workspace_root: &Path,
) -> Result<()> {
    validate_quick_plan(plan)?;

    for step in &plan.steps {
        validate_step_input_for_workspace(step, workspace_root)?;
    }

    Ok(())
}

pub fn build_execution_batches(plan: &QuickToolPlan) -> Result<Vec<Vec<QuickToolStep>>> {
    validate_quick_plan(plan)?;

    let mut remaining = plan
        .steps
        .iter()
        .cloned()
        .map(|step| (step.id.clone(), step))
        .collect::<HashMap<_, _>>();
    let order = plan
        .steps
        .iter()
        .enumerate()
        .map(|(index, step)| (step.id.clone(), index))
        .collect::<HashMap<_, _>>();
    let mut completed = HashSet::new();
    let mut batches = Vec::new();

    while !remaining.is_empty() {
        let mut ready = remaining
            .values()
            .filter(|step| {
                step.depends_on
                    .iter()
                    .all(|dependency| completed.contains(dependency))
            })
            .cloned()
            .collect::<Vec<_>>();
        if ready.is_empty() {
            return Err(anyhow!(
                "Quick-path plan dependencies contain a cycle or an unresolved dependency."
            ));
        }
        ready.sort_by_key(|step| order.get(&step.id).copied().unwrap_or(usize::MAX));
        for step in &ready {
            remaining.remove(&step.id);
            completed.insert(step.id.clone());
        }
        batches.push(ready);
    }

    if batches.len() > MAX_QUICK_BATCHES {
        return Err(anyhow!(
            "Quick-path plan needs {} batches but only {} are allowed.",
            batches.len(),
            MAX_QUICK_BATCHES
        ));
    }

    Ok(batches)
}

pub fn validate_read_only_command(command: &str) -> Result<()> {
    let normalized = prevalidate_command_string(command)?;
    let tokens = tokenize_shell_command(normalized)?;
    validate_read_only_command_tokens(&tokens, None)
}

pub fn validate_read_only_command_in_workspace(command: &str, workspace_root: &Path) -> Result<()> {
    let tokens = tokenize_shell_command(prevalidate_command_string(command)?)?;
    validate_read_only_command_tokens(&tokens, Some(workspace_root))
}

fn validate_step_shape(step: &QuickToolStep) -> Result<()> {
    match step.tool.as_str() {
        "search" => {
            required_string_field(&step.input, "path")?;
            required_string_field(&step.input, "pattern")?;
        }
        "list_files" => {
            required_string_field(&step.input, "path")?;
        }
        "file_read" => {
            required_string_field(&step.input, "file_path")?;
        }
        "execute_command" => {
            required_string_field(&step.input, "command")?;
            if let Some(timeout) = step.input.get("timeout").and_then(Value::as_u64) {
                if timeout > 30 {
                    return Err(anyhow!(
                        "Quick-path execute_command timeout must stay at or below 30 seconds."
                    ));
                }
            }
        }
        _ => {
            return Err(anyhow!(
                "Tool {} is not allowed on the quick path.",
                step.tool
            ));
        }
    }

    Ok(())
}

fn validate_step_input_for_workspace(step: &QuickToolStep, workspace_root: &Path) -> Result<()> {
    validate_step_shape(step)?;

    match step.tool.as_str() {
        "search" => {
            let path = required_string_field(&step.input, "path")?;
            validate_workspace_path(path, workspace_root)?;
        }
        "list_files" => {
            let path = required_string_field(&step.input, "path")?;
            validate_workspace_path(path, workspace_root)?;
        }
        "file_read" => {
            let file_path = required_string_field(&step.input, "file_path")?;
            validate_workspace_path(file_path, workspace_root)?;
        }
        "execute_command" => {
            let command = required_string_field(&step.input, "command")?;
            validate_read_only_command_in_workspace(command, workspace_root)?;
            if let Some(cwd) = step.input.get("cwd").and_then(Value::as_str) {
                validate_workspace_path(cwd, workspace_root)?;
            }
        }
        _ => {}
    }

    Ok(())
}

fn required_string_field<'a>(input: &'a Value, field: &str) -> Result<&'a str> {
    input
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("Quick-path tool input requires string field {}.", field))
}

fn prevalidate_command_string<'a>(command: &'a str) -> Result<&'a str> {
    let normalized = command.trim();
    if normalized.is_empty() {
        return Err(anyhow!("Command cannot be empty."));
    }

    for marker in ["&&", "||", "|", ";", ">", "<", "`", "\n", "\r", "$(", "&"] {
        if normalized.contains(marker) {
            return Err(anyhow!(
                "Command contains unsupported shell control sequence: {}",
                marker
            ));
        }
    }

    Ok(normalized)
}

fn validate_read_only_command_tokens(
    tokens: &[String],
    workspace_root: Option<&Path>,
) -> Result<()> {
    let command_name = tokens
        .first()
        .map(|token| token.to_ascii_lowercase())
        .ok_or_else(|| anyhow!("Command cannot be empty."))?;

    #[cfg(target_os = "windows")]
    {
        match command_name.as_str() {
            "git" => validate_read_only_git_command(tokens, workspace_root),
            "rg" => validate_read_only_rg_command(tokens, workspace_root),
            "dir" => validate_single_optional_path_command(tokens, workspace_root),
            "type" => validate_single_required_path_command(tokens, workspace_root),
            "where" => validate_where_command(tokens),
            _ => Err(anyhow!(
                "Command {} is not whitelisted for the quick path.",
                command_name
            )),
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        match command_name.as_str() {
            "git" => validate_read_only_git_command(tokens, workspace_root),
            "rg" => validate_read_only_rg_command(tokens, workspace_root),
            "ls" => validate_single_optional_path_command(tokens, workspace_root),
            "cat" => validate_single_required_path_command(tokens, workspace_root),
            "pwd" => validate_zero_argument_command(tokens),
            "which" => validate_where_command(tokens),
            _ => Err(anyhow!(
                "Command {} is not whitelisted for the quick path.",
                command_name
            )),
        }
    }
}

fn contains_write_intent(request: &str) -> bool {
    let normalized = request.to_ascii_lowercase();
    [
        "edit ",
        "modify ",
        "change ",
        "update ",
        "implement ",
        "create ",
        "add ",
        "remove ",
        "delete ",
        "rename ",
        "refactor ",
        "fix ",
        "write ",
        "commit ",
        "push ",
        "merge ",
        "rebase ",
        "apply patch",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

fn contains_complex_intent(request: &str) -> bool {
    let normalized = request.to_ascii_lowercase();
    [
        "step by step",
        "after that",
        "and then",
        "thorough",
        "deep dive",
        "root cause",
        "architecture",
        "design",
        "plan",
        "full audit",
        "end-to-end",
        "across the entire",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

fn contains_simple_read_intent(request: &str) -> bool {
    let normalized = request.to_ascii_lowercase();
    [
        "find ",
        "search ",
        "show ",
        "list ",
        "read ",
        "explain ",
        "summarize ",
        "where ",
        "which ",
        "what ",
        "check ",
        "inspect ",
        "locate ",
        "status",
        "diff",
        "log",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

fn latest_user_request(history: &[ChatMessage]) -> String {
    history
        .iter()
        .rev()
        .find(|message| message.role == "user")
        .and_then(|message| message.content.clone())
        .unwrap_or_default()
}

fn visible_history_len(history: &[ChatMessage]) -> usize {
    history
        .iter()
        .filter(|message| matches!(message.role.as_str(), "user" | "assistant"))
        .count()
}

fn has_unsettled_complex_suffix(history: &[ChatMessage]) -> bool {
    let Some(last_user_index) = history.iter().rposition(|message| message.role == "user") else {
        return false;
    };
    if last_user_index == 0 {
        return false;
    }

    let previous_message = &history[last_user_index - 1];
    previous_message.role == "tool"
        || previous_message
            .tool_calls
            .as_ref()
            .map(|tool_calls| !tool_calls.is_empty())
            .unwrap_or(false)
}

fn confidence_threshold(hint: ExecutionModeHint) -> f32 {
    match hint {
        ExecutionModeHint::PreferFast => QUICK_PATH_CONFIDENCE_PREFER_FAST,
        ExecutionModeHint::PreferSlow => QUICK_PATH_CONFIDENCE_PREFER_SLOW,
        ExecutionModeHint::ForceSlow => 1.0,
        ExecutionModeHint::Auto => QUICK_PATH_CONFIDENCE_AUTO,
    }
}

fn auxiliary_model_override(client: &ApiClient, model: &'static str) -> Option<&'static str> {
    (client.provider_kind() == ProviderKind::AnthropicNative).then_some(model)
}

fn tokenize_shell_command(command: &str) -> Result<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None;

    for ch in command.chars() {
        match quote {
            Some(active) if ch == active => {
                quote = None;
            }
            Some(_) => current.push(ch),
            None if ch == '\'' || ch == '"' => {
                quote = Some(ch);
            }
            None if ch.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(current.clone());
                    current.clear();
                }
            }
            None => current.push(ch),
        }
    }

    if quote.is_some() {
        return Err(anyhow!("Command contains an unmatched quote."));
    }
    if !current.is_empty() {
        tokens.push(current);
    }

    Ok(tokens)
}

#[cfg(not(target_os = "windows"))]
fn validate_zero_argument_command(tokens: &[String]) -> Result<()> {
    if tokens.len() == 1 {
        Ok(())
    } else {
        Err(anyhow!(
            "Command {} does not accept additional arguments on the quick path.",
            tokens[0]
        ))
    }
}

fn validate_single_optional_path_command(
    tokens: &[String],
    workspace_root: Option<&Path>,
) -> Result<()> {
    if tokens.len() > 2 {
        return Err(anyhow!(
            "Command {} supports at most one path argument on the quick path.",
            tokens[0]
        ));
    }
    if let (Some(path), Some(workspace_root)) = (tokens.get(1), workspace_root) {
        validate_workspace_path(path, workspace_root)?;
    }
    Ok(())
}

fn validate_single_required_path_command(
    tokens: &[String],
    workspace_root: Option<&Path>,
) -> Result<()> {
    if tokens.len() != 2 {
        return Err(anyhow!(
            "Command {} requires exactly one path argument on the quick path.",
            tokens[0]
        ));
    }
    if let Some(workspace_root) = workspace_root {
        validate_workspace_path(&tokens[1], workspace_root)?;
    }
    Ok(())
}

fn validate_where_command(tokens: &[String]) -> Result<()> {
    if tokens.len() != 2 {
        return Err(anyhow!(
            "Command {} requires exactly one executable name on the quick path.",
            tokens[0]
        ));
    }
    let needle = &tokens[1];
    if needle.contains('/') || needle.contains('\\') || needle.contains(':') {
        return Err(anyhow!(
            "Command {} only accepts bare executable names on the quick path.",
            tokens[0]
        ));
    }
    Ok(())
}

fn validate_read_only_rg_command(tokens: &[String], workspace_root: Option<&Path>) -> Result<()> {
    if tokens.len() < 2 {
        return Err(anyhow!("rg needs a pattern or --files on the quick path."));
    }

    let mut index = 1;
    let mut saw_files_mode = false;
    let mut saw_pattern = false;
    while index < tokens.len() {
        let token = tokens[index].as_str();
        if !saw_pattern && token.starts_with('-') {
            match token {
                "-n" | "-S" | "-F" | "-i" | "-uu" | "--hidden" => {
                    index += 1;
                    continue;
                }
                "--files" => {
                    saw_files_mode = true;
                    index += 1;
                    break;
                }
                _ => {
                    return Err(anyhow!(
                        "rg flag {} is not allowed on the quick path.",
                        token
                    ));
                }
            }
        }

        saw_pattern = true;
        index += 1;
        break;
    }

    if !saw_files_mode && !saw_pattern {
        return Err(anyhow!("rg needs a search pattern on the quick path."));
    }

    if let Some(workspace_root) = workspace_root {
        for path in &tokens[index..] {
            validate_workspace_path(path, workspace_root)?;
        }
    }

    Ok(())
}

fn validate_read_only_git_command(tokens: &[String], workspace_root: Option<&Path>) -> Result<()> {
    let subcommand = tokens
        .get(1)
        .map(|token| token.to_ascii_lowercase())
        .ok_or_else(|| anyhow!("git quick-path commands need a subcommand."))?;
    match subcommand.as_str() {
        "status" => validate_git_status_command(tokens),
        "diff" => validate_git_diff_command(tokens, workspace_root),
        "log" => validate_git_log_command(tokens),
        "show" => validate_git_show_command(tokens),
        "rev-parse" => validate_git_rev_parse_command(tokens),
        "branch" => {
            if tokens.iter().any(|token| {
                matches!(
                    token.as_str(),
                    "-d" | "-D" | "-m" | "-M" | "-c" | "-C" | "--delete" | "--move" | "--copy"
                )
            }) {
                Err(anyhow!(
                    "git branch mutation flags are not allowed on the quick path."
                ))
            } else {
                Ok(())
            }
        }
        _ => Err(anyhow!(
            "git {} is not whitelisted for the quick path.",
            subcommand
        )),
    }
}

fn validate_git_status_command(tokens: &[String]) -> Result<()> {
    for token in &tokens[2..] {
        if !matches!(
            token.as_str(),
            "--short" | "--branch" | "--porcelain" | "-s" | "-b"
        ) {
            return Err(anyhow!(
                "git status flag {} is not allowed on the quick path.",
                token
            ));
        }
    }
    Ok(())
}

fn validate_git_diff_command(tokens: &[String], workspace_root: Option<&Path>) -> Result<()> {
    let mut seen_separator = false;
    for token in &tokens[2..] {
        if token == "--" {
            seen_separator = true;
            continue;
        }
        if seen_separator {
            if let Some(workspace_root) = workspace_root {
                validate_workspace_path(token, workspace_root)?;
            }
            continue;
        }
        if !matches!(
            token.as_str(),
            "--name-only" | "--stat" | "--cached" | "--staged"
        ) {
            return Err(anyhow!(
                "git diff token {} is not allowed on the quick path.",
                token
            ));
        }
    }
    Ok(())
}

fn validate_git_log_command(tokens: &[String]) -> Result<()> {
    let mut index = 2;
    while index < tokens.len() {
        match tokens[index].as_str() {
            "--oneline" | "--decorate" | "--graph" => index += 1,
            "-n" | "--max-count" => {
                let Some(value) = tokens.get(index + 1) else {
                    return Err(anyhow!("git log count flag requires a value."));
                };
                value
                    .parse::<usize>()
                    .map_err(|_| anyhow!("git log count must be numeric on the quick path."))?;
                index += 2;
            }
            other => {
                return Err(anyhow!(
                    "git log token {} is not allowed on the quick path.",
                    other
                ));
            }
        }
    }
    Ok(())
}

fn validate_git_show_command(tokens: &[String]) -> Result<()> {
    if tokens.len() > 4 {
        return Err(anyhow!(
            "git show takes too many arguments for the quick path."
        ));
    }
    for token in &tokens[2..] {
        if token.starts_with('-') && token != "--stat" {
            return Err(anyhow!(
                "git show flag {} is not allowed on the quick path.",
                token
            ));
        }
    }
    Ok(())
}

fn validate_git_rev_parse_command(tokens: &[String]) -> Result<()> {
    for token in &tokens[2..] {
        if !matches!(token.as_str(), "HEAD" | "--show-toplevel") {
            return Err(anyhow!(
                "git rev-parse token {} is not allowed on the quick path.",
                token
            ));
        }
    }
    Ok(())
}

fn validate_workspace_path(raw_path: &str, workspace_root: &Path) -> Result<()> {
    let workspace_root = normalize_workspace_path(workspace_root)?;
    let candidate = normalize_workspace_candidate(raw_path, &workspace_root)?;

    if candidate.starts_with(&workspace_root) {
        Ok(())
    } else {
        Err(anyhow!(
            "Path {} escapes the workspace root {}.",
            raw_path,
            workspace_root.display()
        ))
    }
}

fn normalize_workspace_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| anyhow!("Failed to resolve current directory: {}", error))?
            .join(path)
    };
    normalize_path_lexically(&absolute)
}

fn normalize_workspace_candidate(raw_path: &str, workspace_root: &Path) -> Result<PathBuf> {
    let candidate = PathBuf::from(raw_path);
    let absolute = if candidate.is_absolute() {
        candidate
    } else {
        workspace_root.join(candidate)
    };
    normalize_path_lexically(&absolute)
}

fn normalize_path_lexically(path: &Path) -> Result<PathBuf> {
    let mut normalized = PathBuf::new();

    for component in path.components() {
        match component {
            std::path::Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            std::path::Component::RootDir => normalized.push(component.as_os_str()),
            std::path::Component::CurDir => {}
            std::path::Component::Normal(segment) => normalized.push(segment),
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    return Err(anyhow!(
                        "Path {} escapes above the filesystem root.",
                        path.display()
                    ));
                }
            }
        }
    }

    Ok(normalized)
}

fn build_route_classifier_messages(request: &QuickPathRequest<'_>) -> Vec<ChatMessage> {
    let payload = json!({
        "task": "Decide whether the request is safe for a read-only quick path.",
        "constraints": {
            "allowed_tools": QUICK_ALLOWED_TOOLS,
            "max_steps": MAX_QUICK_STEPS,
            "max_batches": MAX_QUICK_BATCHES,
            "read_only_only": true,
            "if_unsure_choose": "slow"
        },
        "system_prompt_excerpt": summarize_text(request.system_prompt, 1_200),
        "latest_user_request": latest_user_request(request.history),
        "recent_history": summarized_history(request.history, MAX_ROUTE_HISTORY_MESSAGES),
        "allowed_execute_command_examples": allowed_execute_command_examples(),
    });

    vec![
        ChatMessage::system(
            "You route coding requests between a strict read-only quick path and a normal slow agent. Return JSON only with {\"route\":\"quick|slow\",\"confidence\":0.0,\"reason\":\"...\",\"candidate_tools\":[...],\"has_dependencies\":false}. Choose slow if there is any doubt.",
        ),
        ChatMessage::user(
            serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string()),
        ),
    ]
}

fn build_tool_plan_messages(request: &QuickPathRequest<'_>) -> Vec<ChatMessage> {
    let payload = json!({
        "task": "Create a read-only quick-path tool plan for a coding request.",
        "constraints": {
            "return_json_only": true,
            "allowed_tools": QUICK_ALLOWED_TOOLS,
            "max_steps": MAX_QUICK_STEPS,
            "max_batches": MAX_QUICK_BATCHES,
            "all_steps_must_be_read_only": true,
            "if_not_safe_return": { "route": "slow", "reason": "..." }
        },
        "system_prompt_excerpt": summarize_text(request.system_prompt, 1_200),
        "latest_user_request": latest_user_request(request.history),
        "recent_history": summarized_history(request.history, MAX_PLAN_HISTORY_MESSAGES),
        "tool_schemas": [
            {"tool":"search","required":["path","pattern"]},
            {"tool":"list_files","required":["path"],"optional":["recursive"]},
            {"tool":"file_read","required":["file_path"]},
            {"tool":"execute_command","required":["command"],"optional":["cwd","timeout"],"notes":"Only read-only whitelisted commands are allowed."}
        ],
        "allowed_execute_command_examples": allowed_execute_command_examples(),
    });

    vec![
        ChatMessage::system(
            "You plan a tiny read-only tool run for another coding agent. Return JSON only. Use 1 to 3 steps. Each step must include id, tool, input, depends_on, read_only, and reason. If the request is not safely solvable with the quick path, return {\"route\":\"slow\",\"reason\":\"...\"}.",
        ),
        ChatMessage::user(
            serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string()),
        ),
    ]
}

fn build_finalizer_messages(
    request: &QuickPathRequest<'_>,
    plan: &QuickToolPlan,
    executed_steps: &[ExecutedQuickStep],
) -> Vec<ChatMessage> {
    let payload = json!({
        "task": "Answer the user's coding question from read-only tool results.",
        "constraints": {
            "return_json_only": true,
            "allowed_status": ["answer", "slow"],
            "if_results_conflicting_or_incomplete_choose": "slow",
            "do_not_invent_facts": true
        },
        "system_prompt_excerpt": summarize_text(request.system_prompt, 1_200),
        "latest_user_request": latest_user_request(request.history),
        "recent_history": summarized_history(request.history, MAX_PLAN_HISTORY_MESSAGES),
        "plan_goal": plan.goal,
        "tool_results": executed_steps
            .iter()
            .map(|step| {
                json!({
                    "id": step.step.id,
                    "tool": step.step.tool,
                    "input": step.normalized_input,
                    "failed": step.failed,
                    "output": summarize_text(&step.output_message, MAX_TOOL_OUTPUT_CHARS),
                })
            })
            .collect::<Vec<_>>(),
    });

    vec![
        ChatMessage::system(
            "You finalize a read-only coding assistant answer. Return JSON only with either {\"status\":\"answer\",\"answer\":\"...\"} or {\"status\":\"slow\",\"reason\":\"...\"}. Choose slow if the evidence is incomplete or conflicting.",
        ),
        ChatMessage::user(
            serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string()),
        ),
    ]
}

fn summarized_history(history: &[ChatMessage], max_messages: usize) -> Vec<Value> {
    history
        .iter()
        .rev()
        .filter(|message| message.role == "user" || message.role == "assistant")
        .take(max_messages)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|message| {
            let mut value = json!({
                "role": message.role,
            });
            if let Some(content) = &message.content {
                value["content"] = Value::String(summarize_text(content, 1_200));
            }
            if let Some(reasoning) = &message.reasoning_content {
                value["reasoning"] = Value::String(summarize_text(reasoning, 600));
            }
            value
        })
        .collect()
}

fn parse_route_classifier_response(raw: &str) -> Result<RouteClassifierResponse> {
    parse_json_candidates(raw)?
        .into_iter()
        .find_map(|candidate| serde_json::from_str::<RouteClassifierResponse>(candidate).ok())
        .ok_or_else(|| anyhow!("Quick-path classifier returned invalid JSON."))
}

fn parse_planner_response(raw: &str) -> Result<PlannerResponse> {
    parse_json_candidates(raw)?
        .into_iter()
        .find_map(|candidate| serde_json::from_str::<PlannerResponse>(candidate).ok())
        .ok_or_else(|| anyhow!("Quick-path planner returned invalid JSON."))
}

fn parse_finalizer_response(raw: &str) -> Result<FinalizerResponse> {
    parse_json_candidates(raw)?
        .into_iter()
        .find_map(|candidate| serde_json::from_str::<FinalizerResponse>(candidate).ok())
        .ok_or_else(|| anyhow!("Quick-path finalizer returned invalid JSON."))
}

fn parse_json_candidates(raw: &str) -> Result<Vec<&str>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("Model returned an empty body."));
    }

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

    Ok(candidates)
}

fn allowed_execute_command_examples() -> Vec<&'static str> {
    #[cfg(target_os = "windows")]
    {
        vec![
            "git status --short",
            "git diff --name-only",
            "git log --oneline -n 20",
            "rg QuerySubmitRequest src",
            "dir src",
            "type src\\query_engine\\mod.rs",
            "where rg",
        ]
    }

    #[cfg(not(target_os = "windows"))]
    {
        vec![
            "git status --short",
            "git diff --name-only",
            "git log --oneline -n 20",
            "rg QuerySubmitRequest src",
            "ls src",
            "cat src/query_engine/mod.rs",
            "which rg",
        ]
    }
}

fn quick_path_history_messages(executed_steps: &[ExecutedQuickStep]) -> Vec<ChatMessage> {
    if executed_steps.is_empty() {
        return Vec::new();
    }

    let mut messages = Vec::new();
    messages.push(ChatMessage::assistant_with_tools(
        executed_steps
            .iter()
            .map(|step| step.tool_call.clone())
            .collect::<Vec<_>>(),
    ));
    for step in executed_steps {
        messages.push(ChatMessage::tool(
            step.tool_call.id.clone(),
            step.output_message.clone(),
        ));
    }
    messages
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
    summarize_text(&output.content, MAX_TOOL_OUTPUT_CHARS)
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
