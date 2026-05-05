use claude_code_rs::api::{ChatMessage, ToolCall, ToolCallFunction};
use claude_code_rs::fast_path::{
    build_execution_batches, hard_route_decision, validate_quick_plan,
    validate_quick_plan_for_workspace, validate_read_only_command,
    validate_read_only_command_in_workspace, ExecutionModeHint, HardRouteDecision, QuickRouteInput,
    QuickToolPlan, QuickToolStep,
};
use serde_json::json;
use tempfile::tempdir;

#[test]
fn hard_route_forces_slow_path_for_write_intent() {
    let input = QuickRouteInput {
        hint: ExecutionModeHint::Auto,
        history: vec![ChatMessage::user(
            "Please edit src/lib.rs to add a new mode.",
        )],
        has_additional_context_sections: false,
    };

    let decision = hard_route_decision(&input);

    assert_eq!(decision, HardRouteDecision::ForceSlow);
}

#[test]
fn hard_route_marks_simple_read_request_as_candidate() {
    let input = QuickRouteInput {
        hint: ExecutionModeHint::Auto,
        history: vec![ChatMessage::user(
            "Find where QuerySubmitRequest is used and summarize the call sites.",
        )],
        has_additional_context_sections: false,
    };

    let decision = hard_route_decision(&input);

    assert_eq!(decision, HardRouteDecision::QuickCandidate);
}

#[test]
fn validate_read_only_command_allows_git_status_and_rejects_command_chaining() {
    validate_read_only_command("git status --short").expect("git status should be allowed");
    assert!(validate_read_only_command("git status && git add .").is_err());
    assert!(validate_read_only_command("git status & git add .").is_err());
}

#[test]
fn hard_route_does_not_permanently_disable_quick_path_after_old_tool_history() {
    let input = QuickRouteInput {
        hint: ExecutionModeHint::Auto,
        history: vec![
            ChatMessage::user("Find the request type."),
            ChatMessage::assistant_with_tools(vec![ToolCall {
                id: "call_1".to_string(),
                r#type: "function".to_string(),
                function: ToolCallFunction {
                    name: "search".to_string(),
                    arguments: "{\"path\":\"src\",\"pattern\":\"QuerySubmitRequest\"}".to_string(),
                },
            }]),
            ChatMessage::tool("call_1", "older tool output"),
            ChatMessage::assistant("That earlier lookup is finished."),
            ChatMessage::user("Now just list the relevant files."),
        ],
        has_additional_context_sections: false,
    };

    let decision = hard_route_decision(&input);

    assert_eq!(decision, HardRouteDecision::QuickCandidate);
}

#[test]
fn workspace_scoped_validation_rejects_paths_outside_workspace() {
    let temp = tempdir().expect("tempdir");
    let workspace_root = temp.path();

    assert!(validate_read_only_command_in_workspace("type ../secret.txt", workspace_root).is_err());

    let plan = QuickToolPlan {
        goal: "Read outside workspace".to_string(),
        steps: vec![QuickToolStep {
            id: "read".to_string(),
            tool: "file_read".to_string(),
            input: json!({"file_path":"../secret.txt"}),
            depends_on: Vec::new(),
            read_only: true,
            reason: "try to escape the workspace".to_string(),
        }],
    };

    assert!(validate_quick_plan_for_workspace(&plan, workspace_root).is_err());
}

#[test]
fn validate_quick_plan_rejects_unapproved_tool_and_too_many_steps() {
    let invalid_tool_plan = QuickToolPlan {
        goal: "Modify a file".to_string(),
        steps: vec![QuickToolStep {
            id: "step_1".to_string(),
            tool: "file_write".to_string(),
            input: json!({"file_path":"src/lib.rs","content":"oops"}),
            depends_on: Vec::new(),
            read_only: false,
            reason: "write the requested file".to_string(),
        }],
    };

    assert!(validate_quick_plan(&invalid_tool_plan).is_err());

    let too_many_steps = QuickToolPlan {
        goal: "Use too many tools".to_string(),
        steps: (0..4)
            .map(|index| QuickToolStep {
                id: format!("step_{}", index),
                tool: "search".to_string(),
                input: json!({"path":"src","pattern":"AgentRuntime"}),
                depends_on: Vec::new(),
                read_only: true,
                reason: "inspect code".to_string(),
            })
            .collect(),
    };

    assert!(validate_quick_plan(&too_many_steps).is_err());
}

#[test]
fn build_execution_batches_groups_independent_steps_before_dependent_step() {
    let plan = QuickToolPlan {
        goal: "Inspect request flow".to_string(),
        steps: vec![
            QuickToolStep {
                id: "search".to_string(),
                tool: "search".to_string(),
                input: json!({"path":"src","pattern":"QuerySubmitRequest"}),
                depends_on: Vec::new(),
                read_only: true,
                reason: "find the request type".to_string(),
            },
            QuickToolStep {
                id: "list".to_string(),
                tool: "list_files".to_string(),
                input: json!({"path":"src/query_engine","recursive":false}),
                depends_on: Vec::new(),
                read_only: true,
                reason: "see query engine files".to_string(),
            },
            QuickToolStep {
                id: "read".to_string(),
                tool: "file_read".to_string(),
                input: json!({"file_path":"src/query_engine/mod.rs"}),
                depends_on: vec!["search".to_string()],
                read_only: true,
                reason: "read the relevant module".to_string(),
            },
        ],
    };

    let batches = build_execution_batches(&plan).expect("batch plan");

    assert_eq!(batches.len(), 2);
    assert_eq!(batches[0].len(), 2);
    assert_eq!(batches[1].len(), 1);
    assert_eq!(batches[1][0].id, "read");
}
