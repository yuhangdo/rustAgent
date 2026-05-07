use claude_code_rs::api::ChatMessage;
use claude_code_rs::auto_mode::{
    auto_mode_system_prompt, restore_dangerous_permissions,
    strip_dangerous_permissions_for_auto_mode, AutoModeClassifierStage, AutoModeConfig,
    AutoModeDecisionBehavior, AutoModePromptKind, AutoModeSession, AutoModeToolCall,
    PermissionRule, PermissionRuleBehavior,
};
use claude_code_rs::query_engine::{TranscriptEvent, TranscriptStore};
use claude_code_rs::tools::ToolAccess;
use serde_json::json;

#[test]
fn auto_mode_strips_and_restores_dangerous_allow_rules() {
    let rules = vec![
        PermissionRule::new("Bash", Some("git status:*"), PermissionRuleBehavior::Allow),
        PermissionRule::new("Bash", Some("python:*"), PermissionRuleBehavior::Allow),
        PermissionRule::new("Agent", Some("*"), PermissionRuleBehavior::Allow),
        PermissionRule::new("Bash", Some("rm -rf:*"), PermissionRuleBehavior::Deny),
    ];

    let stripped = strip_dangerous_permissions_for_auto_mode(&rules);

    assert_eq!(stripped.safe_rules.len(), 2);
    assert_eq!(stripped.stripped_rules.len(), 2);
    assert!(stripped
        .stripped_rules
        .iter()
        .any(|rule| rule.tool_name == "Bash" && rule.rule_content.as_deref() == Some("python:*")));
    assert!(stripped
        .stripped_rules
        .iter()
        .any(|rule| rule.tool_name == "Agent"));

    let restored = restore_dangerous_permissions(stripped.safe_rules, stripped.stripped_rules);
    assert_eq!(restored.len(), 4);
}

#[tokio::test]
async fn auto_mode_classifier_allows_safe_operations_and_denies_dangerous_commands() {
    let temp = tempfile::tempdir().unwrap();
    let session = AutoModeSession::new(
        temp.path().to_path_buf(),
        "sonnet".to_string(),
        AutoModeConfig::enabled(),
    );
    session.enter("default").await.unwrap();

    let read_decision = session
        .classify_tool_call(AutoModeToolCall::new(
            "search",
            json!({"path": temp.path(), "pattern": "AutoMode"}),
            ToolAccess::ReadOnly,
            vec![ChatMessage::user("查一下 AutoMode 实现")],
        ))
        .await;
    assert_eq!(read_decision.behavior, AutoModeDecisionBehavior::Allow);
    assert_eq!(read_decision.stage, Some(AutoModeClassifierStage::Fast));

    let test_decision = session
        .classify_tool_call(AutoModeToolCall::new(
            "execute_command",
            json!({"command": "cargo test --tests", "cwd": temp.path()}),
            ToolAccess::Write,
            vec![ChatMessage::user("实现后跑测试")],
        ))
        .await;
    assert_eq!(test_decision.behavior, AutoModeDecisionBehavior::Allow);
    assert_eq!(test_decision.stage, Some(AutoModeClassifierStage::Thinking));

    let dangerous_decision = session
        .classify_tool_call(AutoModeToolCall::new(
            "execute_command",
            json!({"command": "rm -rf .git", "cwd": temp.path()}),
            ToolAccess::Write,
            vec![ChatMessage::user("看一下仓库状态")],
        ))
        .await;
    assert_eq!(dangerous_decision.behavior, AutoModeDecisionBehavior::Deny);
    assert!(dangerous_decision.should_block);

    let unknown_decision = session
        .classify_tool_call(AutoModeToolCall::new(
            "execute_command",
            json!({"command": "poetry run custom-deploy", "cwd": temp.path()}),
            ToolAccess::Write,
            vec![ChatMessage::user("继续处理")],
        ))
        .await;
    assert_eq!(unknown_decision.behavior, AutoModeDecisionBehavior::Ask);
}

#[tokio::test]
async fn auto_mode_allows_workspace_edits_and_denies_path_escape() {
    let temp = tempfile::tempdir().unwrap();
    let session = AutoModeSession::new(
        temp.path().to_path_buf(),
        "sonnet".to_string(),
        AutoModeConfig::enabled(),
    );
    session.enter("default").await.unwrap();

    let inside = session
        .classify_tool_call(AutoModeToolCall::new(
            "file_write",
            json!({"file_path": temp.path().join("src/lib.rs"), "content": "mod x;"}),
            ToolAccess::Write,
            vec![ChatMessage::user("在项目里补实现")],
        ))
        .await;
    assert_eq!(inside.behavior, AutoModeDecisionBehavior::Allow);

    let outside = session
        .classify_tool_call(AutoModeToolCall::new(
            "file_write",
            json!({"file_path": temp.path().join("..").join("outside.txt"), "content": "oops"}),
            ToolAccess::Write,
            vec![ChatMessage::user("在项目里补实现")],
        ))
        .await;
    assert_eq!(outside.behavior, AutoModeDecisionBehavior::Deny);
}

#[test]
fn auto_mode_prompt_renders_full_sparse_and_exit_instructions() {
    let base = "Base system prompt.";

    let full = auto_mode_system_prompt(base, AutoModePromptKind::FullInstructions);
    assert!(full.contains("Auto mode is active"));
    assert!(full.contains("Execute immediately"));
    assert!(full.contains("Avoid data exfiltration"));

    let sparse = auto_mode_system_prompt(base, AutoModePromptKind::SparseReminder);
    assert!(sparse.contains("Auto mode still active"));
    assert!(sparse.len() < full.len());

    let exit = auto_mode_system_prompt(base, AutoModePromptKind::ExitInstructions);
    assert!(exit.contains("exited auto mode"));
}

#[test]
fn unsupported_models_and_circuit_breaker_prevent_activation() {
    let temp = tempfile::tempdir().unwrap();
    let unsupported = AutoModeSession::new(
        temp.path().to_path_buf(),
        "tiny-local-model".to_string(),
        AutoModeConfig::enabled(),
    );
    assert!(!unsupported.is_available());

    let mut config = AutoModeConfig::enabled();
    config.circuit_breaker_enabled = true;
    let circuit_broken =
        AutoModeSession::new(temp.path().to_path_buf(), "sonnet".to_string(), config);
    assert!(!circuit_broken.is_available());
}

#[tokio::test]
async fn transcript_replay_restores_auto_mode_status_and_decisions() {
    let temp = tempfile::tempdir().unwrap();
    let store = TranscriptStore::new(temp.path().to_path_buf());

    store
        .append(&TranscriptEvent::AutoModeEntered {
            previous_mode: "default".to_string(),
            model: "sonnet".to_string(),
            stripped_dangerous_rules: vec!["Bash(python:*)".to_string()],
        })
        .await
        .unwrap();
    store
        .append(&TranscriptEvent::AutoModeDecisionRecorded {
            tool_name: "execute_command".to_string(),
            behavior: AutoModeDecisionBehavior::Deny,
            reason: "dangerous recursive deletion".to_string(),
            stage: Some(AutoModeClassifierStage::Fast),
            unavailable: false,
            transcript_too_long: false,
        })
        .await
        .unwrap();

    let replay = store.replay().await.unwrap();

    assert!(replay.auto_mode_status.active);
    assert_eq!(replay.auto_mode_status.model, "sonnet");
    assert_eq!(replay.auto_mode_decisions.len(), 1);
    assert_eq!(
        replay.auto_mode_decisions[0].behavior,
        AutoModeDecisionBehavior::Deny
    );
}
