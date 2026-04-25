use std::path::Path;
use std::sync::Mutex;

use async_trait::async_trait;
use claude_code_rs::api::{ChatMessage, ToolDefinition};
use claude_code_rs::prompting::{
    ProjectMemorySelectionQuery, ProjectMemorySelector, PromptBudget, PromptBuildRequest,
    PromptBuilder, PromptCacheScope, PromptSectionSource,
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
        memory_enabled: true,
        auto_memory_directory: None,
        already_surfaced_memory_paths: Vec::new(),
    }
}

#[derive(Debug)]
struct StaticMemorySelector {
    selected_paths: Vec<String>,
    seen_query: Mutex<Option<ProjectMemorySelectionQuery>>,
}

impl StaticMemorySelector {
    fn new(selected_paths: Vec<&str>) -> Self {
        Self {
            selected_paths: selected_paths.into_iter().map(str::to_string).collect(),
            seen_query: Mutex::new(None),
        }
    }

    fn seen_query(&self) -> ProjectMemorySelectionQuery {
        self.seen_query
            .lock()
            .expect("selector query lock")
            .clone()
            .expect("selector query")
    }
}

#[async_trait]
impl ProjectMemorySelector for StaticMemorySelector {
    async fn select(&self, query: ProjectMemorySelectionQuery) -> anyhow::Result<Vec<String>> {
        *self.seen_query.lock().expect("selector query lock") = Some(query);
        Ok(self.selected_paths.clone())
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

#[tokio::test]
async fn prompt_builder_loads_project_memory_index_and_selected_files_from_auto_memory_dir() {
    let temp_dir = tempdir().expect("temp dir");
    let workspace_root = temp_dir.path().join("project");
    let memory_root = temp_dir.path().join("memory");

    std::fs::create_dir_all(&workspace_root).expect("workspace");
    std::fs::create_dir_all(&memory_root).expect("memory root");
    std::fs::write(
        workspace_root.join("MEMORY.md"),
        "# Legacy Memory\nThis should not win over auto memory.",
    )
    .expect("legacy memory");
    std::fs::write(
        memory_root.join("MEMORY.md"),
        "- [Auth rollout](project_auth_rollout.md) - rollout starts Thursday\n- [Testing preference](feedback_testing.md) - avoid mock databases\n",
    )
    .expect("memory index");
    std::fs::write(
        memory_root.join("project_auth_rollout.md"),
        "---\nname: Auth rollout\ndescription: rollout starts Thursday and auth rewrite is blocked by compliance\ntype: project\n---\nWhy: Release freeze starts Thursday.\nHow to apply: Treat auth changes as compliance-sensitive.\n",
    )
    .expect("project memory");
    std::fs::write(
        memory_root.join("feedback_testing.md"),
        "---\nname: Testing preference\ndescription: prefer integration tests and do not mock databases\ntype: feedback\n---\nWhy: The user has explicitly validated real database coverage.\nHow to apply: favor integration coverage over mocked persistence.\n",
    )
    .expect("feedback memory");

    let selector = StaticMemorySelector::new(vec!["project_auth_rollout.md"]);
    let mut request = request(
        &workspace_root,
        &workspace_root,
        vec![ChatMessage::user(
            "what should I know before touching auth rollout?",
        )],
    );
    request.auto_memory_directory = Some(memory_root.clone());

    let assembly = PromptBuilder::build_with_selector(request, Some(&selector))
        .await
        .expect("prompt assembly");
    let rendered = assembly.render();
    let combined = rendered
        .messages
        .iter()
        .filter_map(|message| message.content.clone())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(combined.contains("Project Memory Index"));
    assert!(combined.contains("rollout starts Thursday"));
    assert!(combined.contains("Relevant Project Memory"));
    assert!(combined.contains("Treat auth changes as compliance-sensitive."));
    assert!(!combined.contains("This should not win over auto memory."));
    assert_eq!(
        assembly.surfaced_memory_paths,
        vec!["project_auth_rollout.md".to_string()]
    );

    let seen_query = selector.seen_query();
    assert_eq!(
        seen_query.query,
        "what should I know before touching auth rollout?"
    );
    assert!(seen_query
        .memory_index_excerpt
        .as_deref()
        .unwrap_or_default()
        .contains("Auth rollout"));
}

#[tokio::test]
async fn prompt_builder_filters_already_surfaced_and_recent_tool_reference_noise() {
    let temp_dir = tempdir().expect("temp dir");
    let workspace_root = temp_dir.path().join("project");
    let memory_root = temp_dir.path().join("memory");

    std::fs::create_dir_all(&workspace_root).expect("workspace");
    std::fs::create_dir_all(&memory_root).expect("memory root");
    std::fs::write(
        memory_root.join("MEMORY.md"),
        "- [Search API](reference_search_api.md) - search tool api reference\n- [Search warning](feedback_search_warning.md) - search tool can miss hidden files\n- [Auth rollout](project_auth_rollout.md) - rollout starts Thursday\n",
    )
    .expect("memory index");
    std::fs::write(
        memory_root.join("reference_search_api.md"),
        "---\nname: Search API\ndescription: search tool api reference and usage docs\ntype: reference\n---\nUse the search tool with pattern filters.\n",
    )
    .expect("search api");
    std::fs::write(
        memory_root.join("feedback_search_warning.md"),
        "---\nname: Search warning\ndescription: search tool can miss hidden files unless the path is explicit\ntype: feedback\n---\nWhy: hidden files were skipped in a prior run.\nHow to apply: prefer explicit paths for hidden file searches.\n",
    )
    .expect("search warning");
    std::fs::write(
        memory_root.join("project_auth_rollout.md"),
        "---\nname: Auth rollout\ndescription: rollout starts Thursday\ntype: project\n---\nFreeze starts Thursday.\n",
    )
    .expect("auth rollout");

    let selector = StaticMemorySelector::new(vec!["feedback_search_warning.md"]);
    let mut request = request(
        &workspace_root,
        &workspace_root,
        vec![
            ChatMessage::assistant_with_tools(vec![claude_code_rs::api::ToolCall {
                id: "call_search".to_string(),
                r#type: "function".to_string(),
                function: claude_code_rs::api::ToolCallFunction {
                    name: "search".to_string(),
                    arguments: "{\"path\":\".\",\"pattern\":\"auth\"}".to_string(),
                },
            }]),
            ChatMessage::tool("call_search", "search results"),
            ChatMessage::user("anything else I should remember about auth?"),
        ],
    );
    request.auto_memory_directory = Some(memory_root.clone());
    request.already_surfaced_memory_paths = vec!["project_auth_rollout.md".to_string()];

    let assembly = PromptBuilder::build_with_selector(request, Some(&selector))
        .await
        .expect("prompt assembly");
    let seen_query = selector.seen_query();

    assert_eq!(seen_query.recent_tools, vec!["search".to_string()]);
    assert!(seen_query
        .already_surfaced_memory_paths
        .contains(&"project_auth_rollout.md".to_string()));
    assert!(!seen_query
        .candidates
        .iter()
        .any(|candidate| candidate.path == "project_auth_rollout.md"));
    assert!(!seen_query.candidates.iter().any(|candidate| {
        candidate.path == "reference_search_api.md"
            && candidate.memory_type.as_deref() == Some("reference")
    }));
    assert!(seen_query.candidates.iter().any(|candidate| {
        candidate.path == "feedback_search_warning.md"
            && candidate.memory_type.as_deref() == Some("feedback")
    }));
    assert_eq!(
        assembly.surfaced_memory_paths,
        vec!["feedback_search_warning.md".to_string()]
    );
}

#[test]
fn prompt_builder_truncates_project_memory_index_by_lines_and_bytes() {
    let temp_dir = tempdir().expect("temp dir");
    let workspace_root = temp_dir.path().join("project");
    let memory_root = temp_dir.path().join("memory");

    std::fs::create_dir_all(&workspace_root).expect("workspace");
    std::fs::create_dir_all(&memory_root).expect("memory root");

    let long_lines = (0..260)
        .map(|index| format!("- [Entry {index}](entry_{index}.md) - {}", "x".repeat(180)))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(memory_root.join("MEMORY.md"), long_lines).expect("memory index");

    let mut request = request(
        &workspace_root,
        &workspace_root,
        vec![ChatMessage::user("latest question")],
    );
    request.auto_memory_directory = Some(memory_root.clone());

    let assembly = PromptBuilder::build(request).expect("prompt assembly");
    let index_section = assembly
        .user_context_sections
        .iter()
        .find(|section| matches!(section.source, PromptSectionSource::ProjectMemoryIndex))
        .expect("memory index section");

    assert!(index_section.content.contains("WARNING: MEMORY.md"));
    assert!(index_section.content.lines().count() <= 210);
    assert!(index_section.content.len() <= 25_400);
}
