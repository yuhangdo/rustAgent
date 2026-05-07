//! Plan Mode safety state machine.
//!
//! Plan Mode creates a read-only exploration phase before any implementation
//! work. The model can enter the mode, inspect the workspace with read-only
//! tools, then submit an editable plan for approval via `exit_plan_mode`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::RwLock;

use crate::tools::{Tool, ToolAccess, ToolError, ToolOutput};

pub const ENTER_PLAN_MODE_TOOL: &str = "enter_plan_mode";
pub const EXIT_PLAN_MODE_TOOL: &str = "exit_plan_mode";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanMode {
    Default,
    Plan,
    AwaitingApproval,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AllowedPrompt {
    pub tool: String,
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanModeStatus {
    pub mode: PlanMode,
    pub previous_mode: Option<String>,
    pub plan_file_path: PathBuf,
    pub allowed_prompts: Vec<AllowedPrompt>,
    pub awaiting_approval: bool,
    pub plan_was_edited: bool,
}

impl Default for PlanModeStatus {
    fn default() -> Self {
        Self {
            mode: PlanMode::Default,
            previous_mode: None,
            plan_file_path: PathBuf::new(),
            allowed_prompts: Vec::new(),
            awaiting_approval: false,
            plan_was_edited: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PlanModeSession {
    inner: Arc<RwLock<PlanModeSessionState>>,
}

#[derive(Debug, Clone)]
struct PlanModeSessionState {
    workspace_root: PathBuf,
    status: PlanModeStatus,
}

impl PlanModeSession {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            inner: Arc::new(RwLock::new(PlanModeSessionState {
                workspace_root,
                status: PlanModeStatus::default(),
            })),
        }
    }

    pub async fn set_workspace_root(&self, workspace_root: PathBuf) {
        self.inner.write().await.workspace_root = workspace_root;
    }

    pub async fn status(&self) -> PlanModeStatus {
        self.inner.read().await.status.clone()
    }

    pub async fn is_active(&self) -> bool {
        self.status().await.mode == PlanMode::Plan
    }

    pub async fn enter(&self, previous_mode: impl Into<String>) -> Result<PlanModeStatus> {
        let mut guard = self.inner.write().await;
        let previous_mode = previous_mode.into();
        guard.status = PlanModeStatus {
            mode: PlanMode::Plan,
            previous_mode: Some(previous_mode),
            plan_file_path: PathBuf::new(),
            allowed_prompts: Vec::new(),
            awaiting_approval: false,
            plan_was_edited: false,
        };
        Ok(guard.status.clone())
    }

    pub async fn exit_with_plan(
        &self,
        plan: impl AsRef<str>,
        allowed_prompts: Vec<AllowedPrompt>,
    ) -> Result<PlanModeStatus> {
        let plan = plan.as_ref().trim();
        if plan.is_empty() {
            return Err(anyhow!("exit_plan_mode requires a non-empty plan"));
        }

        let mut guard = self.inner.write().await;
        if guard.status.mode != PlanMode::Plan {
            return Err(anyhow!(
                "exit_plan_mode can only be called while Plan Mode is active"
            ));
        }

        let plan_file_path = get_plan_file_path(&guard.workspace_root);
        let plan_document = render_plan_document(plan, &allowed_prompts);
        if let Some(parent) = plan_file_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&plan_file_path, plan_document).await?;

        guard.status.mode = PlanMode::AwaitingApproval;
        guard.status.plan_file_path = plan_file_path;
        guard.status.allowed_prompts = allowed_prompts;
        guard.status.awaiting_approval = true;
        guard.status.plan_was_edited = false;
        Ok(guard.status.clone())
    }

    pub async fn decorated_system_prompt(&self, base_system_prompt: &str) -> String {
        plan_mode_system_prompt(base_system_prompt, &self.status().await)
    }
}

pub fn plan_mode_tools(session: PlanModeSession) -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(EnterPlanModeTool::new(session.clone())),
        Box::new(ExitPlanModeTool::new(session)),
    ]
}

pub fn apply_plan_mode_tool_filter(tools: Vec<(String, ToolAccess)>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut allowed = Vec::new();

    for (name, access) in tools {
        if is_tool_allowed_in_plan_mode(&name, access) && seen.insert(name.clone()) {
            allowed.push(name);
        }
    }

    if seen.insert(EXIT_PLAN_MODE_TOOL.to_string()) {
        allowed.push(EXIT_PLAN_MODE_TOOL.to_string());
    }

    allowed.sort();
    allowed
}

pub fn is_tool_allowed_in_plan_mode(tool_name: &str, access: ToolAccess) -> bool {
    access == ToolAccess::ReadOnly || tool_name == EXIT_PLAN_MODE_TOOL
}

pub fn is_tool_visible_for_mode(
    tool_name: &str,
    access: ToolAccess,
    status: &PlanModeStatus,
) -> bool {
    match status.mode {
        PlanMode::Plan => is_tool_allowed_in_plan_mode(tool_name, access),
        PlanMode::Default | PlanMode::AwaitingApproval => tool_name != EXIT_PLAN_MODE_TOOL,
    }
}

pub fn plan_mode_system_prompt(base_system_prompt: &str, status: &PlanModeStatus) -> String {
    match status.mode {
        PlanMode::Plan => format!(
            "{base_system_prompt}\n\nPlan Mode is active.\n- You are in a read-only exploration phase.\n- Use only read-only tools to inspect the workspace.\n- Do not edit files, run mutating commands, create commits, or otherwise change state.\n- When you have a concrete implementation plan, call `{EXIT_PLAN_MODE_TOOL}` with the plan text and optional `allowed_prompts` entries such as {{\"tool\":\"Bash\",\"prompt\":\"run tests\"}}.\n- After submitting the plan, stop and wait for approval before implementation."
        ),
        PlanMode::Default | PlanMode::AwaitingApproval => format!(
            "{base_system_prompt}\n\nPlan Mode safety policy:\n- For ambiguous, high-impact, or multi-file implementation work, call `{ENTER_PLAN_MODE_TOOL}` before editing.\n- Skip Plan Mode for small, obvious, low-risk fixes.\n- In Plan Mode, first inspect with read-only tools, then call `{EXIT_PLAN_MODE_TOOL}` to persist an editable plan for approval."
        ),
    }
}

fn get_plan_file_path(workspace_root: &std::path::Path) -> PathBuf {
    let stamp = Utc::now().format("%Y-%m-%d-%H%M%S").to_string();
    let suffix = uuid::Uuid::new_v4()
        .to_string()
        .chars()
        .take(8)
        .collect::<String>();
    workspace_root
        .join("docs")
        .join("superpowers")
        .join("plans")
        .join(format!("{stamp}-plan-mode-{suffix}.md"))
}

fn render_plan_document(plan: &str, allowed_prompts: &[AllowedPrompt]) -> String {
    let mut document = String::new();
    document.push_str("# Plan Mode Approval Request\n\n");
    document.push_str("This file is editable before approval. If the plan changes, approve the edited version.\n\n");
    document.push_str("## Plan\n\n");
    document.push_str(plan.trim());
    document.push_str("\n\n## Allowed prompts\n\n");

    if allowed_prompts.is_empty() {
        document.push_str("- None requested.\n");
    } else {
        for prompt in allowed_prompts {
            document.push_str(&format!("- {}: {}\n", prompt.tool, prompt.prompt));
        }
    }

    document
}

pub struct EnterPlanModeTool {
    session: PlanModeSession,
}

impl EnterPlanModeTool {
    pub fn new(session: PlanModeSession) -> Self {
        Self { session }
    }
}

#[async_trait]
impl Tool for EnterPlanModeTool {
    fn name(&self) -> &str {
        ENTER_PLAN_MODE_TOOL
    }

    fn description(&self) -> &str {
        "Enter Plan Mode before ambiguous or high-impact implementation work. After this call, only read-only tools and exit_plan_mode are available."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "previous_mode": {
                    "type": "string",
                    "description": "The permission mode to restore after plan approval. Defaults to default."
                },
                "reason": {
                    "type": "string",
                    "description": "Why this task needs Plan Mode."
                }
            }
        })
    }

    fn access(&self) -> ToolAccess {
        ToolAccess::Internal
    }

    async fn execute(&self, input: Value) -> Result<ToolOutput, ToolError> {
        let previous_mode = input
            .get("previous_mode")
            .and_then(Value::as_str)
            .unwrap_or("default");
        let status = self.enter(previous_mode).await?;
        let mut metadata = HashMap::new();
        metadata.insert("plan_mode_action".to_string(), json!("entered"));
        metadata.insert(
            "previous_mode".to_string(),
            json!(status.previous_mode.clone().unwrap_or_default()),
        );

        Ok(ToolOutput {
            output_type: "json".to_string(),
            content: json!({
                "success": true,
                "status": "plan",
                "message": "Plan Mode is active. Continue with read-only exploration, then call exit_plan_mode with an implementation plan."
            })
            .to_string(),
            metadata,
        })
    }
}

impl EnterPlanModeTool {
    async fn enter(&self, previous_mode: &str) -> Result<PlanModeStatus, ToolError> {
        self.session
            .enter(previous_mode)
            .await
            .map_err(tool_error_from_anyhow)
    }
}

pub struct ExitPlanModeTool {
    session: PlanModeSession,
}

impl ExitPlanModeTool {
    pub fn new(session: PlanModeSession) -> Self {
        Self { session }
    }
}

#[async_trait]
impl Tool for ExitPlanModeTool {
    fn name(&self) -> &str {
        EXIT_PLAN_MODE_TOOL
    }

    fn description(&self) -> &str {
        "Submit the researched implementation plan for approval, persist it as an editable plan file, and leave Plan Mode awaiting approval."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "plan": {
                    "type": "string",
                    "description": "Concrete implementation plan for user approval."
                },
                "allowed_prompts": {
                    "type": "array",
                    "description": "Prompt-based permissions to activate after approval.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "tool": {
                                "type": "string",
                                "enum": ["Bash"],
                                "description": "Tool family the semantic prompt applies to."
                            },
                            "prompt": {
                                "type": "string",
                                "description": "Semantic description, for example run tests."
                            }
                        },
                        "required": ["tool", "prompt"]
                    }
                }
            },
            "required": ["plan"]
        })
    }

    fn access(&self) -> ToolAccess {
        ToolAccess::Internal
    }

    async fn execute(&self, input: Value) -> Result<ToolOutput, ToolError> {
        let plan = input
            .get("plan")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError {
                message: "plan is required".to_string(),
                code: Some("missing_plan".to_string()),
            })?;
        let allowed_prompts = parse_allowed_prompts(input.get("allowed_prompts"))?;
        let status = self
            .session
            .exit_with_plan(plan, allowed_prompts)
            .await
            .map_err(tool_error_from_anyhow)?;

        let mut metadata = HashMap::new();
        metadata.insert("plan_mode_action".to_string(), json!("exited"));
        metadata.insert(
            "plan_file_path".to_string(),
            json!(status.plan_file_path.display().to_string()),
        );
        metadata.insert(
            "awaiting_approval".to_string(),
            json!(status.awaiting_approval),
        );
        metadata.insert("plan_was_edited".to_string(), json!(status.plan_was_edited));
        metadata.insert("allowed_prompts".to_string(), json!(status.allowed_prompts));

        Ok(ToolOutput {
            output_type: "json".to_string(),
            content: json!({
                "success": true,
                "status": "awaiting_approval",
                "plan_file_path": status.plan_file_path,
                "awaiting_approval": status.awaiting_approval,
                "message": "Plan persisted for approval. Stop now and wait for approval before implementation."
            })
            .to_string(),
            metadata,
        })
    }
}

fn parse_allowed_prompts(value: Option<&Value>) -> Result<Vec<AllowedPrompt>, ToolError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    let prompts = value.as_array().ok_or_else(|| ToolError {
        message: "allowed_prompts must be an array".to_string(),
        code: Some("invalid_allowed_prompts".to_string()),
    })?;

    let mut parsed = Vec::new();
    for prompt in prompts {
        let tool = prompt
            .get("tool")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError {
                message: "allowed_prompts entries require tool".to_string(),
                code: Some("invalid_allowed_prompt".to_string()),
            })?;
        let semantic_prompt = prompt
            .get("prompt")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError {
                message: "allowed_prompts entries require prompt".to_string(),
                code: Some("invalid_allowed_prompt".to_string()),
            })?;
        if tool != "Bash" {
            return Err(ToolError {
                message: format!("Unsupported allowed prompt tool: {}", tool),
                code: Some("unsupported_allowed_prompt_tool".to_string()),
            });
        }
        parsed.push(AllowedPrompt {
            tool: tool.to_string(),
            prompt: semantic_prompt.to_string(),
        });
    }

    Ok(parsed)
}

fn tool_error_from_anyhow(error: anyhow::Error) -> ToolError {
    ToolError {
        message: error.to_string(),
        code: Some("plan_mode_error".to_string()),
    }
}
