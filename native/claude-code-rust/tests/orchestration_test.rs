use claude_code_rs::{
    coordinator_allowed_tools, coordinator_system_prompt, is_coordinator_mode_enabled,
    is_swarm_mode_enabled, render_task_notification, worker_allowed_tools, AgentTeamStore,
    SwarmTaskPriority, SwarmTaskStatus, TaskNotification, TeammateRole,
};

#[test]
fn coordinator_policy_restricts_tools_and_renders_prompt() {
    let tools = coordinator_allowed_tools();

    assert_eq!(
        tools,
        vec![
            "agent",
            "send_message",
            "task_output",
            "task_stop",
            "subscribe_pr_activity"
        ]
    );
    assert!(!tools.contains(&"file_read"));
    assert!(!tools.contains(&"execute_command"));

    let prompt = coordinator_system_prompt("Base prompt", Some("/tmp/scratchpad"));
    assert!(prompt.contains("Base prompt"));
    assert!(prompt.contains("Coordinator Mode"));
    assert!(prompt.contains("must understand the task before delegation"));
    assert!(prompt.contains("TaskOutput"));
    assert!(prompt.contains("/tmp/scratchpad"));
}

#[test]
fn worker_policy_excludes_internal_team_tools() {
    let simple = worker_allowed_tools(true);
    assert_eq!(simple, vec!["execute_command", "file_read", "file_edit"]);

    let full = worker_allowed_tools(false);
    assert!(full.contains(&"file_write"));
    assert!(full.contains(&"search"));
    assert!(!full.contains(&"team_create"));
    assert!(!full.contains(&"send_message"));
    assert!(!full.contains(&"task_stop"));
}

#[test]
fn environment_gates_follow_documented_names() {
    std::env::remove_var("CLAUDE_CODE_COORDINATOR_MODE");
    std::env::remove_var("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS");
    assert!(!is_coordinator_mode_enabled());
    assert!(!is_swarm_mode_enabled());

    std::env::set_var("CLAUDE_CODE_COORDINATOR_MODE", "1");
    std::env::set_var("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS", "true");
    assert!(is_coordinator_mode_enabled());
    assert!(is_swarm_mode_enabled());

    std::env::remove_var("CLAUDE_CODE_COORDINATOR_MODE");
    std::env::remove_var("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS");
}

#[test]
fn task_notification_uses_documented_xml_shape() {
    let xml = render_task_notification(&TaskNotification {
        task_id: "agent-a1b".to_string(),
        status: "completed".to_string(),
        summary: "Agent completed".to_string(),
        result: "Found auth issue".to_string(),
        total_tokens: 123,
        tool_uses: 4,
        duration_ms: 987,
    });

    assert!(xml.contains("<task-notification>"));
    assert!(xml.contains("<task-id>agent-a1b</task-id>"));
    assert!(xml.contains("<status>completed</status>"));
    assert!(xml.contains("<summary>Agent completed</summary>"));
    assert!(xml.contains("<result>Found auth issue</result>"));
    assert!(xml.contains("<total_tokens>123</total_tokens>"));
    assert!(xml.contains("<tool_uses>4</tool_uses>"));
    assert!(xml.contains("<duration_ms>987</duration_ms>"));
}

#[tokio::test]
async fn swarm_claims_tasks_once_and_persists_team_state() {
    let temp = tempfile::tempdir().unwrap();
    let store = AgentTeamStore::new(temp.path().to_path_buf());

    let team = store
        .create_team("core-refactor", "lead-session")
        .await
        .unwrap();
    assert_eq!(team.task_list_id, "core-refactor");

    store
        .add_teammate("core-refactor", "alice", TeammateRole::Teammate)
        .await
        .unwrap();
    store
        .add_teammate("core-refactor", "bob", TeammateRole::Teammate)
        .await
        .unwrap();

    let task = store
        .create_task(
            "core-refactor",
            "Split auth",
            "Move auth helpers into a module",
            SwarmTaskPriority::High,
            Vec::new(),
        )
        .await
        .unwrap();

    let claimed = store
        .claim_task("core-refactor", &task.id, "alice")
        .await
        .unwrap();
    assert_eq!(claimed.status, SwarmTaskStatus::InProgress);
    assert_eq!(claimed.owner.as_deref(), Some("alice"));

    let second_claim = store.claim_task("core-refactor", &task.id, "bob").await;
    assert!(second_claim.is_err());
    assert!(second_claim
        .unwrap_err()
        .to_string()
        .contains("already_claimed"));

    store
        .complete_task("core-refactor", &task.id, "alice", "Done")
        .await
        .unwrap();

    let reloaded = AgentTeamStore::new(temp.path().to_path_buf());
    let loaded_team = reloaded.load_team("core-refactor").await.unwrap().unwrap();
    assert_eq!(loaded_team.teammates.len(), 2);

    let tasks = reloaded.list_tasks("core-refactor").await.unwrap();
    assert_eq!(tasks[0].status, SwarmTaskStatus::Completed);
    assert_eq!(tasks[0].result.as_deref(), Some("Done"));
}

#[tokio::test]
async fn swarm_mailbox_broadcasts_and_unassigns_idle_teammate_tasks() {
    let temp = tempfile::tempdir().unwrap();
    let store = AgentTeamStore::new(temp.path().to_path_buf());

    store
        .create_team("lint-fixes", "lead-session")
        .await
        .unwrap();
    store
        .add_teammate("lint-fixes", "alice", TeammateRole::Teammate)
        .await
        .unwrap();
    store
        .add_teammate("lint-fixes", "bob", TeammateRole::Teammate)
        .await
        .unwrap();

    store
        .send_message("lint-fixes", "lead", Some("alice"), "please inspect cli")
        .await
        .unwrap();
    store
        .broadcast("lint-fixes", "lead", "sync before edits")
        .await
        .unwrap();

    let alice_inbox = store.inbox("lint-fixes", "alice").await.unwrap();
    assert_eq!(alice_inbox.len(), 2);

    let bob_inbox = store.inbox("lint-fixes", "bob").await.unwrap();
    assert_eq!(bob_inbox.len(), 1);
    assert_eq!(bob_inbox[0].content, "sync before edits");

    let task = store
        .create_task(
            "lint-fixes",
            "Fix cli warning",
            "Clean one warning",
            SwarmTaskPriority::Medium,
            Vec::new(),
        )
        .await
        .unwrap();
    store
        .claim_task("lint-fixes", &task.id, "alice")
        .await
        .unwrap();

    let reset = store
        .unassign_teammate_tasks("lint-fixes", "alice")
        .await
        .unwrap();
    assert_eq!(reset, 1);

    let tasks = store.list_tasks("lint-fixes").await.unwrap();
    assert_eq!(tasks[0].status, SwarmTaskStatus::Pending);
    assert!(tasks[0].owner.is_none());
}
