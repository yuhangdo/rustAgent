use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;

use async_trait::async_trait;
use claude_code_rs::api::ChatMessage;
use claude_code_rs::sub_agents::{
    AgentIsolation, AgentLaunchRequest, AgentPermissionMode, SendMessageRequest,
    SubAgentDefinition, SubAgentManager, SubAgentManagerConfig, SubAgentRegistry,
    SubAgentRunRequest, SubAgentRunResult, SubAgentRunner, SubAgentStatus,
};

#[derive(Debug)]
struct ScriptedRunner {
    delay: Duration,
    calls: Arc<AtomicUsize>,
}

impl ScriptedRunner {
    fn new(delay: Duration) -> Self {
        Self {
            delay,
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn calls(&self) -> Arc<AtomicUsize> {
        self.calls.clone()
    }
}

#[async_trait]
impl SubAgentRunner for ScriptedRunner {
    async fn run(&self, request: SubAgentRunRequest) -> anyhow::Result<SubAgentRunResult> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(self.delay).await;
        let last_user = request
            .history
            .iter()
            .rev()
            .find(|message| message.role == "user")
            .and_then(|message| message.content.clone())
            .unwrap_or_default();

        Ok(SubAgentRunResult {
            answer: format!(
                "{}:{}:{}",
                request.definition.name,
                request.workspace_root.display(),
                last_user
            ),
            total_tokens: 42,
            tool_uses: request.definition.tools.len(),
            history: {
                let mut history = request.history;
                history.push(ChatMessage::assistant(format!("answered {}", last_user)));
                history
            },
        })
    }
}

#[tokio::test]
async fn markdown_frontmatter_defines_named_sub_agent_policy() {
    let temp = tempfile::tempdir().unwrap();
    let agents_dir = temp.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("reviewer.md"),
        r#"---
name: reviewer
description: Reviews risky code paths
when_to_use: Use before touching auth or payment code
tools:
  - file_read
  - search
model: opus
permissionMode: plan
background: true
isolation: worktree
maxTurns: 5
memory: project
requiredMcpServers:
  - github
hooks:
  PreToolUse:
    - echo pre
skills:
  - code-review
initialPrompt: Start by mapping risks.
---
You are a careful reviewer.
"#,
    )
    .unwrap();

    let registry = SubAgentRegistry::load_from_workspace(temp.path())
        .await
        .unwrap();
    let definition = registry.get("reviewer").unwrap();

    assert_eq!(definition.name, "reviewer");
    assert_eq!(definition.description, "Reviews risky code paths");
    assert_eq!(
        definition.when_to_use,
        "Use before touching auth or payment code"
    );
    assert_eq!(definition.tools, vec!["file_read", "search"]);
    assert_eq!(definition.model.as_deref(), Some("opus"));
    assert_eq!(definition.permission_mode, AgentPermissionMode::Plan);
    assert!(definition.background);
    assert_eq!(definition.isolation, AgentIsolation::Worktree);
    assert_eq!(definition.max_turns, Some(5));
    assert_eq!(definition.memory.as_deref(), Some("project"));
    assert_eq!(definition.required_mcp_servers, vec!["github"]);
    assert_eq!(
        definition
            .hooks
            .get("PreToolUse")
            .cloned()
            .unwrap_or_default(),
        vec!["echo pre"]
    );
    assert_eq!(definition.skills, vec!["code-review"]);
    assert_eq!(
        definition.initial_prompt.as_deref(),
        Some("Start by mapping risks.")
    );
    assert!(definition.system_prompt.contains("careful reviewer"));
}

#[tokio::test]
async fn invalid_custom_definition_does_not_disable_builtin_agents() {
    let temp = tempfile::tempdir().unwrap();
    let agents_dir = temp.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("broken.md"),
        "---\nname: broken\npermissionMode: definitely-not-valid\n---\nBody",
    )
    .unwrap();

    let registry = SubAgentRegistry::load_from_workspace(temp.path())
        .await
        .unwrap();

    assert!(registry.get("general-purpose").is_some());
    assert!(registry.get("broken").is_none());
}

#[tokio::test]
async fn background_agent_writes_sidechain_output_and_notification() {
    let temp = tempfile::tempdir().unwrap();
    let mut registry = SubAgentRegistry::with_builtin_agents();
    registry.register(SubAgentDefinition {
        name: "reviewer".to_string(),
        description: "Review things".to_string(),
        when_to_use: "When review is needed".to_string(),
        tools: vec!["file_read".to_string(), "search".to_string()],
        ..SubAgentDefinition::default()
    });
    let runner = Arc::new(ScriptedRunner::new(Duration::from_millis(20)));
    let manager = SubAgentManager::new(
        SubAgentManagerConfig::for_workspace(temp.path().to_path_buf()),
        registry,
        runner,
    );

    let launched = manager
        .launch(AgentLaunchRequest {
            subagent_type: "reviewer".to_string(),
            description: "review auth".to_string(),
            prompt: "inspect auth flow".to_string(),
            run_in_background: Some(true),
            ..AgentLaunchRequest::default()
        })
        .await
        .unwrap();

    assert_eq!(launched.status, SubAgentStatus::Running);
    assert!(launched.output_file.is_some());

    let running = manager
        .task_output(&launched.task_id, false, Duration::ZERO)
        .await
        .unwrap();
    assert_eq!(running.status, SubAgentStatus::Running);
    assert!(running.result.is_none());

    let completed = manager
        .task_output(&launched.task_id, true, Duration::from_secs(2))
        .await
        .unwrap();

    assert_eq!(completed.status, SubAgentStatus::Completed);
    assert!(completed
        .result
        .as_deref()
        .unwrap()
        .contains("inspect auth flow"));
    assert!(completed.sidechain_transcript.as_ref().unwrap().exists());
    assert!(completed.output_file.as_ref().unwrap().exists());

    let notifications = manager.drain_notifications().await;
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].task_id, launched.task_id);
    assert_eq!(notifications[0].status, "completed");
}

#[tokio::test]
async fn send_message_queues_running_agent_and_resumes_terminal_agent() {
    let temp = tempfile::tempdir().unwrap();
    let runner = ScriptedRunner::new(Duration::from_millis(20));
    let calls = runner.calls();
    let manager = SubAgentManager::new(
        SubAgentManagerConfig::for_workspace(temp.path().to_path_buf()),
        SubAgentRegistry::with_builtin_agents(),
        Arc::new(runner),
    );

    let launched = manager
        .launch(AgentLaunchRequest {
            subagent_type: "general-purpose".to_string(),
            description: "long task".to_string(),
            prompt: "first request".to_string(),
            run_in_background: Some(true),
            ..AgentLaunchRequest::default()
        })
        .await
        .unwrap();

    let queued = manager
        .send_message(SendMessageRequest {
            to: launched.task_id.clone(),
            message: "follow-up while running".to_string(),
            block: false,
            timeout: Duration::from_secs(1),
        })
        .await
        .unwrap();
    assert_eq!(queued.status, SubAgentStatus::Running);
    assert!(queued.queued);

    let completed = manager
        .task_output(&launched.task_id, true, Duration::from_secs(2))
        .await
        .unwrap();
    assert!(completed
        .result
        .as_deref()
        .unwrap()
        .contains("follow-up while running"));
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let resumed = manager
        .send_message(SendMessageRequest {
            to: launched.task_id.clone(),
            message: "follow-up after completion".to_string(),
            block: true,
            timeout: Duration::from_secs(2),
        })
        .await
        .unwrap();
    assert!(!resumed.queued);
    assert_eq!(resumed.status, SubAgentStatus::Completed);
    assert!(resumed
        .result
        .as_deref()
        .unwrap()
        .contains("follow-up after completion"));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn task_stop_kills_background_agent_and_prevents_more_messages() {
    let temp = tempfile::tempdir().unwrap();
    let manager = SubAgentManager::new(
        SubAgentManagerConfig::for_workspace(temp.path().to_path_buf()),
        SubAgentRegistry::with_builtin_agents(),
        Arc::new(ScriptedRunner::new(Duration::from_secs(5))),
    );

    let launched = manager
        .launch(AgentLaunchRequest {
            subagent_type: "general-purpose".to_string(),
            description: "slow task".to_string(),
            prompt: "keep working".to_string(),
            run_in_background: Some(true),
            ..AgentLaunchRequest::default()
        })
        .await
        .unwrap();

    let stopped = manager
        .task_stop(&launched.task_id, Some("user redirected work".to_string()))
        .await
        .unwrap();
    assert_eq!(stopped.status, SubAgentStatus::Killed);

    let output = manager
        .task_output(&launched.task_id, false, Duration::ZERO)
        .await
        .unwrap();
    assert_eq!(output.status, SubAgentStatus::Killed);
    assert!(output.result.as_deref().unwrap().contains("redirected"));

    let message = manager
        .send_message(SendMessageRequest {
            to: launched.task_id,
            message: "do more".to_string(),
            block: false,
            timeout: Duration::from_secs(1),
        })
        .await;
    assert!(message.unwrap_err().to_string().contains("killed"));
}

#[tokio::test]
async fn worktree_isolation_rejects_explicit_cwd_to_avoid_ambiguous_writes() {
    let temp = tempfile::tempdir().unwrap();
    let manager = SubAgentManager::new(
        SubAgentManagerConfig::for_workspace(temp.path().to_path_buf()),
        SubAgentRegistry::with_builtin_agents(),
        Arc::new(ScriptedRunner::new(Duration::from_millis(1))),
    );

    let result = manager
        .launch(AgentLaunchRequest {
            subagent_type: "general-purpose".to_string(),
            description: "ambiguous".to_string(),
            prompt: "run".to_string(),
            cwd: Some(temp.path().join("other")),
            isolation: Some(AgentIsolation::Worktree),
            ..AgentLaunchRequest::default()
        })
        .await;

    assert!(result
        .unwrap_err()
        .to_string()
        .contains("cwd cannot be combined with worktree isolation"));
}
