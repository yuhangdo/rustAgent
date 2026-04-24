use std::path::Path;

use claude_code_rs::api::{ChatMessage, ToolDefinition};
use claude_code_rs::prompting::{
    PromptBudget, PromptBuildRequest, PromptBuilder, PromptCacheScope, PromptSectionSource,
};
use serde_json::json;
use tempfile::tempdir;

fn tool_definition(name: &str) -> ToolDefinition {
    ToolDefinition::new(
        name,
        format!("tool {}", name),
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" }
            }
        }),
    )
}

fn request(
    workspace_root: &Path,
    current_working_dir: &Path,
    history: Vec<ChatMessage>,
) -> PromptBuildRequest {
    PromptBuildRequest {
        base_system_prompt: "Base instructions".to_string(),
        history,
        workspace_root: workspace_root.to_path_buf(),
        current_working_dir: Some(current_working_dir.to_path_buf()),
        tool_definitions: vec![tool_definition("search")],
        budget: PromptBudget {
            total_input_tokens: 4_096,
            reserved_output_tokens: 512,
            recent_message_count: 8,
        },
        entrypoint: "rust-agent-test".to_string(),
        version_fingerprint: Some("test-build".to_string()),
        global_config_root: None,
    }
}

#[test]
fn prompt_builder_merges_instruction_documents_from_global_and_workspace_ancestors() {
    let temp_dir = tempdir().expect("temp dir");
    let home_dir = temp_dir.path().join("home");
    let global_root = home_dir.join(".claude");
    let workspace_root = temp_dir.path().join("project");
    let current_dir = workspace_root.join("src").join("feature");

    std::fs::create_dir_all(&global_root).expect("global root");
    std::fs::create_dir_all(&current_dir).expect("current dir");

    std::fs::write(
        global_root.join("CLAUDE.md"),
        "# Global\nAlways be concise.",
    )
    .expect("global claude");
    std::fs::write(
        workspace_root.join("CLAUDE.md"),
        "# Project\nUse strict MVI.",
    )
    .expect("project claude");
    std::fs::write(
        workspace_root.join("src").join("AGENTS.md"),
        "# Module\nFeature modules own reducers.",
    )
    .expect("agents");

    let mut request = request(
        &workspace_root,
        &current_dir,
        vec![ChatMessage::user("latest question")],
    );
    request.global_config_root = Some(global_root.clone());

    let assembly = PromptBuilder::build(request).expect("prompt assembly");
    let system_parts = assembly.system_prompt_parts();
    let joined = system_parts.join("\n\n");

    assert!(joined.contains("Always be concise."));
    assert!(joined.contains("Use strict MVI."));
    assert!(joined.contains("Feature modules own reducers."));
    assert!(joined.find("Always be concise.") < joined.find("Use strict MVI."));
    assert!(joined.find("Use strict MVI.") < joined.find("Feature modules own reducers."));

    let instruction_sources = assembly
        .system_sections
        .iter()
        .filter(|section| {
            matches!(
                section.source,
                PromptSectionSource::GlobalInstruction
                    | PromptSectionSource::WorkspaceInstruction
                    | PromptSectionSource::DirectoryInstruction
            )
        })
        .map(|section| section.cache_scope)
        .collect::<Vec<_>>();

    assert!(instruction_sources.contains(&PromptCacheScope::Global));
    assert!(instruction_sources.contains(&PromptCacheScope::Org));
}

#[test]
fn prompt_builder_keeps_memory_only_in_user_context_and_prepends_it_to_messages() {
    let temp_dir = tempdir().expect("temp dir");
    let workspace_root = temp_dir.path().join("project");

    std::fs::create_dir_all(&workspace_root).expect("workspace");
    std::fs::write(
        workspace_root.join("MEMORY.md"),
        "# Memory\nAuth rewrite is blocked by compliance review.",
    )
    .expect("memory");

    let assembly = PromptBuilder::build(request(
        &workspace_root,
        &workspace_root,
        vec![ChatMessage::user("what should I know before editing auth?")],
    ))
    .expect("prompt assembly");
    let rendered = assembly.render();

    assert!(assembly
        .system_sections
        .iter()
        .all(|section| !matches!(section.source, PromptSectionSource::WorkspaceMemory)));
    assert!(assembly
        .user_context_sections
        .iter()
        .any(|section| matches!(section.source, PromptSectionSource::WorkspaceMemory)));
    assert!(rendered
        .effective_system_prompt
        .contains("Base instructions"));
    assert!(!rendered
        .effective_system_prompt
        .contains("Auth rewrite is blocked by compliance review."));

    let prepended = rendered
        .prepended_user_context
        .expect("prepended user context");
    assert_eq!(rendered.messages[1].role, "user");
    assert_eq!(rendered.messages[1].content, prepended.content);
    assert!(prepended
        .content
        .as_deref()
        .unwrap_or_default()
        .contains("Auth rewrite is blocked by compliance review."));
}

#[test]
fn prompt_builder_exposes_dynamic_boundary_and_openai_compatible_rendering() {
    let history = vec![
        ChatMessage::user("old request ".repeat(160)),
        ChatMessage::assistant("old response ".repeat(160)),
        ChatMessage::user("latest question"),
    ];

    let temp_dir = tempdir().expect("temp dir");
    let workspace_root = temp_dir.path().join("project");
    std::fs::create_dir_all(&workspace_root).expect("workspace");

    let mut request = request(&workspace_root, &workspace_root, history);
    request.budget = PromptBudget {
        total_input_tokens: 260,
        reserved_output_tokens: 64,
        recent_message_count: 1,
    };

    let assembly = PromptBuilder::build(request).expect("prompt assembly");
    let rendered = assembly.render();

    assert!(assembly.dynamic_boundary_index > 0);
    assert!(assembly.dynamic_boundary_index < assembly.system_sections.len());
    assert!(assembly.system_sections[..assembly.dynamic_boundary_index]
        .iter()
        .all(|section| !section.is_dynamic));
    assert!(assembly.system_sections[assembly.dynamic_boundary_index..]
        .iter()
        .any(|section| section.is_dynamic));

    assert_eq!(
        rendered
            .messages
            .first()
            .map(|message| message.role.as_str()),
        Some("system")
    );
    assert_eq!(
        rendered
            .messages
            .last()
            .and_then(|message| message.content.as_deref()),
        Some("latest question")
    );
    assert!(rendered.system_prompt_parts.len() >= assembly.system_sections.len());
}

#[test]
fn prompt_builder_respects_ignore_memory_directive_for_file_and_session_memory() {
    let temp_dir = tempdir().expect("temp dir");
    let workspace_root = temp_dir.path().join("project");

    std::fs::create_dir_all(&workspace_root).expect("workspace");
    std::fs::write(
        workspace_root.join("MEMORY.md"),
        "# Memory\nThis should be ignored.",
    )
    .expect("memory");

    let history = vec![
        ChatMessage::user("Please ignore memory for this task and inspect the current repo only."),
        ChatMessage::assistant("Earlier answer ".repeat(120)),
        ChatMessage::user("latest question"),
    ];

    let mut request = request(&workspace_root, &workspace_root, history);
    request.budget = PromptBudget {
        total_input_tokens: 220,
        reserved_output_tokens: 64,
        recent_message_count: 1,
    };

    let assembly = PromptBuilder::build(request).expect("prompt assembly");
    let rendered = assembly.render();
    let combined = rendered
        .messages
        .iter()
        .filter_map(|message| message.content.clone())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(assembly.user_context_sections.is_empty());
    assert!(!combined.contains("Project Memory"));
    assert!(!combined.contains("Session Memory"));
    assert!(!combined.contains("This should be ignored."));
}

#[test]
fn prompt_builder_reinjects_trimmed_history_as_session_memory_with_tool_summary() {
    let temp_dir = tempdir().expect("temp dir");
    let workspace_root = temp_dir.path().join("project");

    std::fs::create_dir_all(&workspace_root).expect("workspace");

    let history = vec![
        ChatMessage::user("please inspect the auth flow and note that rollout starts Thursday"),
        ChatMessage::assistant(
            "I inspected the flow and found the auth rewrite depends on compliance.",
        ),
        ChatMessage::assistant_with_tools(vec![claude_code_rs::api::ToolCall {
            id: "call_2".to_string(),
            r#type: "function".to_string(),
            function: claude_code_rs::api::ToolCallFunction {
                name: "search".to_string(),
                arguments: "{\"path\":\".\",\"pattern\":\"auth\"}".to_string(),
            },
        }]),
        ChatMessage::tool("call_2", "search results ".repeat(300)),
        ChatMessage::user("latest question"),
    ];

    let mut request = request(&workspace_root, &workspace_root, history);
    request.budget = PromptBudget {
        total_input_tokens: 240,
        reserved_output_tokens: 64,
        recent_message_count: 1,
    };

    let assembly = PromptBuilder::build(request).expect("prompt assembly");
    let session_memory = assembly
        .user_context_sections
        .iter()
        .find(|section| matches!(section.source, PromptSectionSource::SessionMemory))
        .expect("session memory section");

    assert!(assembly.trim_report.dropped_message_count > 0);
    assert!(session_memory.content.contains("Earlier user requests"));
    assert!(session_memory.content.contains("rollout starts Thursday"));
    assert!(session_memory.content.contains("Earlier tools used"));
    assert!(session_memory.content.contains("search"));
    assert!(session_memory
        .content
        .contains("Earlier tool result messages: 1"));
    assert!(assembly
        .system_sections
        .iter()
        .any(|section| matches!(section.source, PromptSectionSource::ContextTrimNotice)));
}

#[test]
fn prompt_builder_compacts_old_tool_results_before_dropping_recent_turns() {
    let temp_dir = tempdir().expect("temp dir");
    let workspace_root = temp_dir.path().join("project");

    std::fs::create_dir_all(&workspace_root).expect("workspace");

    let long_tool_output = "tool output ".repeat(500);
    let history = vec![
        ChatMessage::assistant_with_tools(vec![claude_code_rs::api::ToolCall {
            id: "call_1".to_string(),
            r#type: "function".to_string(),
            function: claude_code_rs::api::ToolCallFunction {
                name: "search".to_string(),
                arguments: "{\"path\":\".\"}".to_string(),
            },
        }]),
        ChatMessage::tool("call_1", long_tool_output.clone()),
        ChatMessage::user("recent question"),
        ChatMessage::assistant("recent answer"),
    ];

    let mut request = request(&workspace_root, &workspace_root, history);
    request.budget = PromptBudget {
        total_input_tokens: 900,
        reserved_output_tokens: 64,
        recent_message_count: 2,
    };

    let assembly = PromptBuilder::build(request).expect("prompt assembly");
    let tool_message = assembly
        .history_messages
        .iter()
        .find(|message| message.role == "tool")
        .and_then(|message| message.content.clone())
        .expect("tool message");

    assert!(assembly.trim_report.compacted_tool_message_count > 0);
    assert!(tool_message.contains("[compacted tool result]"));
    assert!(tool_message.len() < long_tool_output.len());
    assert!(assembly
        .history_messages
        .iter()
        .any(|message| message.content.as_deref() == Some("recent question")));
}
