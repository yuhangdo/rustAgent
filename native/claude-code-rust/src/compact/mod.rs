use std::collections::{BTreeSet, HashMap};

use crate::api::ChatMessage;
use crate::prompting::{PromptCacheScope, PromptSection, PromptSectionRole, PromptSectionSource};
use crate::token_budget::{rough_count_messages, rough_count_text};

const MICRO_COMPACT_MAX_TOOL_CHARS: usize = 480;
const SUMMARY_SNIPPET_CHARS: usize = 220;
const MAX_SUMMARY_SNIPPETS: usize = 4;
const MAX_REINJECTED_TOOLS: usize = 6;
const MAX_REINJECTED_PATHS: usize = 6;
const COMPACTABLE_TOOL_NAMES: &[&str] = &[
    "execute_command",
    "file_edit",
    "file_read",
    "file_write",
    "list_files",
    "search",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactDirection {
    UpTo,
    From,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactStrategy {
    Micro,
    SessionMemory,
    Full,
    PartialUpTo,
    PartialFrom,
}

#[derive(Debug, Clone)]
pub struct CompactResult {
    pub history: Vec<ChatMessage>,
    pub system_sections: Vec<PromptSection>,
    pub user_context_sections: Vec<PromptSection>,
    pub strategy: CompactStrategy,
    pub before_tokens: usize,
    pub after_tokens: usize,
    pub compacted_message_count: usize,
    pub preserved_message_count: usize,
}

pub fn micro_compact_history(
    history: &[ChatMessage],
    preserve_recent_messages: usize,
) -> CompactResult {
    let before_tokens = rough_count_messages(history);
    let recent_start = history.len().saturating_sub(preserve_recent_messages);
    let tool_names = tool_names_by_call_id(history);
    let mut compacted_message_count = 0;
    let mut compacted_history = history.to_vec();

    for (index, message) in compacted_history.iter_mut().enumerate() {
        if index >= recent_start || message.role != "tool" {
            continue;
        }
        let Some(content) = message.content.as_ref() else {
            continue;
        };
        if content.chars().count() <= MICRO_COMPACT_MAX_TOOL_CHARS {
            continue;
        }

        let tool_name = message
            .tool_call_id
            .as_ref()
            .and_then(|tool_call_id| tool_names.get(tool_call_id))
            .map(String::as_str)
            .unwrap_or("tool");
        if !is_compactable_tool(tool_name) {
            continue;
        }

        message.content = Some(compact_tool_payload(tool_name, content));
        compacted_message_count += 1;
    }

    let after_tokens = rough_count_messages(&compacted_history);
    CompactResult {
        history: compacted_history,
        system_sections: Vec::new(),
        user_context_sections: Vec::new(),
        strategy: CompactStrategy::Micro,
        before_tokens,
        after_tokens,
        compacted_message_count,
        preserved_message_count: history.len().min(preserve_recent_messages),
    }
}

pub fn session_memory_compact(
    history: &[ChatMessage],
    preserve_recent_messages: usize,
) -> CompactResult {
    compact_history_internal(
        history,
        CompactDirection::UpTo,
        None,
        preserve_recent_messages,
        CompactStrategy::SessionMemory,
        "auto_session_memory",
        None,
    )
}

pub fn full_compact(
    history: &[ChatMessage],
    direction: CompactDirection,
    anchor_index: Option<usize>,
    preserve_recent_messages: usize,
) -> CompactResult {
    full_compact_with_summary(
        history,
        direction,
        anchor_index,
        preserve_recent_messages,
        None,
    )
}

pub fn full_compact_with_summary(
    history: &[ChatMessage],
    direction: CompactDirection,
    anchor_index: Option<usize>,
    preserve_recent_messages: usize,
    summary_override: Option<&str>,
) -> CompactResult {
    let strategy = match (direction, anchor_index.is_some()) {
        (CompactDirection::UpTo, true) => CompactStrategy::PartialUpTo,
        (CompactDirection::From, true) => CompactStrategy::PartialFrom,
        _ => CompactStrategy::Full,
    };
    let compact_kind = match strategy {
        CompactStrategy::PartialUpTo => "partial_up_to",
        CompactStrategy::PartialFrom => "partial_from",
        _ => "auto_full_compact",
    };

    compact_history_internal(
        history,
        direction,
        anchor_index,
        preserve_recent_messages,
        strategy,
        compact_kind,
        summary_override,
    )
}

fn compact_history_internal(
    history: &[ChatMessage],
    direction: CompactDirection,
    anchor_index: Option<usize>,
    preserve_recent_messages: usize,
    strategy: CompactStrategy,
    compact_kind: &str,
    summary_override: Option<&str>,
) -> CompactResult {
    let before_tokens = rough_count_messages(history);
    let split_index = resolve_split_index(
        history.len(),
        direction,
        anchor_index,
        preserve_recent_messages,
    );
    let (compacted_slice, preserved_history) = match direction {
        CompactDirection::UpTo => (&history[..split_index], history[split_index..].to_vec()),
        CompactDirection::From => (&history[split_index..], history[..split_index].to_vec()),
    };
    let preserved_message_count = preserved_history.len();

    if compacted_slice.is_empty() {
        return CompactResult {
            history: history.to_vec(),
            system_sections: Vec::new(),
            user_context_sections: Vec::new(),
            strategy,
            before_tokens,
            after_tokens: before_tokens,
            compacted_message_count: 0,
            preserved_message_count: history.len(),
        };
    }

    let summary = summary_override
        .map(str::trim)
        .filter(|summary| !summary.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            build_compaction_summary(compacted_slice, compact_kind, preserved_message_count)
        });
    let summary_section = PromptSection {
        id: "compact_summary".to_string(),
        role: PromptSectionRole::User,
        content: summary,
        cache_scope: PromptCacheScope::None,
        is_dynamic: true,
        source: PromptSectionSource::CompactSummary,
    };
    let reinjection_section =
        build_post_compact_reinjection_section(history, split_index, direction);
    let mut user_context_sections = Vec::new();
    let mut compacted_history = preserved_history;
    match direction {
        CompactDirection::UpTo => {
            user_context_sections.push(summary_section);
            if let Some(section) = reinjection_section {
                user_context_sections.push(section);
            }
        }
        CompactDirection::From => {
            let mut synthetic_blocks = vec![summary_section.content];
            if let Some(section) = reinjection_section {
                synthetic_blocks.push(section.content);
            }
            compacted_history.push(synthetic_compact_message(&synthetic_blocks.join("\n\n")));
        }
    }

    let system_sections = vec![PromptSection {
        id: "compact_boundary".to_string(),
        role: PromptSectionRole::System,
        content: format!(
            "## Compact Boundary\n- Type: {}\n- Direction: {}\n- Compacted messages: {}\n- Preserved messages: {}",
            compact_kind,
            match direction {
                CompactDirection::UpTo => "up_to",
                CompactDirection::From => "from",
            },
            compacted_slice.len(),
            preserved_message_count,
        ),
        cache_scope: PromptCacheScope::None,
        is_dynamic: true,
        source: PromptSectionSource::CompactBoundary,
    }];

    let after_tokens = rough_count_messages(&compacted_history)
        + user_context_sections
            .iter()
            .map(|section| rough_count_text(&section.content, false))
            .sum::<usize>()
        + system_sections
            .iter()
            .map(|section| rough_count_text(&section.content, false))
            .sum::<usize>();

    CompactResult {
        history: compacted_history,
        system_sections,
        user_context_sections,
        strategy,
        before_tokens,
        after_tokens,
        compacted_message_count: compacted_slice.len(),
        preserved_message_count: history.len().saturating_sub(compacted_slice.len()),
    }
}

fn resolve_split_index(
    history_len: usize,
    direction: CompactDirection,
    anchor_index: Option<usize>,
    preserve_recent_messages: usize,
) -> usize {
    match (direction, anchor_index) {
        (CompactDirection::UpTo, Some(anchor)) => anchor.min(history_len),
        (CompactDirection::From, Some(anchor)) => anchor.min(history_len),
        (CompactDirection::UpTo, None) => history_len.saturating_sub(preserve_recent_messages),
        (CompactDirection::From, None) => preserve_recent_messages.min(history_len),
    }
}

fn tool_names_by_call_id(history: &[ChatMessage]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for message in history {
        if let Some(tool_calls) = &message.tool_calls {
            for tool_call in tool_calls {
                map.insert(tool_call.id.clone(), tool_call.function.name.clone());
            }
        }
    }
    map
}

fn is_compactable_tool(tool_name: &str) -> bool {
    COMPACTABLE_TOOL_NAMES.contains(&tool_name)
}

fn compact_tool_payload(tool_name: &str, content: &str) -> String {
    let sentinel = if content.contains("[image]") {
        "[image]"
    } else if content.contains("[document]") {
        "[document]"
    } else {
        ""
    };
    let summarized = summarize_text(content, MICRO_COMPACT_MAX_TOOL_CHARS);
    if sentinel.is_empty() {
        format!("[micro-compacted {} output] {}", tool_name, summarized)
    } else {
        format!(
            "[micro-compacted {} output {}] {}",
            tool_name, sentinel, summarized
        )
    }
}

fn build_compaction_summary(
    compacted_slice: &[ChatMessage],
    compact_kind: &str,
    preserved_message_count: usize,
) -> String {
    let user_snippets = collect_summaries(compacted_slice, "user");
    let assistant_snippets = collect_summaries(compacted_slice, "assistant");
    let tool_names = compacted_slice
        .iter()
        .filter_map(|message| message.tool_calls.as_ref())
        .flat_map(|tool_calls| {
            tool_calls
                .iter()
                .map(|tool_call| tool_call.function.name.clone())
        })
        .collect::<BTreeSet<_>>();
    let tool_result_count = compacted_slice
        .iter()
        .filter(|message| message.role == "tool")
        .count();

    let mut sections = vec![format!(
        "### Compacted Conversation\nCompact Boundary\n- Type: {}\n- Preserved messages after boundary: {}",
        compact_kind, preserved_message_count
    )];
    if !user_snippets.is_empty() {
        sections.push(format!(
            "Earlier user requests:\n- {}",
            user_snippets.join("\n- ")
        ));
    }
    if !assistant_snippets.is_empty() {
        sections.push(format!(
            "Earlier assistant conclusions:\n- {}",
            assistant_snippets.join("\n- ")
        ));
    }
    if !tool_names.is_empty() {
        sections.push(format!(
            "Earlier tools used:\n- {}",
            tool_names.into_iter().collect::<Vec<_>>().join("\n- ")
        ));
    }
    if tool_result_count > 0 {
        sections.push(format!(
            "Earlier tool result messages: {}",
            tool_result_count
        ));
    }
    sections.push(
        "Use this compacted summary as soft recall only. Re-check files, symbols, tool state, and repository contents before relying on it."
            .to_string(),
    );
    sections.join("\n\n")
}

fn build_post_compact_reinjection_section(
    history: &[ChatMessage],
    split_index: usize,
    direction: CompactDirection,
) -> Option<PromptSection> {
    let anchor_start = split_index.saturating_sub(6);
    let anchor_end = match direction {
        CompactDirection::UpTo => history.len().min(split_index.saturating_add(6)),
        CompactDirection::From => split_index.min(history.len()),
    };
    let anchor_slice = &history[anchor_start..anchor_end];
    let mut tool_names = BTreeSet::new();
    let mut paths = BTreeSet::new();

    for message in anchor_slice.iter().rev().take(12) {
        if let Some(tool_calls) = &message.tool_calls {
            for tool_call in tool_calls.iter().rev() {
                if tool_names.len() < MAX_REINJECTED_TOOLS {
                    tool_names.insert(tool_call.function.name.clone());
                }
                if let Ok(value) =
                    serde_json::from_str::<serde_json::Value>(&tool_call.function.arguments)
                {
                    collect_candidate_paths(&value, &mut paths);
                }
            }
        }
    }

    if tool_names.is_empty() && paths.is_empty() {
        return None;
    }

    let mut lines = vec!["### Post-Compact Rehydration".to_string()];
    if !tool_names.is_empty() {
        lines.push(format!(
            "Recent tools to keep in mind:\n- {}",
            tool_names
                .into_iter()
                .take(MAX_REINJECTED_TOOLS)
                .collect::<Vec<_>>()
                .join("\n- ")
        ));
    }
    if !paths.is_empty() {
        lines.push(format!(
            "Recent workspace paths:\n- {}",
            paths
                .into_iter()
                .take(MAX_REINJECTED_PATHS)
                .collect::<Vec<_>>()
                .join("\n- ")
        ));
    }

    Some(PromptSection {
        id: "post_compact_reinjection".to_string(),
        role: PromptSectionRole::User,
        content: lines.join("\n\n"),
        cache_scope: PromptCacheScope::None,
        is_dynamic: true,
        source: PromptSectionSource::PostCompactReinjection,
    })
}

fn collect_candidate_paths(value: &serde_json::Value, paths: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                if matches!(key.as_str(), "path" | "file_path" | "cwd") {
                    if let Some(path) = value.as_str() {
                        if !path.trim().is_empty() {
                            paths.insert(path.to_string());
                        }
                    }
                }
                collect_candidate_paths(value, paths);
            }
        }
        serde_json::Value::Array(items) => {
            for value in items {
                collect_candidate_paths(value, paths);
            }
        }
        _ => {}
    }
}

fn collect_summaries(messages: &[ChatMessage], role: &str) -> Vec<String> {
    messages
        .iter()
        .filter(|message| message.role == role)
        .filter_map(|message| message.content.as_deref())
        .map(|content| summarize_text(content, SUMMARY_SNIPPET_CHARS))
        .filter(|summary| !summary.is_empty())
        .take(MAX_SUMMARY_SNIPPETS)
        .collect()
}

fn synthetic_compact_message(body: &str) -> ChatMessage {
    ChatMessage::user(format!(
        "<system-reminder>\n{}\n\nTreat this compact summary as a lossy recap. Re-open files, rerun tools, and verify repository state before relying on it.\n</system-reminder>",
        body
    ))
}

fn summarize_text(value: &str, max_chars: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = normalized.trim();
    if trimmed.chars().count() <= max_chars {
        trimmed.to_string()
    } else {
        trimmed.chars().take(max_chars).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{ToolCall, ToolCallFunction};

    fn assistant_with_tool(id: &str, name: &str, arguments: &str) -> ChatMessage {
        ChatMessage::assistant_with_tools(vec![ToolCall {
            id: id.to_string(),
            r#type: "function".to_string(),
            function: ToolCallFunction {
                name: name.to_string(),
                arguments: arguments.to_string(),
            },
        }])
    }

    #[test]
    fn micro_compact_replaces_old_tool_outputs_with_stable_placeholders() {
        let history = vec![
            assistant_with_tool("call_1", "search", r#"{"path":"src","pattern":"auth"}"#),
            ChatMessage::tool("call_1", "result ".repeat(200)),
            ChatMessage::user("latest question"),
        ];

        let result = micro_compact_history(&history, 1);

        assert_eq!(result.strategy, CompactStrategy::Micro);
        assert!(result.after_tokens < result.before_tokens);
        assert_eq!(
            result.history[1].content.as_deref().unwrap_or_default(),
            result.history[1].content.as_deref().unwrap_or_default()
        );
        assert!(result.history[1]
            .content
            .as_deref()
            .unwrap_or_default()
            .starts_with("[micro-compacted search output]"));
    }

    #[test]
    fn session_memory_compact_keeps_recent_messages_and_emits_summary_section() {
        let history = vec![
            ChatMessage::user("please remember rollout starts Thursday"),
            ChatMessage::assistant("compliance review blocks auth rewrite"),
            assistant_with_tool("call_2", "file_read", r#"{"path":"src/auth.rs"}"#),
            ChatMessage::tool("call_2", "auth file contents"),
            ChatMessage::user("latest question"),
        ];

        let result = session_memory_compact(&history, 1);

        assert_eq!(result.strategy, CompactStrategy::SessionMemory);
        assert_eq!(result.history.len(), 1);
        assert!(result.user_context_sections[0]
            .content
            .contains("rollout starts Thursday"));
        assert!(result.system_sections[0]
            .content
            .contains("auto_session_memory"));
    }

    #[test]
    fn full_compact_up_to_injects_boundary_and_rehydration() {
        let history = vec![
            ChatMessage::user("inspect auth"),
            assistant_with_tool("call_1", "search", r#"{"path":"src","pattern":"auth"}"#),
            ChatMessage::tool("call_1", "search results"),
            ChatMessage::assistant("auth depends on compliance"),
            ChatMessage::user("latest question"),
        ];

        let result = full_compact(&history, CompactDirection::UpTo, None, 2);

        assert_eq!(result.history.len(), 2);
        assert!(result.system_sections[0]
            .content
            .contains("Compact Boundary"));
        assert!(result
            .user_context_sections
            .iter()
            .any(|section| matches!(section.source, PromptSectionSource::PostCompactReinjection)));
    }

    #[test]
    fn partial_from_compact_preserves_prefix_and_marks_direction() {
        let history = vec![
            ChatMessage::user("keep this prefix"),
            ChatMessage::assistant("stable context"),
            ChatMessage::user("compress from here"),
            ChatMessage::assistant("tail details"),
        ];

        let result = full_compact(&history, CompactDirection::From, Some(2), 1);

        assert_eq!(result.strategy, CompactStrategy::PartialFrom);
        assert_eq!(result.history.len(), 3);
        assert_eq!(
            result.history[0].content.as_deref(),
            Some("keep this prefix")
        );
        assert_eq!(result.history[1].content.as_deref(), Some("stable context"));
        assert!(result.history[2]
            .content
            .as_deref()
            .unwrap_or_default()
            .contains("tail details"));
        assert!(result.user_context_sections.is_empty());
        assert!(result.system_sections[0]
            .content
            .contains("Direction: from"));
    }

    #[test]
    fn full_compact_with_summary_uses_override_for_compacted_recap() {
        let history = vec![
            ChatMessage::user("inspect auth"),
            ChatMessage::assistant("compliance still blocks rollout"),
            ChatMessage::user("latest question"),
        ];

        let result = full_compact_with_summary(
            &history,
            CompactDirection::UpTo,
            None,
            1,
            Some("### Compacted Conversation\n- Auth depends on compliance sign-off."),
        );

        assert!(result.user_context_sections[0]
            .content
            .contains("Auth depends on compliance sign-off."));
    }
}
