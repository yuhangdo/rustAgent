//! Auto Mode safety state machine and classifier.
//!
//! Auto Mode keeps the normal tool surface visible, but every tool call is
//! classified before execution. Safe calls run autonomously, dangerous calls are
//! denied, and ambiguous calls downgrade to an ask-style refusal instead of
//! being auto-approved.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::RwLock;

use crate::api::ChatMessage;
use crate::tools::ToolAccess;

const DEFAULT_MAX_TRANSCRIPT_MESSAGES: usize = 80;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AutoModeDecisionBehavior {
    Allow,
    Deny,
    Ask,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AutoModeClassifierStage {
    Fast,
    Thinking,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AutoModeClassifierRunMode {
    Both,
    Fast,
    Thinking,
}

impl Default for AutoModeClassifierRunMode {
    fn default() -> Self {
        Self::Both
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AutoModePromptKind {
    FullInstructions,
    SparseReminder,
    ExitInstructions,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PermissionRuleBehavior {
    Allow,
    Ask,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PermissionRule {
    pub tool_name: String,
    #[serde(default)]
    pub rule_content: Option<String>,
    pub behavior: PermissionRuleBehavior,
}

impl PermissionRule {
    pub fn new(
        tool_name: impl Into<String>,
        rule_content: Option<&str>,
        behavior: PermissionRuleBehavior,
    ) -> Self {
        Self {
            tool_name: tool_name.into(),
            rule_content: rule_content.map(ToString::to_string),
            behavior,
        }
    }

    pub fn display_rule(&self) -> String {
        match &self.rule_content {
            Some(content) => format!("{}({})", self.tool_name, content),
            None => self.tool_name.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutoModeRuleStripResult {
    pub safe_rules: Vec<PermissionRule>,
    pub stripped_rules: Vec<PermissionRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutoModeConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub run_mode: AutoModeClassifierRunMode,
    #[serde(default)]
    pub circuit_breaker_enabled: bool,
    #[serde(default = "default_max_transcript_messages")]
    pub max_transcript_messages: usize,
    #[serde(default)]
    pub always_allow_rules: Vec<PermissionRule>,
    #[serde(default)]
    pub user_allow_rules: Vec<String>,
    #[serde(default)]
    pub user_deny_rules: Vec<String>,
    #[serde(default)]
    pub user_environment_rules: Vec<String>,
}

impl Default for AutoModeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            run_mode: AutoModeClassifierRunMode::Both,
            circuit_breaker_enabled: false,
            max_transcript_messages: DEFAULT_MAX_TRANSCRIPT_MESSAGES,
            always_allow_rules: Vec::new(),
            user_allow_rules: Vec::new(),
            user_deny_rules: Vec::new(),
            user_environment_rules: Vec::new(),
        }
    }
}

impl AutoModeConfig {
    pub fn enabled() -> Self {
        Self {
            enabled: true,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutoModeStatus {
    pub active: bool,
    pub previous_mode: Option<String>,
    pub model: String,
    pub model_supported: bool,
    pub circuit_broken: bool,
    pub stripped_dangerous_rules: Vec<String>,
}

impl Default for AutoModeStatus {
    fn default() -> Self {
        Self {
            active: false,
            previous_mode: None,
            model: String::new(),
            model_supported: false,
            circuit_broken: false,
            stripped_dangerous_rules: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutoModeDecision {
    pub behavior: AutoModeDecisionBehavior,
    pub should_block: bool,
    pub reason: String,
    pub unavailable: bool,
    pub transcript_too_long: bool,
    pub model: String,
    #[serde(default)]
    pub stage: Option<AutoModeClassifierStage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoModeToolCall {
    pub tool_name: String,
    pub input: Value,
    pub access: ToolAccess,
    pub transcript: Vec<ChatMessage>,
}

impl AutoModeToolCall {
    pub fn new(
        tool_name: impl Into<String>,
        input: Value,
        access: ToolAccess,
        transcript: Vec<ChatMessage>,
    ) -> Self {
        Self {
            tool_name: tool_name.into(),
            input,
            access,
            transcript,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AutoModeSession {
    model: String,
    config: AutoModeConfig,
    model_supported: bool,
    inner: Arc<RwLock<AutoModeSessionState>>,
}

#[derive(Debug, Clone)]
struct AutoModeSessionState {
    workspace_root: PathBuf,
    status: AutoModeStatus,
    full_prompt_injected: bool,
}

impl AutoModeSession {
    pub fn new(workspace_root: PathBuf, model: String, config: AutoModeConfig) -> Self {
        let model_supported = model_supports_auto_mode(&model);
        let status = AutoModeStatus {
            active: config.enabled && model_supported && !config.circuit_breaker_enabled,
            previous_mode: if config.enabled {
                Some("default".to_string())
            } else {
                None
            },
            model: model.clone(),
            model_supported,
            circuit_broken: config.circuit_breaker_enabled,
            stripped_dangerous_rules: if config.enabled && model_supported {
                strip_dangerous_permissions_for_auto_mode(&config.always_allow_rules)
                    .stripped_rules
                    .iter()
                    .map(PermissionRule::display_rule)
                    .collect()
            } else {
                Vec::new()
            },
        };

        Self {
            model,
            config,
            model_supported,
            inner: Arc::new(RwLock::new(AutoModeSessionState {
                workspace_root,
                status,
                full_prompt_injected: false,
            })),
        }
    }

    pub fn is_available(&self) -> bool {
        self.config.enabled && self.model_supported && !self.config.circuit_breaker_enabled
    }

    pub async fn set_workspace_root(&self, workspace_root: PathBuf) {
        self.inner.write().await.workspace_root = workspace_root;
    }

    pub async fn status(&self) -> AutoModeStatus {
        self.inner.read().await.status.clone()
    }

    pub async fn is_active(&self) -> bool {
        self.status().await.active
    }

    pub async fn enter(&self, previous_mode: impl Into<String>) -> Result<AutoModeStatus> {
        if !self.model_supported {
            return Err(anyhow!(
                "Auto Mode is not supported by model {}",
                self.model
            ));
        }
        if self.config.circuit_breaker_enabled {
            return Err(anyhow!("Auto Mode circuit breaker is enabled"));
        }

        let stripped = strip_dangerous_permissions_for_auto_mode(&self.config.always_allow_rules);
        let mut guard = self.inner.write().await;
        guard.full_prompt_injected = false;
        guard.status = AutoModeStatus {
            active: true,
            previous_mode: Some(previous_mode.into()),
            model: self.model.clone(),
            model_supported: self.model_supported,
            circuit_broken: false,
            stripped_dangerous_rules: stripped
                .stripped_rules
                .iter()
                .map(PermissionRule::display_rule)
                .collect(),
        };
        Ok(guard.status.clone())
    }

    pub async fn exit(&self) -> AutoModeStatus {
        let mut guard = self.inner.write().await;
        guard.status.active = false;
        guard.full_prompt_injected = false;
        guard.status.clone()
    }

    pub async fn decorated_system_prompt(&self, base_system_prompt: &str) -> String {
        let mut guard = self.inner.write().await;
        if !guard.status.active {
            return base_system_prompt.to_string();
        }
        let kind = if guard.full_prompt_injected {
            AutoModePromptKind::SparseReminder
        } else {
            guard.full_prompt_injected = true;
            AutoModePromptKind::FullInstructions
        };
        auto_mode_system_prompt(base_system_prompt, kind)
    }

    pub async fn classify_tool_call(&self, tool_call: AutoModeToolCall) -> AutoModeDecision {
        let guard = self.inner.read().await;
        let workspace_root = guard.workspace_root.clone();
        let active = guard.status.active;
        drop(guard);

        if !active {
            return auto_decision(
                AutoModeDecisionBehavior::Ask,
                true,
                "Auto Mode is not active; fall back to normal permission handling.",
                false,
                false,
                &self.model,
                None,
            );
        }
        if !self.is_available() {
            return auto_decision(
                AutoModeDecisionBehavior::Ask,
                true,
                format!(
                    "{} is temporarily unavailable, so auto mode cannot determine the safety of {} right now.",
                    self.model, tool_call.tool_name
                ),
                true,
                false,
                &self.model,
                None,
            );
        }
        if tool_call.transcript.len() > self.config.max_transcript_messages {
            return auto_decision(
                AutoModeDecisionBehavior::Ask,
                true,
                "Transcript is too long for Auto Mode classification; ask for permission instead.",
                false,
                true,
                &self.model,
                None,
            );
        }

        classify_with_rules(&workspace_root, &self.model, &tool_call, &self.config)
    }
}

pub fn strip_dangerous_permissions_for_auto_mode(
    rules: &[PermissionRule],
) -> AutoModeRuleStripResult {
    let mut safe_rules = Vec::new();
    let mut stripped_rules = Vec::new();

    for rule in rules {
        if rule.behavior == PermissionRuleBehavior::Allow && is_dangerous_allow_rule(rule) {
            stripped_rules.push(rule.clone());
        } else {
            safe_rules.push(rule.clone());
        }
    }

    AutoModeRuleStripResult {
        safe_rules,
        stripped_rules,
    }
}

pub fn restore_dangerous_permissions(
    mut safe_rules: Vec<PermissionRule>,
    stripped_rules: Vec<PermissionRule>,
) -> Vec<PermissionRule> {
    safe_rules.extend(stripped_rules);
    safe_rules
}

pub fn model_supports_auto_mode(model: &str) -> bool {
    let normalized = model.to_ascii_lowercase();
    normalized.contains("sonnet")
        || normalized.contains("opus")
        || normalized.contains("haiku")
        || normalized.contains("gpt-5")
        || normalized.contains("gpt-4")
        || normalized.contains("claude")
}

pub fn auto_mode_system_prompt(base_system_prompt: &str, kind: AutoModePromptKind) -> String {
    match kind {
        AutoModePromptKind::FullInstructions => format!(
            "{base_system_prompt}\n\nAuto mode is active. The user chose continuous, autonomous execution. You should:\n1. Execute immediately when the next step is clear and safe.\n2. Minimize interruptions; make ordinary implementation decisions yourself.\n3. Prefer action over planning for routine work, but still use Plan Mode for ambiguous or high-impact changes.\n4. Expect course corrections; the user can interrupt or redirect.\n5. Do not take overly destructive actions, delete user data, weaken safety controls, or modify production systems without explicit intent.\n6. Avoid data exfiltration; never intentionally reveal secrets, credentials, or private internal data."
        ),
        AutoModePromptKind::SparseReminder => format!(
            "{base_system_prompt}\n\nAuto mode still active. Execute autonomously, minimize interruptions, prefer action over planning for routine safe work, and avoid destructive or exfiltrating actions."
        ),
        AutoModePromptKind::ExitInstructions => format!(
            "{base_system_prompt}\n\nYou have exited auto mode. Ask clarifying questions when the approach is ambiguous rather than making assumptions."
        ),
    }
}

fn classify_with_rules(
    workspace_root: &Path,
    model: &str,
    tool_call: &AutoModeToolCall,
    config: &AutoModeConfig,
) -> AutoModeDecision {
    if tool_call.access == ToolAccess::ReadOnly {
        return auto_decision(
            AutoModeDecisionBehavior::Allow,
            false,
            "Read-only tools are safe for Auto Mode.",
            false,
            false,
            model,
            Some(AutoModeClassifierStage::Fast),
        );
    }
    if tool_call.access == ToolAccess::Internal {
        return auto_decision(
            AutoModeDecisionBehavior::Allow,
            false,
            "Internal safety transition tools are allowed.",
            false,
            false,
            model,
            Some(AutoModeClassifierStage::Fast),
        );
    }

    match tool_call.tool_name.as_str() {
        "file_edit" | "file_write" => classify_file_mutation(workspace_root, model, tool_call),
        "execute_command" => classify_command(workspace_root, model, tool_call, config),
        "git_operations" => classify_git_operation(workspace_root, model, tool_call),
        "task_management" | "note_edit" => auto_decision(
            AutoModeDecisionBehavior::Allow,
            false,
            "Workspace-local metadata mutation is safe for Auto Mode.",
            false,
            false,
            model,
            Some(AutoModeClassifierStage::Thinking),
        ),
        _ => auto_decision(
            AutoModeDecisionBehavior::Ask,
            true,
            format!(
                "Auto Mode has no classifier rule for tool {}; ask before executing.",
                tool_call.tool_name
            ),
            false,
            false,
            model,
            Some(AutoModeClassifierStage::Thinking),
        ),
    }
}

fn classify_file_mutation(
    workspace_root: &Path,
    model: &str,
    tool_call: &AutoModeToolCall,
) -> AutoModeDecision {
    let Some(path) = tool_call
        .input
        .get("file_path")
        .or_else(|| tool_call.input.get("path"))
        .and_then(Value::as_str)
    else {
        return auto_decision(
            AutoModeDecisionBehavior::Ask,
            true,
            "File mutation did not include a path; ask before executing.",
            false,
            false,
            model,
            Some(AutoModeClassifierStage::Thinking),
        );
    };

    if path_is_within_workspace(path, workspace_root) {
        auto_decision(
            AutoModeDecisionBehavior::Allow,
            false,
            "File mutation stays inside the workspace.",
            false,
            false,
            model,
            Some(AutoModeClassifierStage::Thinking),
        )
    } else {
        auto_decision(
            AutoModeDecisionBehavior::Deny,
            true,
            "File mutation escapes the workspace root.",
            false,
            false,
            model,
            Some(AutoModeClassifierStage::Fast),
        )
    }
}

fn classify_git_operation(
    workspace_root: &Path,
    model: &str,
    tool_call: &AutoModeToolCall,
) -> AutoModeDecision {
    if let Some(path) = tool_call.input.get("path").and_then(Value::as_str) {
        if !path_is_within_workspace(path, workspace_root) {
            return auto_decision(
                AutoModeDecisionBehavior::Deny,
                true,
                "Git operation path escapes the workspace root.",
                false,
                false,
                model,
                Some(AutoModeClassifierStage::Fast),
            );
        }
    }

    let operation = tool_call
        .input
        .get("operation")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();

    match operation.as_str() {
        "status" | "log" | "diff" => auto_decision(
            AutoModeDecisionBehavior::Allow,
            false,
            "Git read-only operation is safe for Auto Mode.",
            false,
            false,
            model,
            Some(AutoModeClassifierStage::Fast),
        ),
        "add" | "commit" | "branch" | "checkout" => auto_decision(
            AutoModeDecisionBehavior::Allow,
            false,
            "Git workspace-local mutation is allowed in Auto Mode.",
            false,
            false,
            model,
            Some(AutoModeClassifierStage::Thinking),
        ),
        "push" | "pull" => {
            if user_explicitly_requested(tool_call, &operation) {
                auto_decision(
                    AutoModeDecisionBehavior::Allow,
                    false,
                    "User explicitly requested this remote git operation.",
                    false,
                    false,
                    model,
                    Some(AutoModeClassifierStage::Thinking),
                )
            } else {
                auto_decision(
                    AutoModeDecisionBehavior::Ask,
                    true,
                    "Remote git operations require explicit user intent.",
                    false,
                    false,
                    model,
                    Some(AutoModeClassifierStage::Thinking),
                )
            }
        }
        _ => auto_decision(
            AutoModeDecisionBehavior::Ask,
            true,
            "Unknown git operation; ask before executing.",
            false,
            false,
            model,
            Some(AutoModeClassifierStage::Thinking),
        ),
    }
}

fn classify_command(
    workspace_root: &Path,
    model: &str,
    tool_call: &AutoModeToolCall,
    config: &AutoModeConfig,
) -> AutoModeDecision {
    let command = tool_call
        .input
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let cwd = tool_call
        .input
        .get("cwd")
        .and_then(Value::as_str)
        .unwrap_or_else(|| workspace_root.to_str().unwrap_or("."));

    if !path_is_within_workspace(cwd, workspace_root) {
        return auto_decision(
            AutoModeDecisionBehavior::Deny,
            true,
            "Command working directory escapes the workspace root.",
            false,
            false,
            model,
            Some(AutoModeClassifierStage::Fast),
        );
    }

    let lower = command.to_ascii_lowercase();
    if matches_user_deny_rule(&lower, config) {
        return auto_decision(
            AutoModeDecisionBehavior::Deny,
            true,
            "Command matched a user Auto Mode deny rule.",
            false,
            false,
            model,
            Some(AutoModeClassifierStage::Fast),
        );
    }
    if contains_dangerous_command(&lower) {
        return auto_decision(
            AutoModeDecisionBehavior::Deny,
            true,
            "Command matches a dangerous Auto Mode pattern.",
            false,
            false,
            model,
            Some(AutoModeClassifierStage::Fast),
        );
    }
    if is_read_only_command(&lower) {
        return auto_decision(
            AutoModeDecisionBehavior::Allow,
            false,
            "Command is a read-only inspection command.",
            false,
            false,
            model,
            Some(AutoModeClassifierStage::Fast),
        );
    }
    if is_test_lint_or_build_command(&lower) || matches_user_allow_rule(&lower, config) {
        return auto_decision(
            AutoModeDecisionBehavior::Allow,
            false,
            "Command is a local test, lint, build, or user-allowed command.",
            false,
            false,
            model,
            Some(AutoModeClassifierStage::Thinking),
        );
    }
    if lower.contains("git push") || lower.contains("git pull") {
        if user_explicitly_requested(tool_call, "push")
            || user_explicitly_requested(tool_call, "pull")
        {
            return auto_decision(
                AutoModeDecisionBehavior::Allow,
                false,
                "User explicitly requested this remote git command.",
                false,
                false,
                model,
                Some(AutoModeClassifierStage::Thinking),
            );
        }
        return auto_decision(
            AutoModeDecisionBehavior::Ask,
            true,
            "Remote git commands require explicit user intent.",
            false,
            false,
            model,
            Some(AutoModeClassifierStage::Thinking),
        );
    }

    auto_decision(
        AutoModeDecisionBehavior::Ask,
        true,
        "Auto Mode could not confidently classify this command.",
        false,
        false,
        model,
        Some(AutoModeClassifierStage::Thinking),
    )
}

fn auto_decision(
    behavior: AutoModeDecisionBehavior,
    should_block: bool,
    reason: impl Into<String>,
    unavailable: bool,
    transcript_too_long: bool,
    model: &str,
    stage: Option<AutoModeClassifierStage>,
) -> AutoModeDecision {
    AutoModeDecision {
        behavior,
        should_block,
        reason: reason.into(),
        unavailable,
        transcript_too_long,
        model: model.to_string(),
        stage,
    }
}

fn is_dangerous_allow_rule(rule: &PermissionRule) -> bool {
    let tool = rule.tool_name.to_ascii_lowercase();
    let content = rule
        .rule_content
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if tool == "agent" || tool.starts_with("agent(") {
        return true;
    }
    if !matches!(
        tool.as_str(),
        "bash" | "powershell" | "execute_command" | "shell"
    ) {
        return false;
    }

    let dangerous_fragments = [
        "python:",
        "node:",
        "ruby:",
        "perl:",
        "bash:",
        "sh:",
        "cmd:",
        "powershell:",
        "pwsh:",
        "sudo:",
        "su:",
        "eval:",
        "exec:",
        "rm -rf",
        "remove-item",
        "del /s",
        "rmdir /s",
    ];
    dangerous_fragments
        .iter()
        .any(|fragment| content.contains(fragment))
}

fn contains_dangerous_command(lower: &str) -> bool {
    let dangerous_fragments = [
        "rm -rf",
        "remove-item",
        " del /s",
        "rmdir /s",
        "format ",
        "mkfs",
        "dd if=",
        "sudo ",
        " su ",
        "chmod 777",
        "chown -r",
        "eval ",
        "bash -c",
        "sh -c",
        "cmd /c",
        "powershell -command",
        "pwsh -command",
        "curl ",
        "wget ",
        "nc ",
        "netcat ",
        "scp ",
        "ssh ",
        "python -c",
        "python3 -c",
        "node -e",
        "perl -e",
        "ruby -e",
        "openssl enc",
    ];

    dangerous_fragments
        .iter()
        .any(|fragment| lower.contains(fragment))
}

fn is_read_only_command(lower: &str) -> bool {
    let trimmed = lower.trim();
    trimmed == "pwd"
        || trimmed == "git status"
        || trimmed.starts_with("git status ")
        || trimmed.starts_with("git diff")
        || trimmed.starts_with("git log")
        || trimmed.starts_with("git show")
        || trimmed.starts_with("git rev-parse")
        || trimmed.starts_with("rg ")
        || trimmed == "rg --files"
        || trimmed.starts_with("rg --files ")
        || trimmed.starts_with("ls")
        || trimmed.starts_with("dir")
        || trimmed.starts_with("cat ")
        || trimmed.starts_with("type ")
        || trimmed.starts_with("get-childitem")
        || trimmed.starts_with("get-content ")
        || trimmed.starts_with("where ")
        || trimmed.starts_with("which ")
}

fn is_test_lint_or_build_command(lower: &str) -> bool {
    let trimmed = lower.trim();
    trimmed.starts_with("cargo test")
        || trimmed.starts_with("cargo check")
        || trimmed.starts_with("cargo build")
        || trimmed.starts_with("cargo clippy")
        || trimmed.starts_with("cargo fmt")
        || trimmed.starts_with("npm test")
        || trimmed.starts_with("npm run test")
        || trimmed.starts_with("npm run lint")
        || trimmed.starts_with("npm run build")
        || trimmed.starts_with("pnpm test")
        || trimmed.starts_with("pnpm lint")
        || trimmed.starts_with("pnpm build")
        || trimmed.starts_with("yarn test")
        || trimmed.starts_with("yarn lint")
        || trimmed.starts_with("yarn build")
        || trimmed.starts_with("pytest")
        || trimmed.starts_with("python -m pytest")
        || trimmed.starts_with("go test")
        || trimmed.starts_with("go vet")
        || trimmed.starts_with("mvn test")
        || trimmed.starts_with("gradle test")
        || trimmed.starts_with("./gradlew test")
}

fn matches_user_allow_rule(lower_command: &str, config: &AutoModeConfig) -> bool {
    config
        .user_allow_rules
        .iter()
        .any(|rule| !rule.trim().is_empty() && lower_command.contains(&rule.to_ascii_lowercase()))
}

fn matches_user_deny_rule(lower_command: &str, config: &AutoModeConfig) -> bool {
    config
        .user_deny_rules
        .iter()
        .any(|rule| !rule.trim().is_empty() && lower_command.contains(&rule.to_ascii_lowercase()))
}

fn user_explicitly_requested(tool_call: &AutoModeToolCall, keyword: &str) -> bool {
    let keyword = keyword.to_ascii_lowercase();
    tool_call.transcript.iter().rev().take(8).any(|message| {
        if message.role != "user" {
            return false;
        }
        message
            .content
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .contains(&keyword)
    })
}

fn path_is_within_workspace(raw_path: &str, workspace_root: &Path) -> bool {
    let Ok(workspace_root) = normalize_workspace_path(workspace_root) else {
        return false;
    };
    let candidate = PathBuf::from(raw_path);
    let absolute = if candidate.is_absolute() {
        candidate
    } else {
        workspace_root.join(candidate)
    };
    normalize_path_lexically(&absolute)
        .map(|path| path.starts_with(&workspace_root))
        .unwrap_or(false)
}

fn normalize_workspace_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    normalize_path_lexically(&absolute)
}

fn normalize_path_lexically(path: &Path) -> Result<PathBuf> {
    let mut normalized = PathBuf::new();

    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::Normal(segment) => normalized.push(segment),
            Component::ParentDir => {
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

fn default_max_transcript_messages() -> usize {
    DEFAULT_MAX_TRANSCRIPT_MESSAGES
}

pub fn auto_mode_tool_denial_payload(tool_name: &str, decision: &AutoModeDecision) -> Value {
    json!({
        "success": false,
        "error": format!("Auto Mode did not allow tool execution for {}: {}", tool_name, decision.reason),
        "code": match decision.behavior {
            AutoModeDecisionBehavior::Deny => "auto_mode_tool_denied",
            AutoModeDecisionBehavior::Ask | AutoModeDecisionBehavior::Allow => "auto_mode_permission_required",
        },
        "auto_mode": {
            "behavior": decision.behavior,
            "should_block": decision.should_block,
            "reason": decision.reason,
            "unavailable": decision.unavailable,
            "transcript_too_long": decision.transcript_too_long,
            "model": decision.model,
            "stage": decision.stage,
        }
    })
}
