//! General sub-agent orchestration.
//!
//! This module implements the shared AgentTool lifecycle used by normal agents,
//! coordinator mode, and SDK/headless callers: named agent definitions, sync and
//! background runs, sidechain transcripts, task output, follow-up messages, and
//! task stop semantics.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio::time::sleep;

use crate::agent_runtime::{
    AgentExecutionOutcome, AgentExecutionRequest, AgentRuntime, NoopAgentCancellation,
    NoopAgentEventHandler,
};
use crate::api::ChatMessage;
use crate::config::Settings;
use crate::fast_path::ExecutionModeHint;
use crate::tools::{Tool, ToolAccess, ToolError, ToolOutput};

const TASK_OUTPUT_POLL_INTERVAL: Duration = Duration::from_millis(20);
const DEFAULT_TASK_OUTPUT_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_MAX_TURNS: usize = 8;
const DEFAULT_AGENT_TOOLS: &[&str] = &[
    "file_read",
    "file_write",
    "file_edit",
    "search",
    "list_files",
    "execute_command",
    "git_operations",
    "task_management",
    "note_edit",
];
const READ_ONLY_TOOLS: &[&str] = &["file_read", "search", "list_files", "task_output"];
const INTERNAL_TOOLS: &[&str] = &["agent", "send_message", "task_output", "task_stop"];

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum AgentPermissionMode {
    #[default]
    Default,
    Plan,
    Auto,
    AcceptEdits,
    BypassPermissions,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum AgentIsolation {
    #[default]
    None,
    Worktree,
    Remote,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SubAgentStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Killed,
    #[default]
    Unknown,
}

impl SubAgentStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            SubAgentStatus::Completed | SubAgentStatus::Failed | SubAgentStatus::Killed
        )
    }

    fn as_str(self) -> &'static str {
        match self {
            SubAgentStatus::Pending => "pending",
            SubAgentStatus::Running => "running",
            SubAgentStatus::Completed => "completed",
            SubAgentStatus::Failed => "failed",
            SubAgentStatus::Killed => "killed",
            SubAgentStatus::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubAgentDefinition {
    pub name: String,
    pub description: String,
    pub when_to_use: String,
    pub tools: Vec<String>,
    pub model: Option<String>,
    pub permission_mode: AgentPermissionMode,
    pub background: bool,
    pub isolation: AgentIsolation,
    pub max_turns: Option<usize>,
    pub memory: Option<String>,
    pub required_mcp_servers: Vec<String>,
    pub hooks: HashMap<String, Vec<String>>,
    pub skills: Vec<String>,
    pub initial_prompt: Option<String>,
    pub system_prompt: String,
    pub source: String,
    pub base_dir: PathBuf,
}

impl Default for SubAgentDefinition {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            when_to_use: String::new(),
            tools: default_agent_tools(),
            model: None,
            permission_mode: AgentPermissionMode::Default,
            background: false,
            isolation: AgentIsolation::None,
            max_turns: None,
            memory: None,
            required_mcp_servers: Vec::new(),
            hooks: HashMap::new(),
            skills: Vec::new(),
            initial_prompt: None,
            system_prompt: String::new(),
            source: "runtime".to_string(),
            base_dir: PathBuf::new(),
        }
    }
}

impl SubAgentDefinition {
    pub fn from_markdown(path: &Path, content: &str) -> Result<Self> {
        let (frontmatter, body) = split_frontmatter(content);
        let mut definition = SubAgentDefinition {
            name: path
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("custom")
                .to_string(),
            source: path.display().to_string(),
            base_dir: path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(PathBuf::new),
            system_prompt: body.trim().to_string(),
            ..SubAgentDefinition::default()
        };

        if let Some(frontmatter) = frontmatter {
            parse_agent_frontmatter(frontmatter, &mut definition)?;
        }

        if definition.description.trim().is_empty() {
            definition.description = format!("Custom sub-agent {}", definition.name);
        }
        if definition.when_to_use.trim().is_empty() {
            definition.when_to_use = "Use when explicitly requested by name.".to_string();
        }
        Ok(definition)
    }
}

#[derive(Debug, Clone, Default)]
pub struct SubAgentRegistry {
    definitions: HashMap<String, SubAgentDefinition>,
}

impl SubAgentRegistry {
    pub fn with_builtin_agents() -> Self {
        let mut registry = Self::default();
        for definition in builtin_definitions() {
            registry.register(definition);
        }
        registry
    }

    pub async fn load_from_workspace(workspace_root: &Path) -> Result<Self> {
        Self::load_from_workspace_sync(workspace_root)
    }

    pub fn load_from_workspace_sync(workspace_root: &Path) -> Result<Self> {
        let mut registry = Self::with_builtin_agents();
        let agents_dir = workspace_root.join(".claude").join("agents");
        registry.load_from_dir_sync(&agents_dir)?;
        Ok(registry)
    }

    pub fn load_from_dir_sync(&mut self, dir: &Path) -> Result<()> {
        if !dir.exists() {
            return Ok(());
        }

        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
                continue;
            }
            let content = std::fs::read_to_string(&path)?;
            match SubAgentDefinition::from_markdown(&path, &content) {
                Ok(definition) => self.register(definition),
                Err(error) => {
                    tracing::warn!(
                        "skipping invalid sub-agent definition {}: {}",
                        path.display(),
                        error
                    );
                }
            }
        }
        Ok(())
    }

    pub fn register(&mut self, definition: SubAgentDefinition) {
        self.definitions
            .insert(normalize_agent_name(&definition.name), definition);
    }

    pub fn get(&self, name: &str) -> Option<SubAgentDefinition> {
        self.definitions.get(&normalize_agent_name(name)).cloned()
    }

    pub fn list(&self) -> Vec<SubAgentDefinition> {
        let mut definitions = self.definitions.values().cloned().collect::<Vec<_>>();
        definitions.sort_by(|left, right| left.name.cmp(&right.name));
        definitions
    }
}

#[derive(Debug, Clone)]
pub struct SubAgentManagerConfig {
    pub workspace_root: PathBuf,
    pub state_root: PathBuf,
    pub fork_subagents_enabled: bool,
    pub coordinator_context: bool,
    pub max_background_tasks: usize,
}

impl SubAgentManagerConfig {
    pub fn for_workspace(workspace_root: PathBuf) -> Self {
        let state_root = workspace_root.join(".claude").join("agents-runtime");
        Self {
            workspace_root,
            state_root,
            fork_subagents_enabled: is_env_truthy("FORK_SUBAGENT")
                || is_env_truthy("CLAUDE_CODE_FORK_SUBAGENT"),
            coordinator_context: false,
            max_background_tasks: 16,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct AgentLaunchRequest {
    pub subagent_type: String,
    pub description: String,
    pub prompt: String,
    pub run_in_background: Option<bool>,
    pub fork: bool,
    pub cwd: Option<PathBuf>,
    pub output_file: Option<PathBuf>,
    pub isolation: Option<AgentIsolation>,
    pub max_turns: Option<usize>,
    pub model: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SendMessageRequest {
    pub to: String,
    pub message: String,
    pub block: bool,
    pub timeout: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentTaskSnapshot {
    pub task_id: String,
    pub name: String,
    pub description: String,
    pub status: SubAgentStatus,
    pub result: Option<String>,
    pub queued: bool,
    pub output_file: Option<PathBuf>,
    pub sidechain_transcript: Option<PathBuf>,
    pub total_tokens: usize,
    pub tool_uses: usize,
    pub duration_ms: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubAgentNotification {
    pub task_id: String,
    pub status: String,
    pub summary: String,
    pub result: String,
    pub total_tokens: usize,
    pub tool_uses: usize,
    pub duration_ms: u64,
}

#[derive(Debug, Clone)]
pub struct SubAgentRunRequest {
    pub task_id: String,
    pub definition: SubAgentDefinition,
    pub description: String,
    pub history: Vec<ChatMessage>,
    pub workspace_root: PathBuf,
    pub output_file: Option<PathBuf>,
    pub sidechain_transcript: Option<PathBuf>,
    pub forked: bool,
}

#[derive(Debug, Clone)]
pub struct SubAgentRunResult {
    pub answer: String,
    pub total_tokens: usize,
    pub tool_uses: usize,
    pub history: Vec<ChatMessage>,
}

#[async_trait]
pub trait SubAgentRunner: Send + Sync {
    async fn run(&self, request: SubAgentRunRequest) -> Result<SubAgentRunResult>;
}

pub struct RuntimeSubAgentRunner {
    settings: Settings,
}

impl RuntimeSubAgentRunner {
    pub fn new(settings: Settings) -> Self {
        Self { settings }
    }
}

#[async_trait]
impl SubAgentRunner for RuntimeSubAgentRunner {
    async fn run(&self, request: SubAgentRunRequest) -> Result<SubAgentRunResult> {
        let mut settings = self.settings.clone();
        settings.working_dir = request.workspace_root.clone();
        if let Some(model) = &request.definition.model {
            settings.model = model.clone();
        }
        if request.definition.permission_mode == AgentPermissionMode::Auto {
            settings.safety.auto_mode = true;
        }

        let runtime = AgentRuntime::new(settings);
        let outcome = runtime
            .execute(
                AgentExecutionRequest {
                    system_prompt: render_agent_system_prompt(&request.definition, request.forked),
                    history: request.history.clone(),
                    workspace_root: request.workspace_root,
                    already_surfaced_memory_paths: Vec::new(),
                    max_iterations: request.definition.max_turns.unwrap_or(DEFAULT_MAX_TURNS),
                    execution_mode_hint: ExecutionModeHint::Auto,
                    token_budget_state: None,
                    additional_system_sections: Vec::new(),
                    additional_user_context_sections: Vec::new(),
                    allowed_tool_names: Some(effective_allowed_tools(&request.definition)),
                },
                &NoopAgentEventHandler,
                &NoopAgentCancellation,
            )
            .await?;

        match outcome {
            AgentExecutionOutcome::Completed(result) => {
                let mut history = request.history;
                history.push(ChatMessage::assistant(result.answer.clone()));
                Ok(SubAgentRunResult {
                    answer: result.answer,
                    total_tokens: result
                        .usage_records
                        .iter()
                        .map(|usage| usage.total_tokens)
                        .sum(),
                    tool_uses: 0,
                    history,
                })
            }
            AgentExecutionOutcome::Cancelled => Err(anyhow!("sub_agent_cancelled")),
        }
    }
}

#[derive(Clone)]
pub struct SubAgentManager {
    inner: Arc<SubAgentManagerInner>,
}

struct SubAgentManagerInner {
    config: SubAgentManagerConfig,
    catalog: String,
    registry: RwLock<SubAgentRegistry>,
    runner: Arc<dyn SubAgentRunner>,
    tasks: RwLock<HashMap<String, SubAgentTaskRecord>>,
    handles: Mutex<HashMap<String, JoinHandle<()>>>,
    notifications: RwLock<Vec<SubAgentNotification>>,
}

#[derive(Debug, Clone)]
struct SubAgentTaskRecord {
    task_id: String,
    definition: SubAgentDefinition,
    description: String,
    status: SubAgentStatus,
    history: Vec<ChatMessage>,
    pending_messages: Vec<String>,
    result: Option<String>,
    queued: bool,
    workspace_root: PathBuf,
    output_file: Option<PathBuf>,
    sidechain_transcript: Option<PathBuf>,
    forked: bool,
    turns_completed: usize,
    total_tokens: usize,
    tool_uses: usize,
    duration_ms: u64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl SubAgentManager {
    pub fn new(
        config: SubAgentManagerConfig,
        registry: SubAgentRegistry,
        runner: Arc<dyn SubAgentRunner>,
    ) -> Self {
        let catalog = render_agent_catalog(&registry.list());
        Self {
            inner: Arc::new(SubAgentManagerInner {
                config,
                catalog,
                registry: RwLock::new(registry),
                runner,
                tasks: RwLock::new(HashMap::new()),
                handles: Mutex::new(HashMap::new()),
                notifications: RwLock::new(Vec::new()),
            }),
        }
    }

    pub fn from_settings(settings: Settings) -> Result<Self> {
        let config = SubAgentManagerConfig::for_workspace(settings.working_dir.clone());
        let registry = SubAgentRegistry::load_from_workspace_sync(&settings.working_dir)?;
        Ok(Self::new(
            config,
            registry,
            Arc::new(RuntimeSubAgentRunner::new(settings)),
        ))
    }

    pub async fn list_agents(&self) -> Vec<SubAgentDefinition> {
        self.inner.registry.read().await.list()
    }

    pub fn catalog(&self) -> &str {
        &self.inner.catalog
    }

    pub async fn launch(&self, request: AgentLaunchRequest) -> Result<SubAgentTaskSnapshot> {
        validate_launch_request(&request)?;
        let definition = self
            .inner
            .registry
            .read()
            .await
            .get(&request.subagent_type)
            .ok_or_else(|| anyhow!("sub_agent_not_found: {}", request.subagent_type))?;

        if request.fork
            && (!self.inner.config.fork_subagents_enabled
                || self.inner.config.coordinator_context
                || request.run_in_background.unwrap_or(definition.background))
        {
            return Err(anyhow!(
                "fork sub-agents are disabled for this context or request"
            ));
        }

        let task_id = format!("agent-{}", uuid::Uuid::new_v4().simple());
        let isolation = request.isolation.unwrap_or(definition.isolation);
        let workspace_root = self
            .prepare_workspace(&task_id, isolation, request.cwd.as_ref())
            .await?;
        let output_file = Some(resolve_output_file(
            &self.inner.config,
            &task_id,
            request.output_file.as_ref(),
        ));
        let sidechain_transcript = Some(
            self.inner
                .config
                .state_root
                .join("transcripts")
                .join(format!("{}.jsonl", task_id)),
        );
        let mut run_definition = definition.clone();
        if let Some(max_turns) = request.max_turns {
            run_definition.max_turns = Some(max_turns);
        }
        if let Some(model) = request.model {
            run_definition.model = Some(model);
        }
        let mut history = Vec::new();
        if let Some(initial_prompt) = &run_definition.initial_prompt {
            history.push(ChatMessage::user(initial_prompt.clone()));
        }
        history.push(ChatMessage::user(request.prompt.clone()));

        let now = Utc::now();
        let record = SubAgentTaskRecord {
            task_id: task_id.clone(),
            definition: run_definition,
            description: request.description,
            status: SubAgentStatus::Running,
            history,
            pending_messages: Vec::new(),
            result: None,
            queued: false,
            workspace_root,
            output_file,
            sidechain_transcript,
            forked: request.fork,
            turns_completed: 0,
            total_tokens: 0,
            tool_uses: 0,
            duration_ms: 0,
            created_at: now,
            updated_at: now,
        };

        self.inner
            .tasks
            .write()
            .await
            .insert(task_id.clone(), record);
        self.append_transcript_event(&task_id, "task_started", json!({}))
            .await?;

        if request.run_in_background.unwrap_or(definition.background) {
            self.spawn_background_lifecycle(task_id.clone()).await;
            return self.snapshot(&task_id, false).await;
        }

        self.run_task_lifecycle(task_id.clone()).await;
        self.snapshot(&task_id, false).await
    }

    pub async fn send_message(&self, request: SendMessageRequest) -> Result<SubAgentTaskSnapshot> {
        if request.message.trim().is_empty() {
            return Err(anyhow!("message is required"));
        }
        let task_id = self.resolve_target(&request.to).await?;
        let status = {
            let mut tasks = self.inner.tasks.write().await;
            let task = tasks
                .get_mut(&task_id)
                .ok_or_else(|| anyhow!("sub_agent_not_found: {}", task_id))?;
            if task.status == SubAgentStatus::Killed {
                return Err(anyhow!("cannot message killed sub-agent: {}", task_id));
            }
            task.pending_messages.push(request.message.clone());
            task.queued = true;
            task.updated_at = Utc::now();
            task.status
        };
        self.append_transcript_event(
            &task_id,
            "message_queued",
            json!({ "message": request.message }),
        )
        .await?;

        if status == SubAgentStatus::Running {
            return self.snapshot(&task_id, true).await;
        }

        {
            let mut tasks = self.inner.tasks.write().await;
            if let Some(task) = tasks.get_mut(&task_id) {
                task.status = SubAgentStatus::Running;
                task.result = None;
                task.updated_at = Utc::now();
            }
        }

        if request.block {
            self.run_task_lifecycle(task_id.clone()).await;
            self.task_output(&task_id, true, request.timeout).await
        } else {
            self.spawn_background_lifecycle(task_id.clone()).await;
            self.snapshot(&task_id, true).await
        }
    }

    pub async fn task_output(
        &self,
        task_id: &str,
        block: bool,
        timeout: Duration,
    ) -> Result<SubAgentTaskSnapshot> {
        let timeout = if timeout.is_zero() {
            DEFAULT_TASK_OUTPUT_TIMEOUT
        } else {
            timeout
        };
        let started = Instant::now();
        loop {
            let snapshot = self.snapshot(task_id, false).await?;
            if snapshot.status.is_terminal() || !block {
                return Ok(snapshot);
            }
            if started.elapsed() >= timeout {
                return Ok(snapshot);
            }
            sleep(TASK_OUTPUT_POLL_INTERVAL).await;
        }
    }

    pub async fn task_stop(
        &self,
        task_id: &str,
        reason: Option<String>,
    ) -> Result<SubAgentTaskSnapshot> {
        let reason = reason.unwrap_or_else(|| "Stopped by caller".to_string());
        {
            let mut handles = self.inner.handles.lock().await;
            if let Some(handle) = handles.remove(task_id) {
                handle.abort();
            }
        }
        {
            let mut tasks = self.inner.tasks.write().await;
            let task = tasks
                .get_mut(task_id)
                .ok_or_else(|| anyhow!("sub_agent_not_found: {}", task_id))?;
            task.status = SubAgentStatus::Killed;
            task.result = Some(reason.clone());
            task.updated_at = Utc::now();
        }
        self.append_transcript_event(task_id, "task_killed", json!({ "reason": reason }))
            .await?;
        let snapshot = self.snapshot(task_id, false).await?;
        self.enqueue_notification(&snapshot).await;
        Ok(snapshot)
    }

    pub async fn drain_notifications(&self) -> Vec<SubAgentNotification> {
        let mut notifications = self.inner.notifications.write().await;
        std::mem::take(&mut *notifications)
    }

    async fn spawn_background_lifecycle(&self, task_id: String) {
        let manager = self.clone();
        let handle_task_id = task_id.clone();
        let handle = tokio::spawn(async move {
            manager.run_task_lifecycle(handle_task_id.clone()).await;
            manager.inner.handles.lock().await.remove(&handle_task_id);
        });
        self.inner.handles.lock().await.insert(task_id, handle);
    }

    async fn run_task_lifecycle(&self, task_id: String) {
        loop {
            let request = match self.next_run_request(&task_id).await {
                Ok(Some(request)) => request,
                Ok(None) => return,
                Err(error) => {
                    self.fail_task(&task_id, error.to_string()).await;
                    return;
                }
            };
            let started = Instant::now();
            let result = self.inner.runner.run(request).await;
            match result {
                Ok(result) => {
                    if self
                        .finish_successful_turn(&task_id, result, started.elapsed())
                        .await
                    {
                        continue;
                    }
                    if let Ok(snapshot) = self.snapshot(&task_id, false).await {
                        self.enqueue_notification(&snapshot).await;
                    }
                    return;
                }
                Err(error) => {
                    self.fail_task(&task_id, error.to_string()).await;
                    return;
                }
            }
        }
    }

    async fn next_run_request(&self, task_id: &str) -> Result<Option<SubAgentRunRequest>> {
        let mut tasks = self.inner.tasks.write().await;
        let task = tasks
            .get_mut(task_id)
            .ok_or_else(|| anyhow!("sub_agent_not_found: {}", task_id))?;
        if task.status == SubAgentStatus::Killed {
            return Ok(None);
        }
        if task.turns_completed > 0 && !task.pending_messages.is_empty() {
            let messages = std::mem::take(&mut task.pending_messages);
            for message in messages {
                task.history.push(ChatMessage::user(message));
            }
            task.queued = false;
        }
        task.status = SubAgentStatus::Running;
        task.updated_at = Utc::now();
        Ok(Some(SubAgentRunRequest {
            task_id: task.task_id.clone(),
            definition: task.definition.clone(),
            description: task.description.clone(),
            history: task.history.clone(),
            workspace_root: task.workspace_root.clone(),
            output_file: task.output_file.clone(),
            sidechain_transcript: task.sidechain_transcript.clone(),
            forked: task.forked,
        }))
    }

    async fn finish_successful_turn(
        &self,
        task_id: &str,
        result: SubAgentRunResult,
        elapsed: Duration,
    ) -> bool {
        let should_continue = {
            let mut tasks = self.inner.tasks.write().await;
            let Some(task) = tasks.get_mut(task_id) else {
                return false;
            };
            if task.status == SubAgentStatus::Killed {
                return false;
            }
            task.total_tokens += result.total_tokens;
            task.tool_uses += result.tool_uses;
            task.duration_ms += elapsed.as_millis() as u64;
            task.turns_completed += 1;
            task.history = result.history;
            task.result = Some(result.answer);
            task.status = SubAgentStatus::Completed;
            task.updated_at = Utc::now();
            let should_continue = !task.pending_messages.is_empty();
            if should_continue {
                task.status = SubAgentStatus::Running;
                task.result = None;
            }
            should_continue
        };

        let _ = self.persist_task_artifacts(task_id).await;
        should_continue
    }

    async fn fail_task(&self, task_id: &str, error: String) {
        {
            let mut tasks = self.inner.tasks.write().await;
            if let Some(task) = tasks.get_mut(task_id) {
                if task.status != SubAgentStatus::Killed {
                    task.status = SubAgentStatus::Failed;
                    task.result = Some(error.clone());
                    task.updated_at = Utc::now();
                }
            }
        }
        let _ = self
            .append_transcript_event(task_id, "task_failed", json!({ "error": error }))
            .await;
        if let Ok(snapshot) = self.snapshot(task_id, false).await {
            self.enqueue_notification(&snapshot).await;
        }
    }

    async fn snapshot(&self, task_id: &str, queued: bool) -> Result<SubAgentTaskSnapshot> {
        let tasks = self.inner.tasks.read().await;
        let task = tasks
            .get(task_id)
            .ok_or_else(|| anyhow!("sub_agent_not_found: {}", task_id))?;
        Ok(SubAgentTaskSnapshot {
            task_id: task.task_id.clone(),
            name: task.definition.name.clone(),
            description: task.description.clone(),
            status: task.status,
            result: task.result.clone(),
            queued: queued || task.queued,
            output_file: task.output_file.clone(),
            sidechain_transcript: task.sidechain_transcript.clone(),
            total_tokens: task.total_tokens,
            tool_uses: task.tool_uses,
            duration_ms: task.duration_ms,
            created_at: task.created_at,
            updated_at: task.updated_at,
        })
    }

    async fn resolve_target(&self, to: &str) -> Result<String> {
        let tasks = self.inner.tasks.read().await;
        if tasks.contains_key(to) {
            return Ok(to.to_string());
        }

        let matches = tasks
            .values()
            .filter(|task| task.definition.name == to || task.description == to)
            .map(|task| task.task_id.clone())
            .collect::<Vec<_>>();
        match matches.len() {
            1 => Ok(matches[0].clone()),
            0 => Err(anyhow!("sub_agent_not_found: {}", to)),
            _ => Err(anyhow!("ambiguous_sub_agent_target: {}", to)),
        }
    }

    async fn prepare_workspace(
        &self,
        task_id: &str,
        isolation: AgentIsolation,
        cwd: Option<&PathBuf>,
    ) -> Result<PathBuf> {
        if isolation == AgentIsolation::Worktree && cwd.is_some() {
            return Err(anyhow!(
                "cwd cannot be combined with worktree isolation; choose one execution root"
            ));
        }

        if let Some(cwd) = cwd {
            return Ok(resolve_workspace_path(
                &self.inner.config.workspace_root,
                cwd,
            ));
        }

        match isolation {
            AgentIsolation::None => Ok(self.inner.config.workspace_root.clone()),
            AgentIsolation::Remote => Err(anyhow!(
                "remote sub-agent isolation is not configured in this runtime"
            )),
            AgentIsolation::Worktree => {
                let worktree_root = self.inner.config.state_root.join("worktrees").join(task_id);
                tokio::fs::create_dir_all(&worktree_root).await?;
                if self.inner.config.workspace_root.join(".git").exists() {
                    create_git_worktree(&self.inner.config.workspace_root, &worktree_root).await?;
                }
                Ok(worktree_root)
            }
        }
    }

    async fn append_transcript_event(
        &self,
        task_id: &str,
        kind: &str,
        payload: Value,
    ) -> Result<()> {
        let transcript_path = {
            let tasks = self.inner.tasks.read().await;
            tasks
                .get(task_id)
                .and_then(|task| task.sidechain_transcript.clone())
        };
        let Some(transcript_path) = transcript_path else {
            return Ok(());
        };
        if let Some(parent) = transcript_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let line = json!({
            "timestamp": Utc::now(),
            "task_id": task_id,
            "kind": kind,
            "payload": payload,
        })
        .to_string();
        use tokio::io::AsyncWriteExt;
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&transcript_path)
            .await?;
        file.write_all(line.as_bytes()).await?;
        file.write_all(b"\n").await?;
        Ok(())
    }

    async fn persist_task_artifacts(&self, task_id: &str) -> Result<()> {
        let snapshot = self.snapshot(task_id, false).await?;
        if let Some(output_file) = &snapshot.output_file {
            if let Some(parent) = output_file.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(
                output_file,
                format!(
                    "# Sub-agent output\n\n- task_id: {}\n- agent: {}\n- status: {}\n\n{}",
                    snapshot.task_id,
                    snapshot.name,
                    snapshot.status.as_str(),
                    snapshot.result.clone().unwrap_or_default()
                ),
            )
            .await?;
        }
        self.append_transcript_event(
            task_id,
            "task_completed",
            json!({
                "status": snapshot.status.as_str(),
                "result": snapshot.result,
                "total_tokens": snapshot.total_tokens,
                "tool_uses": snapshot.tool_uses,
                "duration_ms": snapshot.duration_ms,
            }),
        )
        .await
    }

    async fn enqueue_notification(&self, snapshot: &SubAgentTaskSnapshot) {
        self.inner
            .notifications
            .write()
            .await
            .push(SubAgentNotification {
                task_id: snapshot.task_id.clone(),
                status: snapshot.status.as_str().to_string(),
                summary: format!(
                    "Agent \"{}\" {}",
                    snapshot.description,
                    snapshot.status.as_str()
                ),
                result: snapshot.result.clone().unwrap_or_default(),
                total_tokens: snapshot.total_tokens,
                tool_uses: snapshot.tool_uses,
                duration_ms: snapshot.duration_ms,
            });
    }
}

pub fn sub_agent_tools(settings: Settings) -> Result<(SubAgentManager, Vec<Box<dyn Tool>>)> {
    let manager = SubAgentManager::from_settings(settings)?;
    let tools = sub_agent_tools_with_manager(manager.clone());
    Ok((manager, tools))
}

pub fn sub_agent_tools_with_manager(manager: SubAgentManager) -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(AgentTool::new(manager.clone())),
        Box::new(SendMessageTool::new(manager.clone())),
        Box::new(TaskOutputTool::new(manager.clone())),
        Box::new(TaskStopTool::new(manager)),
    ]
}

pub fn render_sub_agent_notification(notification: &SubAgentNotification) -> String {
    format!(
        "<task-notification>\n  <task-id>{}</task-id>\n  <status>{}</status>\n  <summary>{}</summary>\n  <result>{}</result>\n  <usage>\n    <total_tokens>{}</total_tokens>\n    <tool_uses>{}</tool_uses>\n    <duration_ms>{}</duration_ms>\n  </usage>\n</task-notification>",
        escape_xml(&notification.task_id),
        escape_xml(&notification.status),
        escape_xml(&notification.summary),
        escape_xml(&notification.result),
        notification.total_tokens,
        notification.tool_uses,
        notification.duration_ms
    )
}

pub fn render_agent_catalog(definitions: &[SubAgentDefinition]) -> String {
    let rows = definitions
        .iter()
        .map(|definition| {
            format!(
                "- `{}`: {} Tools: {}. Use when: {}.",
                definition.name,
                definition.description,
                definition.tools.join(", "),
                definition.when_to_use
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "Sub-Agent Catalog\n{}\n\nUse `agent` for delegated work, `send_message` for follow-ups, `task_output` for background results, and `task_stop` to cancel or redirect.",
        rows
    )
}

struct AgentTool {
    manager: SubAgentManager,
    description: String,
}

impl AgentTool {
    fn new(manager: SubAgentManager) -> Self {
        let description = format!(
            "Run a named sub-agent, either synchronously or in the background.\n{}",
            manager.catalog()
        );
        Self {
            manager,
            description,
        }
    }
}

#[async_trait]
impl Tool for AgentTool {
    fn name(&self) -> &str {
        "agent"
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "subagent_type": { "type": "string", "description": "Named agent such as general-purpose, explore, plan, verification, worker, or a custom .claude/agents/*.md name" },
                "description": { "type": "string", "description": "Short task title shown in notifications" },
                "prompt": { "type": "string", "description": "Precise task instructions for the sub-agent" },
                "run_in_background": { "type": "boolean", "description": "Start asynchronously and return a task id immediately" },
                "fork": { "type": "boolean", "description": "Use fork-style inheritance when the feature gate allows it" },
                "cwd": { "type": "string", "description": "Optional execution directory; do not combine with worktree isolation" },
                "output_file": { "type": "string", "description": "Optional file path where final background output is written" },
                "isolation": { "type": "string", "enum": ["none", "worktree", "remote"], "description": "Execution isolation policy" },
                "max_turns": { "type": "integer", "description": "Override maximum agent turns" },
                "model": { "type": "string", "description": "Optional model override" }
            },
            "required": ["subagent_type", "description", "prompt"]
        })
    }

    async fn execute(&self, input: Value) -> std::result::Result<ToolOutput, ToolError> {
        let request = AgentLaunchRequest {
            subagent_type: required_string(&input, "subagent_type")?,
            description: required_string(&input, "description")?,
            prompt: required_string(&input, "prompt")?,
            run_in_background: input.get("run_in_background").and_then(Value::as_bool),
            fork: input.get("fork").and_then(Value::as_bool).unwrap_or(false),
            cwd: optional_path(&input, "cwd"),
            output_file: optional_path(&input, "output_file"),
            isolation: optional_isolation(&input)?,
            max_turns: input
                .get("max_turns")
                .and_then(Value::as_u64)
                .map(|value| value as usize),
            model: input
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string),
        };

        let snapshot = self.manager.launch(request).await.map_err(tool_error)?;
        Ok(json_tool_output(json!({
            "success": true,
            "task_id": snapshot.task_id,
            "agent": snapshot.name,
            "status": snapshot.status.as_str(),
            "result": snapshot.result,
            "queued": snapshot.queued,
            "output_file": snapshot.output_file,
            "sidechain_transcript": snapshot.sidechain_transcript,
            "total_tokens": snapshot.total_tokens,
            "tool_uses": snapshot.tool_uses,
            "duration_ms": snapshot.duration_ms
        })))
    }

    fn access(&self) -> ToolAccess {
        ToolAccess::Internal
    }
}

struct SendMessageTool {
    manager: SubAgentManager,
}

impl SendMessageTool {
    fn new(manager: SubAgentManager) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for SendMessageTool {
    fn name(&self) -> &str {
        "send_message"
    }

    fn description(&self) -> &str {
        "Send a follow-up message to a running or completed sub-agent"
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "to": { "type": "string", "description": "Task id, agent name, or unique task description" },
                "message": { "type": "string", "description": "Follow-up instruction" },
                "block": { "type": "boolean", "description": "Wait for a terminal resumed result when the agent is not currently running" },
                "timeout_ms": { "type": "integer", "description": "Maximum wait time for block=true" }
            },
            "required": ["to", "message"]
        })
    }

    async fn execute(&self, input: Value) -> std::result::Result<ToolOutput, ToolError> {
        let request = SendMessageRequest {
            to: required_string(&input, "to")?,
            message: required_string(&input, "message")?,
            block: input.get("block").and_then(Value::as_bool).unwrap_or(false),
            timeout: input
                .get("timeout_ms")
                .and_then(Value::as_u64)
                .map(Duration::from_millis)
                .unwrap_or(DEFAULT_TASK_OUTPUT_TIMEOUT),
        };
        let snapshot = self
            .manager
            .send_message(request)
            .await
            .map_err(tool_error)?;
        Ok(json_tool_output(json!({
            "success": true,
            "task_id": snapshot.task_id,
            "status": snapshot.status.as_str(),
            "queued": snapshot.queued,
            "result": snapshot.result,
            "output_file": snapshot.output_file,
            "sidechain_transcript": snapshot.sidechain_transcript
        })))
    }

    fn access(&self) -> ToolAccess {
        ToolAccess::Internal
    }
}

struct TaskOutputTool {
    manager: SubAgentManager,
}

impl TaskOutputTool {
    fn new(manager: SubAgentManager) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for TaskOutputTool {
    fn name(&self) -> &str {
        "task_output"
    }

    fn description(&self) -> &str {
        "Read or optionally wait for a background sub-agent output"
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "description": "Task id returned by agent" },
                "block": { "type": "boolean", "description": "Wait until the task reaches a terminal state" },
                "timeout_ms": { "type": "integer", "description": "Maximum wait time when block=true" }
            },
            "required": ["task_id"]
        })
    }

    async fn execute(&self, input: Value) -> std::result::Result<ToolOutput, ToolError> {
        let task_id = required_string(&input, "task_id")?;
        let snapshot = self
            .manager
            .task_output(
                &task_id,
                input.get("block").and_then(Value::as_bool).unwrap_or(false),
                input
                    .get("timeout_ms")
                    .and_then(Value::as_u64)
                    .map(Duration::from_millis)
                    .unwrap_or(DEFAULT_TASK_OUTPUT_TIMEOUT),
            )
            .await
            .map_err(tool_error)?;
        Ok(json_tool_output(json!({
            "success": true,
            "task_id": snapshot.task_id,
            "status": snapshot.status.as_str(),
            "result": snapshot.result,
            "output_file": snapshot.output_file,
            "sidechain_transcript": snapshot.sidechain_transcript,
            "total_tokens": snapshot.total_tokens,
            "tool_uses": snapshot.tool_uses,
            "duration_ms": snapshot.duration_ms
        })))
    }

    fn access(&self) -> ToolAccess {
        ToolAccess::Internal
    }
}

struct TaskStopTool {
    manager: SubAgentManager,
}

impl TaskStopTool {
    fn new(manager: SubAgentManager) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for TaskStopTool {
    fn name(&self) -> &str {
        "task_stop"
    }

    fn description(&self) -> &str {
        "Stop a background sub-agent and prevent further messages"
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "description": "Task id returned by agent" },
                "reason": { "type": "string", "description": "Optional stop reason" }
            },
            "required": ["task_id"]
        })
    }

    async fn execute(&self, input: Value) -> std::result::Result<ToolOutput, ToolError> {
        let task_id = required_string(&input, "task_id")?;
        let snapshot = self
            .manager
            .task_stop(
                &task_id,
                input
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            )
            .await
            .map_err(tool_error)?;
        Ok(json_tool_output(json!({
            "success": true,
            "task_id": snapshot.task_id,
            "status": snapshot.status.as_str(),
            "result": snapshot.result
        })))
    }

    fn access(&self) -> ToolAccess {
        ToolAccess::Internal
    }
}

fn builtin_definitions() -> Vec<SubAgentDefinition> {
    vec![
        builtin_agent(
            "general-purpose",
            "Handles general delegated tasks with broad project tools",
            "Use for a concrete task that does not need a specialized role.",
            DEFAULT_AGENT_TOOLS,
            "You are a general-purpose project sub-agent. Complete the delegated task and report concise results.",
        ),
        builtin_agent(
            "worker",
            "Executes a concrete assignment from a coordinator",
            "Use for implementation, exploration, or verification delegated by a coordinator.",
            &[
                "execute_command",
                "file_read",
                "file_edit",
                "file_write",
                "search",
                "list_files",
                "git_operations",
                "task_management",
                "note_edit",
            ],
            "You are a worker sub-agent. Do not spawn nested agents unless explicitly allowed by the caller.",
        ),
        builtin_agent(
            "explore",
            "Explores and analyzes codebases",
            "Use when you need to inspect files, find symbols, or understand architecture.",
            &["file_read", "search", "list_files"],
            "You are an exploration sub-agent. Gather facts, cite file paths, and avoid edits.",
        ),
        builtin_agent(
            "plan",
            "Creates implementation plans and risk breakdowns",
            "Use when complex work needs sequencing before edits.",
            &["file_read", "search", "list_files"],
            "You are a planning sub-agent. Produce an actionable plan with dependencies and risks.",
        ),
        builtin_agent(
            "verification",
            "Verifies implementations and test results",
            "Use after changes to validate behavior and report failures.",
            &["file_read", "search", "execute_command"],
            "You are a verification sub-agent. Run targeted checks and report exact evidence.",
        ),
        builtin_agent(
            "claude-code-guide",
            "Explains Claude Code usage and workflows",
            "Use when the user asks about Claude Code behavior or commands.",
            &["file_read", "search"],
            "You are a Claude Code guide sub-agent. Explain workflows clearly and practically.",
        ),
    ]
}

fn builtin_agent(
    name: &str,
    description: &str,
    when_to_use: &str,
    tools: &[&str],
    system_prompt: &str,
) -> SubAgentDefinition {
    SubAgentDefinition {
        name: name.to_string(),
        description: description.to_string(),
        when_to_use: when_to_use.to_string(),
        tools: tools.iter().map(|tool| tool.to_string()).collect(),
        model: Some("sonnet".to_string()),
        system_prompt: system_prompt.to_string(),
        source: "built-in".to_string(),
        base_dir: PathBuf::from("built-in"),
        ..SubAgentDefinition::default()
    }
}

fn render_agent_system_prompt(definition: &SubAgentDefinition, forked: bool) -> String {
    let mut prompt = definition.system_prompt.clone();
    prompt.push_str("\n\nSub-Agent Runtime\n");
    prompt.push_str(&format!("- Agent name: {}\n", definition.name));
    prompt.push_str(&format!(
        "- Permission mode: {:?}\n",
        definition.permission_mode
    ));
    prompt.push_str(&format!("- Isolation: {:?}\n", definition.isolation));
    if forked {
        prompt.push_str(
            "- Fork mode: exact parent context/tool inheritance was requested by the caller.\n",
        );
    }
    if !definition.required_mcp_servers.is_empty() {
        prompt.push_str(&format!(
            "- Required MCP servers: {}\n",
            definition.required_mcp_servers.join(", ")
        ));
    }
    if !definition.skills.is_empty() {
        prompt.push_str(&format!("- Skills: {}\n", definition.skills.join(", ")));
    }
    if !definition.hooks.is_empty() {
        prompt.push_str(
            "- Hooks are declared for this agent; honor their intent when deciding tool use.\n",
        );
    }
    if let Some(memory) = &definition.memory {
        prompt.push_str(&format!("- Memory scope: {}\n", memory));
    }
    prompt
}

fn effective_allowed_tools(definition: &SubAgentDefinition) -> Vec<String> {
    let mut tools = if definition.tools.is_empty() {
        default_agent_tools()
    } else {
        definition.tools.clone()
    };
    tools.retain(|tool| !INTERNAL_TOOLS.contains(&tool.as_str()));
    if definition.permission_mode == AgentPermissionMode::Plan {
        tools.retain(|tool| READ_ONLY_TOOLS.contains(&tool.as_str()));
    }
    tools.sort();
    tools.dedup();
    tools
}

fn split_frontmatter(content: &str) -> (Option<&str>, &str) {
    let trimmed = content.strip_prefix('\u{feff}').unwrap_or(content);
    if !trimmed.starts_with("---") {
        return (None, trimmed);
    }
    let Some(rest) = trimmed.strip_prefix("---") else {
        return (None, trimmed);
    };
    let rest = rest
        .strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'))
        .unwrap_or(rest);
    if let Some(index) = rest.find("\n---") {
        let frontmatter = &rest[..index];
        let body = &rest[index + "\n---".len()..];
        let body = body
            .strip_prefix("\r\n")
            .or_else(|| body.strip_prefix('\n'))
            .unwrap_or(body);
        (Some(frontmatter), body)
    } else {
        (None, trimmed)
    }
}

fn parse_agent_frontmatter(frontmatter: &str, definition: &mut SubAgentDefinition) -> Result<()> {
    let mut current_key: Option<String> = None;
    let mut current_hook: Option<String> = None;
    for raw_line in frontmatter.lines() {
        let line = raw_line.trim_end();
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if !line.starts_with(' ') && !line.starts_with('\t') {
            let (key, value) = split_key_value(trimmed)?;
            current_key = Some(key.to_string());
            current_hook = None;
            if !value.is_empty() {
                apply_frontmatter_scalar(definition, key, value)?;
                current_key = None;
            } else if key != "hooks" {
                clear_collection(definition, key);
            }
            continue;
        }

        let Some(key) = current_key.as_deref() else {
            continue;
        };
        if key == "hooks" && line.starts_with("  ") && !line.starts_with("    ") {
            let (hook_name, value) = split_key_value(trimmed)?;
            current_hook = Some(hook_name.to_string());
            definition.hooks.entry(hook_name.to_string()).or_default();
            if value.starts_with('[') {
                definition
                    .hooks
                    .insert(hook_name.to_string(), parse_inline_list(value));
            }
            continue;
        }
        if key == "hooks" && trimmed.starts_with("- ") {
            if let Some(hook_name) = current_hook.as_ref() {
                definition
                    .hooks
                    .entry(hook_name.clone())
                    .or_default()
                    .push(unquote(trimmed.trim_start_matches("- ")));
            }
            continue;
        }
        if trimmed.starts_with("- ") {
            apply_frontmatter_list_item(
                definition,
                key,
                unquote(trimmed.trim_start_matches("- ")),
            )?;
        }
    }
    Ok(())
}

fn split_key_value(line: &str) -> Result<(&str, &str)> {
    let Some((key, value)) = line.split_once(':') else {
        return Err(anyhow!("invalid agent frontmatter line: {}", line));
    };
    Ok((key.trim(), value.trim()))
}

fn apply_frontmatter_scalar(
    definition: &mut SubAgentDefinition,
    key: &str,
    value: &str,
) -> Result<()> {
    match key {
        "name" => definition.name = unquote(value),
        "description" => definition.description = unquote(value),
        "when_to_use" | "whenToUse" => definition.when_to_use = unquote(value),
        "tools" => definition.tools = parse_inline_list(value),
        "model" => definition.model = Some(unquote(value)),
        "permissionMode" | "permission_mode" => {
            definition.permission_mode = parse_permission_mode(value)?
        }
        "background" => definition.background = parse_bool(value),
        "isolation" => definition.isolation = parse_isolation(value)?,
        "maxTurns" | "max_turns" => definition.max_turns = value.parse::<usize>().ok(),
        "memory" => definition.memory = Some(unquote(value)),
        "requiredMcpServers" | "required_mcp_servers" => {
            definition.required_mcp_servers = parse_inline_list(value)
        }
        "skills" => definition.skills = parse_inline_list(value),
        "initialPrompt" | "initial_prompt" => definition.initial_prompt = Some(unquote(value)),
        _ => {}
    }
    Ok(())
}

fn clear_collection(definition: &mut SubAgentDefinition, key: &str) {
    match key {
        "tools" => definition.tools.clear(),
        "requiredMcpServers" | "required_mcp_servers" => definition.required_mcp_servers.clear(),
        "skills" => definition.skills.clear(),
        "hooks" => definition.hooks.clear(),
        _ => {}
    }
}

fn apply_frontmatter_list_item(
    definition: &mut SubAgentDefinition,
    key: &str,
    value: String,
) -> Result<()> {
    match key {
        "tools" => definition.tools.push(value),
        "requiredMcpServers" | "required_mcp_servers" => {
            definition.required_mcp_servers.push(value)
        }
        "skills" => definition.skills.push(value),
        _ => {}
    }
    Ok(())
}

fn parse_permission_mode(value: &str) -> Result<AgentPermissionMode> {
    match unquote(value).to_ascii_lowercase().as_str() {
        "default" => Ok(AgentPermissionMode::Default),
        "plan" => Ok(AgentPermissionMode::Plan),
        "auto" => Ok(AgentPermissionMode::Auto),
        "acceptedits" | "accept-edits" | "accept_edits" => Ok(AgentPermissionMode::AcceptEdits),
        "bypasspermissions" | "bypass-permissions" | "bypass_permissions" => {
            Ok(AgentPermissionMode::BypassPermissions)
        }
        other => Err(anyhow!("invalid permissionMode: {}", other)),
    }
}

fn parse_isolation(value: &str) -> Result<AgentIsolation> {
    match unquote(value).to_ascii_lowercase().as_str() {
        "none" => Ok(AgentIsolation::None),
        "worktree" => Ok(AgentIsolation::Worktree),
        "remote" => Ok(AgentIsolation::Remote),
        other => Err(anyhow!("invalid isolation: {}", other)),
    }
}

fn parse_inline_list(value: &str) -> Vec<String> {
    let value = value.trim();
    if value.starts_with('[') && value.ends_with(']') {
        return value
            .trim_start_matches('[')
            .trim_end_matches(']')
            .split(',')
            .map(unquote)
            .filter(|item| !item.is_empty())
            .collect();
    }
    let item = unquote(value);
    if item.is_empty() {
        Vec::new()
    } else {
        vec![item]
    }
}

fn parse_bool(value: &str) -> bool {
    matches!(
        unquote(value).to_ascii_lowercase().as_str(),
        "true" | "1" | "yes" | "on"
    )
}

fn unquote(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .to_string()
}

fn default_agent_tools() -> Vec<String> {
    DEFAULT_AGENT_TOOLS
        .iter()
        .map(|tool| tool.to_string())
        .collect()
}

fn validate_launch_request(request: &AgentLaunchRequest) -> Result<()> {
    if request.subagent_type.trim().is_empty() {
        return Err(anyhow!("subagent_type is required"));
    }
    if request.description.trim().is_empty() {
        return Err(anyhow!("description is required"));
    }
    if request.prompt.trim().is_empty() {
        return Err(anyhow!("prompt is required"));
    }
    Ok(())
}

fn resolve_output_file(
    config: &SubAgentManagerConfig,
    task_id: &str,
    requested: Option<&PathBuf>,
) -> PathBuf {
    match requested {
        Some(path) => resolve_workspace_path(&config.workspace_root, path),
        None => config
            .state_root
            .join("outputs")
            .join(format!("{}.md", task_id)),
    }
}

fn resolve_workspace_path(workspace_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root.join(path)
    }
}

async fn create_git_worktree(workspace_root: &Path, worktree_root: &Path) -> Result<()> {
    let output = tokio::process::Command::new("git")
        .arg("-C")
        .arg(workspace_root)
        .arg("worktree")
        .arg("add")
        .arg("--detach")
        .arg(worktree_root)
        .arg("HEAD")
        .output()
        .await?;
    if output.status.success() {
        return Ok(());
    }
    Err(anyhow!(
        "git worktree add failed: {}",
        String::from_utf8_lossy(&output.stderr)
    ))
}

fn required_string(input: &Value, key: &str) -> std::result::Result<String, ToolError> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| ToolError {
            message: format!("{} is required", key),
            code: Some(format!("missing_{}", key)),
        })
}

fn optional_path(input: &Value, key: &str) -> Option<PathBuf> {
    input
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
}

fn optional_isolation(input: &Value) -> std::result::Result<Option<AgentIsolation>, ToolError> {
    input
        .get("isolation")
        .and_then(Value::as_str)
        .map(parse_isolation)
        .transpose()
        .map_err(tool_error)
}

fn json_tool_output(content: Value) -> ToolOutput {
    ToolOutput {
        output_type: "json".to_string(),
        content: content.to_string(),
        metadata: HashMap::new(),
    }
}

fn tool_error(error: anyhow::Error) -> ToolError {
    ToolError {
        message: error.to_string(),
        code: Some("sub_agent_error".to_string()),
    }
}

fn normalize_agent_name(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}

fn is_env_truthy(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn escape_xml(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
