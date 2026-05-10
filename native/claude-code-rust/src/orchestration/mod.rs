use std::collections::HashSet;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::fs;
use tokio::sync::RwLock;
use tokio::time::sleep;

use crate::config::Settings;
use crate::tools::{Tool, ToolError, ToolOutput};

const COORDINATOR_MODE_ENV: &str = "CLAUDE_CODE_COORDINATOR_MODE";
const SWARM_MODE_ENV: &str = "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS";
const SIMPLE_MODE_ENV: &str = "CLAUDE_CODE_SIMPLE";
const SCRATCHPAD_ENV: &str = "CLAUDE_CODE_SCRATCHPAD";
const LOCK_RETRY_COUNT: usize = 50;
const LOCK_RETRY_DELAY: Duration = Duration::from_millis(20);

const COORDINATOR_TOOLS: &[&str] = &[
    "agent",
    "send_message",
    "task_output",
    "task_stop",
    "subscribe_pr_activity",
];
const INTERNAL_WORKER_TOOLS: &[&str] = &[
    "agent",
    "team_create",
    "team_delete",
    "send_message",
    "task_output",
    "task_stop",
    "synthetic_output",
];
const SIMPLE_WORKER_TOOLS: &[&str] = &["execute_command", "file_read", "file_edit"];
const FULL_WORKER_TOOLS: &[&str] = &[
    "execute_command",
    "file_read",
    "file_edit",
    "file_write",
    "search",
    "list_files",
    "git_operations",
    "task_management",
    "note_edit",
];

pub fn is_coordinator_mode_enabled() -> bool {
    is_env_truthy(COORDINATOR_MODE_ENV)
}

pub fn is_swarm_mode_enabled() -> bool {
    is_env_truthy(SWARM_MODE_ENV)
}

pub fn is_simple_worker_mode_enabled() -> bool {
    is_env_truthy(SIMPLE_MODE_ENV)
}

pub fn is_scratchpad_enabled() -> bool {
    is_env_truthy(SCRATCHPAD_ENV)
}

pub fn coordinator_allowed_tools() -> Vec<&'static str> {
    COORDINATOR_TOOLS.to_vec()
}

pub fn worker_allowed_tools(simple_mode: bool) -> Vec<&'static str> {
    if simple_mode {
        return SIMPLE_WORKER_TOOLS.to_vec();
    }

    let internal: HashSet<&str> = INTERNAL_WORKER_TOOLS.iter().copied().collect();
    FULL_WORKER_TOOLS
        .iter()
        .copied()
        .filter(|tool| !internal.contains(tool))
        .collect()
}

pub fn coordinator_allowed_tool_names() -> Vec<String> {
    coordinator_allowed_tools()
        .into_iter()
        .map(str::to_string)
        .collect()
}

pub fn worker_allowed_tool_names(simple_mode: bool) -> Vec<String> {
    worker_allowed_tools(simple_mode)
        .into_iter()
        .map(str::to_string)
        .collect()
}

pub fn coordinator_system_prompt(base_prompt: &str, scratchpad_path: Option<&str>) -> String {
    let scratchpad = scratchpad_path
        .map(|path| {
            format!(
                "\nScratchpad: Workers may read and write shared notes under `{}`. Use it to pass durable findings between workers without making the Coordinator relay every detail.",
                path
            )
        })
        .unwrap_or_default();

    format!(
        "{base_prompt}\n\n\
Coordinator Mode\n\
- You are the Coordinator. You plan, delegate, monitor, and synthesize.\n\
- You do not edit files, read files, run commands, or do implementation work yourself.\n\
- Available tools are limited to Agent, SendMessage, TaskOutput, and TaskStop.\n\
- You must understand the task before delegation. Give workers precise instructions with concrete files, constraints, and success criteria.\n\
- Do not delegate vague understanding. Synthesize worker results into one coherent answer for the user.\n\
- Workers report completion through <task-notification> messages; use <task-id> when sending follow-up instructions.{scratchpad}"
    )
}

pub fn worker_system_prompt(
    base_prompt: &str,
    tool_names: &[String],
    scratchpad_path: Option<&str>,
) -> String {
    let scratchpad = scratchpad_path
        .map(|path| {
            format!(
                "\nShared scratchpad path: `{}`. Use it for cross-worker findings when useful.",
                path
            )
        })
        .unwrap_or_default();

    format!(
        "{base_prompt}\n\n\
Worker Mode\n\
- You are a worker agent. Execute the Coordinator's concrete assignment.\n\
- Do not create teams, spawn nested workers, send inter-agent messages, or stop other agents.\n\
- Available tools: {}.\n\
- Report concise findings and implementation results back to the Coordinator.{scratchpad}",
        tool_names.join(", ")
    )
}

pub fn coordinator_tools(settings: Settings) -> Vec<Box<dyn Tool>> {
    let state = std::sync::Arc::new(CoordinatorToolState::new());
    let mut tools = crate::sub_agents::sub_agent_tools(settings)
        .map(|(_, tools)| tools)
        .unwrap_or_default();
    tools.push(Box::new(SubscribePrActivityTool::new(state)));
    tools
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskNotification {
    pub task_id: String,
    pub status: String,
    pub summary: String,
    pub result: String,
    pub total_tokens: usize,
    pub tool_uses: usize,
    pub duration_ms: u64,
}

pub fn render_task_notification(notification: &TaskNotification) -> String {
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

struct CoordinatorToolState {
    pr_subscriptions: RwLock<Vec<String>>,
}

impl CoordinatorToolState {
    fn new() -> Self {
        Self {
            pr_subscriptions: RwLock::new(Vec::new()),
        }
    }
}

struct SubscribePrActivityTool {
    state: std::sync::Arc<CoordinatorToolState>,
}

impl SubscribePrActivityTool {
    fn new(state: std::sync::Arc<CoordinatorToolState>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl Tool for SubscribePrActivityTool {
    fn name(&self) -> &str {
        "subscribe_pr_activity"
    }

    fn description(&self) -> &str {
        "Record a pull request activity subscription for coordinator awareness"
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "repository": {
                    "type": "string",
                    "description": "Repository name, for example owner/repo"
                },
                "pr_number": {
                    "type": "integer",
                    "description": "Pull request number"
                }
            },
            "required": ["repository", "pr_number"]
        })
    }

    async fn execute(&self, input: Value) -> Result<ToolOutput, ToolError> {
        let repository = required_str(&input, "repository")?;
        let pr_number = input
            .get("pr_number")
            .and_then(Value::as_i64)
            .ok_or_else(|| tool_error("missing_pr_number", "pr_number is required"))?;
        let subscription = format!("{}#{}", repository, pr_number);
        self.state
            .pr_subscriptions
            .write()
            .await
            .push(subscription.clone());
        Ok(json_tool_output(json!({
            "success": true,
            "subscription": subscription,
            "message": "PR activity subscription recorded"
        })))
    }
}

fn required_str<'a>(input: &'a Value, name: &str) -> Result<&'a str, ToolError> {
    input
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| tool_error(format!("missing_{}", name), format!("{} is required", name)))
}

fn json_tool_output(content: Value) -> ToolOutput {
    ToolOutput {
        output_type: "json".to_string(),
        content: content.to_string(),
        metadata: std::collections::HashMap::new(),
    }
}

fn tool_error(code: impl Into<String>, message: impl Into<String>) -> ToolError {
    ToolError {
        message: message.into(),
        code: Some(code.into()),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TeammateRole {
    Lead,
    Teammate,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TeammateStatus {
    Idle,
    Running,
    Stopped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SwarmTaskStatus {
    Pending,
    InProgress,
    Completed,
    Blocked,
    Deleted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SwarmTaskPriority {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MailboxMessageMode {
    Message,
    Broadcast,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Teammate {
    pub name: String,
    pub role: TeammateRole,
    pub status: TeammateStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTeam {
    pub team_name: String,
    pub lead_session_id: String,
    pub task_list_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub teammates: Vec<Teammate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmTask {
    pub id: String,
    pub subject: String,
    pub description: String,
    pub status: SwarmTaskStatus,
    pub priority: SwarmTaskPriority,
    pub owner: Option<String>,
    pub dependencies: Vec<String>,
    pub result: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailboxMessage {
    pub id: String,
    pub team_name: String,
    pub from: String,
    pub to: Option<String>,
    pub mode: MailboxMessageMode,
    pub content: String,
    pub created_at: DateTime<Utc>,
    pub read_by: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SwarmHookKind {
    TaskCreated,
    TaskCompleted,
    TeammateIdle,
    MessageSent,
    TaskClaimed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmHookEvent {
    pub id: String,
    pub team_name: String,
    pub kind: SwarmHookKind,
    pub actor: String,
    pub task_id: Option<String>,
    pub message_id: Option<String>,
    pub summary: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct AgentTeamStore {
    root: PathBuf,
}

impl AgentTeamStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn default_root() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        Self::new(home.join(".claude"))
    }

    pub async fn create_team(&self, team_name: &str, lead_session_id: &str) -> Result<AgentTeam> {
        validate_team_name(team_name)?;
        let _lock = self.acquire_lock(team_name).await?;
        if self.config_path(team_name).exists() {
            return Err(anyhow!("team_exists: {}", team_name));
        }

        let now = Utc::now();
        let team = AgentTeam {
            team_name: team_name.to_string(),
            lead_session_id: lead_session_id.to_string(),
            task_list_id: team_name.to_string(),
            created_at: now,
            updated_at: now,
            teammates: Vec::new(),
        };
        self.write_team(&team).await?;
        self.write_tasks(team_name, &[]).await?;
        self.write_mailbox(team_name, &[]).await?;
        self.write_hooks(team_name, &[]).await?;
        Ok(team)
    }

    pub async fn load_team(&self, team_name: &str) -> Result<Option<AgentTeam>> {
        validate_team_name(team_name)?;
        let path = self.config_path(team_name);
        if !path.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(path).await?;
        Ok(Some(serde_json::from_str(&content)?))
    }

    pub async fn delete_team(&self, team_name: &str) -> Result<()> {
        validate_team_name(team_name)?;
        let _lock = self.acquire_lock(team_name).await?;
        let team_dir = self.team_dir(team_name);
        let task_dir = self.task_dir(team_name);
        if team_dir.exists() {
            fs::remove_dir_all(team_dir).await?;
        }
        if task_dir.exists() {
            fs::remove_dir_all(task_dir).await?;
        }
        Ok(())
    }

    pub async fn add_teammate(
        &self,
        team_name: &str,
        teammate_name: &str,
        role: TeammateRole,
    ) -> Result<AgentTeam> {
        validate_teammate_name(teammate_name)?;
        let _lock = self.acquire_lock(team_name).await?;
        let mut team = self.require_team(team_name).await?;
        if team
            .teammates
            .iter()
            .any(|member| member.name == teammate_name)
        {
            return Err(anyhow!("teammate_exists: {}", teammate_name));
        }
        if role == TeammateRole::Lead
            && team
                .teammates
                .iter()
                .any(|member| member.role == TeammateRole::Lead)
        {
            return Err(anyhow!("lead_already_exists: {}", team_name));
        }

        let now = Utc::now();
        team.teammates.push(Teammate {
            name: teammate_name.to_string(),
            role,
            status: TeammateStatus::Idle,
            created_at: now,
            updated_at: now,
        });
        team.updated_at = now;
        self.write_team(&team).await?;
        Ok(team)
    }

    pub async fn create_task(
        &self,
        team_name: &str,
        subject: &str,
        description: &str,
        priority: SwarmTaskPriority,
        dependencies: Vec<String>,
    ) -> Result<SwarmTask> {
        let _lock = self.acquire_lock(team_name).await?;
        self.require_team(team_name).await?;
        let mut tasks = self.read_tasks(team_name).await?;
        let status = if dependencies.is_empty() {
            SwarmTaskStatus::Pending
        } else {
            SwarmTaskStatus::Blocked
        };
        let now = Utc::now();
        let task = SwarmTask {
            id: uuid::Uuid::new_v4().to_string(),
            subject: subject.to_string(),
            description: description.to_string(),
            status,
            priority,
            owner: None,
            dependencies,
            result: None,
            created_at: now,
            updated_at: now,
            completed_at: None,
        };
        tasks.push(task.clone());
        self.unlock_ready_tasks(&mut tasks);
        self.write_tasks(team_name, &tasks).await?;
        self.append_hook(
            team_name,
            SwarmHookKind::TaskCreated,
            "lead",
            Some(task.id.clone()),
            None,
            format!("Task created: {}", task.subject),
        )
        .await?;
        Ok(task)
    }

    pub async fn list_tasks(&self, team_name: &str) -> Result<Vec<SwarmTask>> {
        validate_team_name(team_name)?;
        self.read_tasks(team_name).await
    }

    pub async fn claim_task(
        &self,
        team_name: &str,
        task_id: &str,
        teammate_name: &str,
    ) -> Result<SwarmTask> {
        let _lock = self.acquire_lock(team_name).await?;
        let team = self.require_team(team_name).await?;
        ensure_teammate(&team, teammate_name)?;
        let mut tasks = self.read_tasks(team_name).await?;
        self.unlock_ready_tasks(&mut tasks);
        let task = tasks
            .iter_mut()
            .find(|task| task.id == task_id)
            .ok_or_else(|| anyhow!("task_not_found: {}", task_id))?;

        if task.status != SwarmTaskStatus::Pending || task.owner.is_some() {
            return Err(anyhow!("already_claimed: {}", task_id));
        }

        let now = Utc::now();
        task.status = SwarmTaskStatus::InProgress;
        task.owner = Some(teammate_name.to_string());
        task.updated_at = now;
        let claimed = task.clone();
        self.write_tasks(team_name, &tasks).await?;
        self.append_hook(
            team_name,
            SwarmHookKind::TaskClaimed,
            teammate_name,
            Some(task_id.to_string()),
            None,
            format!("Task claimed by {}", teammate_name),
        )
        .await?;
        Ok(claimed)
    }

    pub async fn complete_task(
        &self,
        team_name: &str,
        task_id: &str,
        teammate_name: &str,
        result: &str,
    ) -> Result<SwarmTask> {
        let _lock = self.acquire_lock(team_name).await?;
        let mut tasks = self.read_tasks(team_name).await?;
        let task = tasks
            .iter_mut()
            .find(|task| task.id == task_id)
            .ok_or_else(|| anyhow!("task_not_found: {}", task_id))?;
        if task.owner.as_deref() != Some(teammate_name) {
            return Err(anyhow!("not_task_owner: {}", task_id));
        }

        let now = Utc::now();
        task.status = SwarmTaskStatus::Completed;
        task.result = Some(result.to_string());
        task.updated_at = now;
        task.completed_at = Some(now);
        let completed = task.clone();
        self.unlock_ready_tasks(&mut tasks);
        self.write_tasks(team_name, &tasks).await?;
        self.append_hook(
            team_name,
            SwarmHookKind::TaskCompleted,
            teammate_name,
            Some(task_id.to_string()),
            None,
            format!("Task completed by {}", teammate_name),
        )
        .await?;
        Ok(completed)
    }

    pub async fn unassign_teammate_tasks(
        &self,
        team_name: &str,
        teammate_name: &str,
    ) -> Result<usize> {
        let _lock = self.acquire_lock(team_name).await?;
        let mut tasks = self.read_tasks(team_name).await?;
        let mut reset = 0;
        for task in &mut tasks {
            if task.owner.as_deref() == Some(teammate_name)
                && task.status == SwarmTaskStatus::InProgress
            {
                task.status = SwarmTaskStatus::Pending;
                task.owner = None;
                task.updated_at = Utc::now();
                reset += 1;
            }
        }
        self.write_tasks(team_name, &tasks).await?;
        if reset > 0 {
            self.append_hook(
                team_name,
                SwarmHookKind::TeammateIdle,
                teammate_name,
                None,
                None,
                format!("Unassigned {} task(s) from {}", reset, teammate_name),
            )
            .await?;
        }
        Ok(reset)
    }

    pub async fn send_message(
        &self,
        team_name: &str,
        from: &str,
        to: Option<&str>,
        content: &str,
    ) -> Result<MailboxMessage> {
        let _lock = self.acquire_lock(team_name).await?;
        self.require_team(team_name).await?;
        let mut mailbox = self.read_mailbox(team_name).await?;
        let message = MailboxMessage {
            id: uuid::Uuid::new_v4().to_string(),
            team_name: team_name.to_string(),
            from: from.to_string(),
            to: to.map(str::to_string),
            mode: MailboxMessageMode::Message,
            content: content.to_string(),
            created_at: Utc::now(),
            read_by: Vec::new(),
        };
        mailbox.push(message.clone());
        self.write_mailbox(team_name, &mailbox).await?;
        self.append_hook(
            team_name,
            SwarmHookKind::MessageSent,
            from,
            None,
            Some(message.id.clone()),
            "Mailbox message sent".to_string(),
        )
        .await?;
        Ok(message)
    }

    pub async fn broadcast(
        &self,
        team_name: &str,
        from: &str,
        content: &str,
    ) -> Result<MailboxMessage> {
        let _lock = self.acquire_lock(team_name).await?;
        self.require_team(team_name).await?;
        let mut mailbox = self.read_mailbox(team_name).await?;
        let message = MailboxMessage {
            id: uuid::Uuid::new_v4().to_string(),
            team_name: team_name.to_string(),
            from: from.to_string(),
            to: None,
            mode: MailboxMessageMode::Broadcast,
            content: content.to_string(),
            created_at: Utc::now(),
            read_by: Vec::new(),
        };
        mailbox.push(message.clone());
        self.write_mailbox(team_name, &mailbox).await?;
        self.append_hook(
            team_name,
            SwarmHookKind::MessageSent,
            from,
            None,
            Some(message.id.clone()),
            "Mailbox broadcast sent".to_string(),
        )
        .await?;
        Ok(message)
    }

    pub async fn inbox(&self, team_name: &str, teammate_name: &str) -> Result<Vec<MailboxMessage>> {
        validate_team_name(team_name)?;
        let mailbox = self.read_mailbox(team_name).await?;
        Ok(mailbox
            .into_iter()
            .filter(|message| match message.mode {
                MailboxMessageMode::Message => message.to.as_deref() == Some(teammate_name),
                MailboxMessageMode::Broadcast => message.from != teammate_name,
            })
            .collect())
    }

    pub async fn hooks(&self, team_name: &str) -> Result<Vec<SwarmHookEvent>> {
        self.read_hooks(team_name).await
    }

    fn unlock_ready_tasks(&self, tasks: &mut [SwarmTask]) {
        let completed: HashSet<String> = tasks
            .iter()
            .filter(|task| task.status == SwarmTaskStatus::Completed)
            .map(|task| task.id.clone())
            .collect();

        for task in tasks.iter_mut() {
            if task.status == SwarmTaskStatus::Blocked
                && task
                    .dependencies
                    .iter()
                    .all(|dependency| completed.contains(dependency))
            {
                task.status = SwarmTaskStatus::Pending;
                task.updated_at = Utc::now();
            }
        }
    }

    async fn require_team(&self, team_name: &str) -> Result<AgentTeam> {
        self.load_team(team_name)
            .await?
            .ok_or_else(|| anyhow!("team_not_found: {}", team_name))
    }

    async fn write_team(&self, team: &AgentTeam) -> Result<()> {
        fs::create_dir_all(self.team_dir(&team.team_name)).await?;
        let serialized = serde_json::to_string_pretty(team)?;
        fs::write(self.config_path(&team.team_name), serialized).await?;
        Ok(())
    }

    async fn read_tasks(&self, team_name: &str) -> Result<Vec<SwarmTask>> {
        validate_team_name(team_name)?;
        read_json_array(self.tasks_path(team_name)).await
    }

    async fn write_tasks(&self, team_name: &str, tasks: &[SwarmTask]) -> Result<()> {
        fs::create_dir_all(self.task_dir(team_name)).await?;
        let serialized = serde_json::to_string_pretty(tasks)?;
        fs::write(self.tasks_path(team_name), serialized).await?;
        Ok(())
    }

    async fn read_mailbox(&self, team_name: &str) -> Result<Vec<MailboxMessage>> {
        validate_team_name(team_name)?;
        read_json_array(self.mailbox_path(team_name)).await
    }

    async fn write_mailbox(&self, team_name: &str, messages: &[MailboxMessage]) -> Result<()> {
        fs::create_dir_all(self.task_dir(team_name)).await?;
        let serialized = serde_json::to_string_pretty(messages)?;
        fs::write(self.mailbox_path(team_name), serialized).await?;
        Ok(())
    }

    async fn read_hooks(&self, team_name: &str) -> Result<Vec<SwarmHookEvent>> {
        validate_team_name(team_name)?;
        read_json_array(self.hooks_path(team_name)).await
    }

    async fn write_hooks(&self, team_name: &str, hooks: &[SwarmHookEvent]) -> Result<()> {
        fs::create_dir_all(self.task_dir(team_name)).await?;
        let serialized = serde_json::to_string_pretty(hooks)?;
        fs::write(self.hooks_path(team_name), serialized).await?;
        Ok(())
    }

    async fn append_hook(
        &self,
        team_name: &str,
        kind: SwarmHookKind,
        actor: &str,
        task_id: Option<String>,
        message_id: Option<String>,
        summary: String,
    ) -> Result<()> {
        let mut hooks = self.read_hooks(team_name).await?;
        hooks.push(SwarmHookEvent {
            id: uuid::Uuid::new_v4().to_string(),
            team_name: team_name.to_string(),
            kind,
            actor: actor.to_string(),
            task_id,
            message_id,
            summary,
            created_at: Utc::now(),
        });
        self.write_hooks(team_name, &hooks).await
    }

    async fn acquire_lock(&self, team_name: &str) -> Result<TeamLock> {
        validate_team_name(team_name)?;
        fs::create_dir_all(self.task_dir(team_name)).await?;
        let lock_path = self.lock_path(team_name);
        for _ in 0..LOCK_RETRY_COUNT {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
                .await
            {
                Ok(_) => return Ok(TeamLock { path: lock_path }),
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                    sleep(LOCK_RETRY_DELAY).await;
                }
                Err(error) => return Err(error.into()),
            }
        }
        Err(anyhow!("team_lock_timeout: {}", team_name))
    }

    fn team_dir(&self, team_name: &str) -> PathBuf {
        self.root.join("teams").join(team_name)
    }

    fn task_dir(&self, team_name: &str) -> PathBuf {
        self.root.join("tasks").join(team_name)
    }

    fn config_path(&self, team_name: &str) -> PathBuf {
        self.team_dir(team_name).join("config.json")
    }

    fn tasks_path(&self, team_name: &str) -> PathBuf {
        self.task_dir(team_name).join("tasks.json")
    }

    fn mailbox_path(&self, team_name: &str) -> PathBuf {
        self.task_dir(team_name).join("mailbox.json")
    }

    fn hooks_path(&self, team_name: &str) -> PathBuf {
        self.task_dir(team_name).join("hooks.json")
    }

    fn lock_path(&self, team_name: &str) -> PathBuf {
        self.task_dir(team_name).join(".lock")
    }
}

struct TeamLock {
    path: PathBuf,
}

impl Drop for TeamLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

async fn read_json_array<T>(path: PathBuf) -> Result<Vec<T>>
where
    T: for<'de> Deserialize<'de>,
{
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(path).await?;
    if content.trim().is_empty() {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_str(&content)?)
}

fn ensure_teammate(team: &AgentTeam, teammate_name: &str) -> Result<()> {
    if team
        .teammates
        .iter()
        .any(|member| member.name == teammate_name)
    {
        Ok(())
    } else {
        Err(anyhow!("teammate_not_found: {}", teammate_name))
    }
}

fn validate_team_name(team_name: &str) -> Result<()> {
    validate_path_segment(team_name, "team_name")
}

fn validate_teammate_name(teammate_name: &str) -> Result<()> {
    validate_path_segment(teammate_name, "teammate_name")
}

fn validate_path_segment(value: &str, label: &str) -> Result<()> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed == "."
        || trimmed == ".."
    {
        return Err(anyhow!("invalid_{}: {}", label, value));
    }
    Ok(())
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
