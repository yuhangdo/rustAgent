use claude_code_rs::plan_mode::{
    apply_plan_mode_tool_filter, is_tool_allowed_in_plan_mode, AllowedPrompt, PlanMode,
    PlanModeSession, PlanModeStatus,
};
use claude_code_rs::query_engine::{TranscriptEvent, TranscriptStore};
use claude_code_rs::tools::{ToolAccess, ToolRegistry};
use serde_json::json;

#[test]
fn plan_mode_filter_exposes_read_only_tools_and_exit_only() {
    let registry = ToolRegistry::new();

    assert_eq!(
        registry.get("file_read").unwrap().access(),
        ToolAccess::ReadOnly
    );
    assert_eq!(
        registry.get("search").unwrap().access(),
        ToolAccess::ReadOnly
    );
    assert_eq!(
        registry.get("list_files").unwrap().access(),
        ToolAccess::ReadOnly
    );
    assert_eq!(
        registry.get("file_edit").unwrap().access(),
        ToolAccess::Write
    );
    assert_eq!(
        registry.get("file_write").unwrap().access(),
        ToolAccess::Write
    );

    let visible_tools = apply_plan_mode_tool_filter(
        registry
            .list()
            .into_iter()
            .map(|tool| (tool.name().to_string(), tool.access()))
            .collect(),
    );

    assert!(visible_tools.contains(&"file_read".to_string()));
    assert!(visible_tools.contains(&"search".to_string()));
    assert!(visible_tools.contains(&"list_files".to_string()));
    assert!(visible_tools.contains(&"exit_plan_mode".to_string()));
    assert!(!visible_tools.contains(&"file_edit".to_string()));
    assert!(!visible_tools.contains(&"file_write".to_string()));
    assert!(!visible_tools.contains(&"execute_command".to_string()));
}

#[test]
fn plan_mode_policy_rejects_write_tools_even_if_model_requests_them() {
    assert!(is_tool_allowed_in_plan_mode(
        "file_read",
        ToolAccess::ReadOnly
    ));
    assert!(is_tool_allowed_in_plan_mode(
        "exit_plan_mode",
        ToolAccess::Internal
    ));
    assert!(!is_tool_allowed_in_plan_mode(
        "file_edit",
        ToolAccess::Write
    ));
    assert!(!is_tool_allowed_in_plan_mode(
        "execute_command",
        ToolAccess::Write
    ));
}

#[tokio::test]
async fn exit_plan_mode_persists_editable_plan_and_allowed_prompts() {
    let temp = tempfile::tempdir().unwrap();
    let session = PlanModeSession::new(temp.path().to_path_buf());

    let entered = session.enter("default").await.unwrap();
    assert_eq!(entered.mode, PlanMode::Plan);
    assert_eq!(entered.previous_mode.as_deref(), Some("default"));

    let exited = session
        .exit_with_plan(
            "# Refactor plan\n\n1. Inspect the auth module.\n2. Move helpers safely.",
            vec![AllowedPrompt {
                tool: "Bash".to_string(),
                prompt: "run tests".to_string(),
            }],
        )
        .await
        .unwrap();

    assert_eq!(exited.mode, PlanMode::AwaitingApproval);
    assert_eq!(exited.allowed_prompts.len(), 1);
    assert_eq!(exited.allowed_prompts[0].prompt, "run tests");
    assert!(exited.plan_file_path.exists());

    let persisted_plan = tokio::fs::read_to_string(&exited.plan_file_path)
        .await
        .unwrap();
    assert!(persisted_plan.contains("Inspect the auth module"));
    assert!(persisted_plan.contains("Allowed prompts"));
    assert!(persisted_plan.contains("run tests"));
}

#[tokio::test]
async fn plan_mode_tools_drive_session_state() {
    let temp = tempfile::tempdir().unwrap();
    let session = PlanModeSession::new(temp.path().to_path_buf());
    let mut registry = ToolRegistry::new();
    registry.register_plan_mode_tools(session.clone());

    let enter = registry
        .execute("enter_plan_mode", json!({"previous_mode":"default"}))
        .await
        .unwrap();
    assert_eq!(enter.output_type, "json");
    assert_eq!(session.status().await.mode, PlanMode::Plan);

    let exit = registry
        .execute(
            "exit_plan_mode",
            json!({
                "plan": "Implement in small steps, then test.",
                "allowed_prompts": [{"tool":"Bash","prompt":"run tests"}]
            }),
        )
        .await
        .unwrap();

    assert!(exit.content.contains("awaiting_approval"));
    assert_eq!(session.status().await.mode, PlanMode::AwaitingApproval);
}

#[tokio::test]
async fn transcript_replay_restores_plan_mode_status() {
    let temp = tempfile::tempdir().unwrap();
    let store = TranscriptStore::new(temp.path().to_path_buf());

    store
        .append(&TranscriptEvent::PlanModeEntered {
            previous_mode: "default".to_string(),
        })
        .await
        .unwrap();
    store
        .append(&TranscriptEvent::PlanModeExited {
            plan_file_path: temp
                .path()
                .join("docs/superpowers/plans/test.md")
                .display()
                .to_string(),
            allowed_prompts: vec![AllowedPrompt {
                tool: "Bash".to_string(),
                prompt: "run tests".to_string(),
            }],
            awaiting_approval: true,
            plan_was_edited: false,
        })
        .await
        .unwrap();

    let replay = store.replay().await.unwrap();

    assert_eq!(replay.plan_mode_status.mode, PlanMode::AwaitingApproval);
    assert!(matches!(
        replay.plan_mode_status,
        PlanModeStatus {
            awaiting_approval: true,
            ..
        }
    ));
    assert_eq!(
        replay.plan_mode_status.allowed_prompts[0].prompt,
        "run tests"
    );
}
