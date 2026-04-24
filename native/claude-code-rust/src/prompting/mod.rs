use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::api::{ChatMessage, ToolDefinition};

const DEFAULT_CONTEXT_WINDOW_TOKENS: usize = 32_000;
const DEFAULT_RECENT_MESSAGE_COUNT: usize = 8;
const MAX_PROJECT_CONTEXT_DOC_CHARS: usize = 4_000;
const MAX_COMPACTED_TOOL_MESSAGE_CHARS: usize = 640;
const MAX_COMPACTED_TEXT_MESSAGE_CHARS: usize = 960;

pub const SYSTEM_PROMPT_DYNAMIC_BOUNDARY: &str =
    "<!-- rust-agent:system-prompt:dynamic-boundary -->";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptSectionRole {
    System,
    User,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptCacheScope {
    None,
    Global,
    Org,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptSectionSource {
    AttributionHeader,
    BaseInstructions,
    ReliabilityRules,
    GlobalInstruction,
    WorkspaceInstruction,
    DirectoryInstruction,
    RuntimeContext,
    ContextTrimNotice,
    GlobalMemory,
    WorkspaceMemory,
    DirectoryMemory,
    SessionMemory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptSection {
    pub id: String,
    pub role: PromptSectionRole,
    pub content: String,
    pub cache_scope: PromptCacheScope,
    pub is_dynamic: bool,
    pub source: PromptSectionSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PromptBudget {
    pub total_input_tokens: usize,
    pub reserved_output_tokens: usize,
    pub recent_message_count: usize,
}

impl PromptBudget {
    pub fn default_for(max_response_tokens: usize) -> Self {
        Self {
            total_input_tokens: DEFAULT_CONTEXT_WINDOW_TOKENS,
            reserved_output_tokens: max_response_tokens.max(512),
            recent_message_count: DEFAULT_RECENT_MESSAGE_COUNT,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptTrimReport {
    pub dropped_message_count: usize,
    pub compacted_tool_message_count: usize,
    pub compacted_text_message_count: usize,
}

impl PromptTrimReport {
    fn merge(&mut self, other: Self) {
        self.dropped_message_count += other.dropped_message_count;
        self.compacted_tool_message_count += other.compacted_tool_message_count;
        self.compacted_text_message_count += other.compacted_text_message_count;
    }

    fn has_changes(&self) -> bool {
        self.dropped_message_count > 0
            || self.compacted_tool_message_count > 0
            || self.compacted_text_message_count > 0
    }
}

#[derive(Debug, Clone)]
pub struct PromptAssembly {
    pub system_sections: Vec<PromptSection>,
    pub user_context_sections: Vec<PromptSection>,
    pub history_messages: Vec<ChatMessage>,
    pub dynamic_boundary_index: usize,
    pub trim_report: PromptTrimReport,
}

impl PromptAssembly {
    pub fn system_prompt_parts(&self) -> Vec<String> {
        self.system_sections
            .iter()
            .map(|section| section.content.clone())
            .collect()
    }

    pub fn cache_aware_system_prompt_parts(&self) -> Vec<String> {
        let mut parts = Vec::with_capacity(self.system_sections.len() + 1);

        for (index, section) in self.system_sections.iter().enumerate() {
            if index == self.dynamic_boundary_index {
                parts.push(SYSTEM_PROMPT_DYNAMIC_BOUNDARY.to_string());
            }
            parts.push(section.content.clone());
        }

        if self.dynamic_boundary_index >= self.system_sections.len() {
            parts.push(SYSTEM_PROMPT_DYNAMIC_BOUNDARY.to_string());
        }

        parts
    }

    pub fn effective_system_prompt(&self) -> String {
        self.system_prompt_parts().join("\n\n")
    }

    pub fn split_system_sections(&self) -> (&[PromptSection], &[PromptSection]) {
        self.system_sections.split_at(self.dynamic_boundary_index)
    }

    pub fn split_system_prompt_parts(&self) -> (Vec<String>, Vec<String>) {
        let (prefix, suffix) = self.split_system_sections();
        (
            prefix
                .iter()
                .map(|section| section.content.clone())
                .collect(),
            suffix
                .iter()
                .map(|section| section.content.clone())
                .collect(),
        )
    }

    pub fn render(&self) -> RenderedPrompt {
        let system_prompt_parts = self.system_prompt_parts();
        let effective_system_prompt = system_prompt_parts.join("\n\n");
        let prepended_user_context =
            render_prepended_user_context_message(&self.user_context_sections);
        let mut messages = Vec::with_capacity(
            self.history_messages.len() + usize::from(prepended_user_context.is_some()) + 1,
        );
        messages.push(ChatMessage::system(effective_system_prompt.clone()));
        if let Some(message) = prepended_user_context.clone() {
            messages.push(message);
        }
        messages.extend(self.history_messages.clone());

        RenderedPrompt {
            system_prompt_parts,
            effective_system_prompt,
            prepended_user_context,
            messages,
            dynamic_boundary_index: self.dynamic_boundary_index,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RenderedPrompt {
    pub system_prompt_parts: Vec<String>,
    pub effective_system_prompt: String,
    pub prepended_user_context: Option<ChatMessage>,
    pub messages: Vec<ChatMessage>,
    pub dynamic_boundary_index: usize,
}

#[derive(Debug, Clone)]
pub struct PromptBuildRequest {
    pub base_system_prompt: String,
    pub history: Vec<ChatMessage>,
    pub workspace_root: PathBuf,
    pub current_working_dir: Option<PathBuf>,
    pub tool_definitions: Vec<ToolDefinition>,
    pub budget: PromptBudget,
    pub entrypoint: String,
    pub version_fingerprint: Option<String>,
    pub global_config_root: Option<PathBuf>,
}

pub struct PromptBuilder;

impl PromptBuilder {
    pub fn build(request: PromptBuildRequest) -> Result<PromptAssembly> {
        let budget = normalize_budget(request.budget);
        let current_working_dir = request
            .current_working_dir
            .clone()
            .filter(|path| path.starts_with(&request.workspace_root))
            .unwrap_or_else(|| request.workspace_root.clone());
        let memory_policy = resolve_memory_policy(&request.history);
        let prompt_documents = load_prompt_documents(
            &request.workspace_root,
            &current_working_dir,
            request.global_config_root.as_deref(),
        );
        let tool_definition_tokens = estimate_tool_definition_tokens(&request.tool_definitions);
        let (mut trimmed_history, mut trim_report) =
            compact_history_messages(request.history, budget.recent_message_count);
        let static_system_sections = build_static_system_sections(
            &request.base_system_prompt,
            &request.workspace_root,
            &current_working_dir,
            &request.entrypoint,
            request.version_fingerprint.as_deref(),
            &prompt_documents,
        );
        let dynamic_boundary_index = static_system_sections.len();
        let project_memory_sections = if memory_policy == MemoryPolicy::Use {
            build_project_memory_sections(&prompt_documents)
        } else {
            Vec::new()
        };
        let mut dropped_messages = Vec::new();
        let stabilization_passes = trimmed_history.len().saturating_add(2).max(2);

        for _ in 0..stabilization_passes {
            let system_sections = build_system_sections(
                &static_system_sections,
                &request.workspace_root,
                &current_working_dir,
                &trim_report,
            );
            let user_context_sections = build_user_context_sections(
                &project_memory_sections,
                &dropped_messages,
                &trim_report,
                trimmed_history.len(),
                memory_policy,
            );
            let history_budget = available_history_tokens(
                budget,
                estimate_sections_tokens(&system_sections),
                tool_definition_tokens,
                render_prepended_user_context_message(&user_context_sections)
                    .as_ref()
                    .map(estimate_message_tokens)
                    .unwrap_or_default(),
            );
            let trim_result = trim_history_to_budget(
                trimmed_history,
                history_budget,
                budget.recent_message_count,
            );

            if trim_result.report.dropped_message_count == 0 {
                return Ok(PromptAssembly {
                    system_sections,
                    user_context_sections,
                    history_messages: trim_result.history,
                    dynamic_boundary_index,
                    trim_report,
                });
            }

            trim_report.merge(trim_result.report);
            dropped_messages.extend(trim_result.dropped_messages);
            trimmed_history = trim_result.history;
        }

        let system_sections = build_system_sections(
            &static_system_sections,
            &request.workspace_root,
            &current_working_dir,
            &trim_report,
        );
        let user_context_sections = build_user_context_sections(
            &project_memory_sections,
            &dropped_messages,
            &trim_report,
            trimmed_history.len(),
            memory_policy,
        );

        Ok(PromptAssembly {
            system_sections,
            user_context_sections,
            history_messages: trimmed_history,
            dynamic_boundary_index,
            trim_report,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemoryPolicy {
    Use,
    Ignore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromptDocumentKind {
    Instruction,
    Memory,
}

#[derive(Debug, Clone)]
struct PromptDocument {
    kind: PromptDocumentKind,
    source: PromptSectionSource,
    cache_scope: PromptCacheScope,
    display_path: String,
    content: String,
}

#[derive(Debug, Clone)]
struct RegisteredPromptSection {
    order: usize,
    section: PromptSection,
}

#[derive(Debug, Default)]
struct PromptSectionRegistry {
    entries: Vec<RegisteredPromptSection>,
}

impl PromptSectionRegistry {
    fn register(&mut self, order: usize, section: PromptSection) {
        self.entries
            .push(RegisteredPromptSection { order, section });
    }

    fn into_sections(mut self) -> Vec<PromptSection> {
        self.entries.sort_by_key(|entry| entry.order);
        self.entries
            .into_iter()
            .map(|entry| entry.section)
            .collect()
    }
}

#[derive(Debug, Clone, Default)]
struct TrimHistoryResult {
    history: Vec<ChatMessage>,
    report: PromptTrimReport,
    dropped_messages: Vec<ChatMessage>,
}

#[derive(Debug, Clone)]
struct ContextUnit {
    messages: Vec<ChatMessage>,
    token_estimate: usize,
    protected: bool,
}

fn normalize_budget(budget: PromptBudget) -> PromptBudget {
    if budget.total_input_tokens == 0
        || budget.reserved_output_tokens == 0
        || budget.recent_message_count == 0
    {
        PromptBudget::default_for(budget.reserved_output_tokens.max(512))
    } else {
        budget
    }
}

fn build_static_system_sections(
    base_system_prompt: &str,
    workspace_root: &Path,
    current_working_dir: &Path,
    entrypoint: &str,
    version_fingerprint: Option<&str>,
    prompt_documents: &[PromptDocument],
) -> Vec<PromptSection> {
    let mut registry = PromptSectionRegistry::default();
    registry.register(
        0,
        PromptSection {
            id: "attribution_header".to_string(),
            role: PromptSectionRole::System,
            content: format_attribution_header(entrypoint, version_fingerprint),
            cache_scope: PromptCacheScope::None,
            is_dynamic: false,
            source: PromptSectionSource::AttributionHeader,
        },
    );

    if !base_system_prompt.trim().is_empty() {
        registry.register(
            10,
            PromptSection {
                id: "base_instructions".to_string(),
                role: PromptSectionRole::System,
                content: base_system_prompt.trim().to_string(),
                cache_scope: PromptCacheScope::Global,
                is_dynamic: false,
                source: PromptSectionSource::BaseInstructions,
            },
        );
    }

    registry.register(
        20,
        PromptSection {
            id: "reliability_rules".to_string(),
            role: PromptSectionRole::System,
            content: "## Context Reliability Rules\n- Treat recalled project context and session memory as hints, not ground truth.\n- Verify file paths, symbols, commands, and repository state against the current workspace before acting on them.\n- If recent user instructions conflict with older memory, follow the recent user instructions.".to_string(),
            cache_scope: PromptCacheScope::Global,
            is_dynamic: false,
            source: PromptSectionSource::ReliabilityRules,
        },
    );

    for (index, document) in prompt_documents
        .iter()
        .filter(|document| document.kind == PromptDocumentKind::Instruction)
        .enumerate()
    {
        registry.register(
            30 + index,
            PromptSection {
                id: format!("instruction_doc_{}", index),
                role: PromptSectionRole::System,
                content: format!(
                    "## Project Instruction File ({})\n{}",
                    document.display_path, document.content
                ),
                cache_scope: document.cache_scope,
                is_dynamic: false,
                source: document.source,
            },
        );
    }

    let _ = (workspace_root, current_working_dir);
    registry.into_sections()
}

fn build_system_sections(
    static_sections: &[PromptSection],
    workspace_root: &Path,
    current_working_dir: &Path,
    trim_report: &PromptTrimReport,
) -> Vec<PromptSection> {
    let mut sections = static_sections.to_vec();
    sections.push(PromptSection {
        id: "runtime_context".to_string(),
        role: PromptSectionRole::System,
        content: format!(
            "## Runtime Context\n- Workspace Root: {}\n- Current Working Directory: {}",
            workspace_root.display(),
            current_working_dir.display()
        ),
        cache_scope: PromptCacheScope::None,
        is_dynamic: true,
        source: PromptSectionSource::RuntimeContext,
    });

    if trim_report.has_changes() {
        sections.push(PromptSection {
            id: "context_trim_notice".to_string(),
            role: PromptSectionRole::System,
            content: format!(
                "## Context Trim Notice\n- Older messages omitted: {}\n- Older tool results compacted: {}\n- Older text messages compacted: {}",
                trim_report.dropped_message_count,
                trim_report.compacted_tool_message_count,
                trim_report.compacted_text_message_count,
            ),
            cache_scope: PromptCacheScope::None,
            is_dynamic: true,
            source: PromptSectionSource::ContextTrimNotice,
        });
    }

    sections
}

fn build_project_memory_sections(prompt_documents: &[PromptDocument]) -> Vec<PromptSection> {
    prompt_documents
        .iter()
        .filter(|document| document.kind == PromptDocumentKind::Memory)
        .enumerate()
        .map(|(index, document)| PromptSection {
            id: format!("memory_doc_{}", index),
            role: PromptSectionRole::User,
            content: format!(
                "### Project Memory ({})\n{}",
                document.display_path, document.content
            ),
            cache_scope: document.cache_scope,
            is_dynamic: false,
            source: document.source,
        })
        .collect()
}

fn build_user_context_sections(
    project_memory_sections: &[PromptSection],
    dropped_messages: &[ChatMessage],
    trim_report: &PromptTrimReport,
    preserved_message_count: usize,
    memory_policy: MemoryPolicy,
) -> Vec<PromptSection> {
    if memory_policy == MemoryPolicy::Ignore {
        return Vec::new();
    }

    let mut sections = project_memory_sections.to_vec();
    if let Some(section) =
        build_session_memory_section(dropped_messages, trim_report, preserved_message_count)
    {
        sections.push(section);
    }
    sections
}

fn build_session_memory_section(
    dropped_messages: &[ChatMessage],
    trim_report: &PromptTrimReport,
    preserved_message_count: usize,
) -> Option<PromptSection> {
    if dropped_messages.is_empty() {
        return None;
    }

    let user_snippets = dropped_messages
        .iter()
        .filter(|message| message.role == "user")
        .filter_map(|message| message.content.as_deref())
        .map(|content| summarize_text(content, 160))
        .filter(|content| !content.is_empty())
        .take(3)
        .collect::<Vec<_>>();

    let assistant_snippets = dropped_messages
        .iter()
        .filter(|message| message.role == "assistant")
        .filter_map(|message| message.content.as_deref())
        .map(|content| summarize_text(content, 160))
        .filter(|content| !content.is_empty())
        .take(3)
        .collect::<Vec<_>>();

    let tool_names = dropped_messages
        .iter()
        .filter_map(|message| message.tool_calls.as_ref())
        .flat_map(|tool_calls| {
            tool_calls
                .iter()
                .map(|tool_call| tool_call.function.name.clone())
        })
        .collect::<BTreeSet<_>>();

    let tool_result_count = dropped_messages
        .iter()
        .filter(|message| message.role == "tool")
        .count();

    let mut sections = vec![format!(
        "### Session Memory\nCompact Boundary\n- Type: auto_session_memory\n- Older messages omitted: {}\n- Older tool results compacted before trimming: {}\n- Older text messages compacted before trimming: {}\n- Preserved Segment after boundary: {} message(s)",
        trim_report.dropped_message_count,
        trim_report.compacted_tool_message_count,
        trim_report.compacted_text_message_count,
        preserved_message_count,
    )];

    if !user_snippets.is_empty() {
        sections.push(format!(
            "Earlier user requests:\n- {}",
            user_snippets.join("\n- ")
        ));
    }

    if !assistant_snippets.is_empty() {
        sections.push(format!(
            "Earlier assistant progress:\n- {}",
            assistant_snippets.join("\n- ")
        ));
    }

    if !tool_names.is_empty() || tool_result_count > 0 {
        let mut tool_section = String::new();
        if !tool_names.is_empty() {
            tool_section.push_str(&format!(
                "Earlier tools used:\n- {}",
                tool_names.into_iter().collect::<Vec<_>>().join("\n- ")
            ));
        }
        if tool_result_count > 0 {
            if !tool_section.is_empty() {
                tool_section.push('\n');
            }
            tool_section.push_str(&format!(
                "Earlier tool result messages: {}",
                tool_result_count
            ));
        }
        sections.push(tool_section);
    }

    sections.push(
        "Use this session memory as soft recall only. Re-check files, symbols, commands, and current repository state before relying on it.".to_string(),
    );

    Some(PromptSection {
        id: "session_memory".to_string(),
        role: PromptSectionRole::User,
        content: sections.join("\n\n"),
        cache_scope: PromptCacheScope::None,
        is_dynamic: true,
        source: PromptSectionSource::SessionMemory,
    })
}

fn render_prepended_user_context_message(
    user_context_sections: &[PromptSection],
) -> Option<ChatMessage> {
    if user_context_sections.is_empty() {
        return None;
    }

    let body = user_context_sections
        .iter()
        .map(|section| section.content.clone())
        .collect::<Vec<_>>()
        .join("\n\n");

    Some(ChatMessage::user(format!(
        "<system-reminder>\n{}\n\nBefore relying on user context:\n- Verify file paths, symbols, commands, and repository state against the current workspace.\n- If project memory conflicts with recent user instructions, follow the recent user instructions.\n</system-reminder>",
        body
    )))
}

fn load_prompt_documents(
    workspace_root: &Path,
    current_working_dir: &Path,
    global_config_root: Option<&Path>,
) -> Vec<PromptDocument> {
    let mut documents = Vec::new();

    if let Some(global_root) = resolve_global_config_root(global_config_root) {
        documents.extend(load_documents_for_directory(
            None,
            Some(&global_root),
            workspace_root,
            true,
        ));
    }

    for directory in collect_directory_chain(workspace_root, current_working_dir) {
        documents.extend(load_documents_for_directory(
            Some(&directory),
            None,
            workspace_root,
            false,
        ));
    }

    documents
}

fn resolve_global_config_root(global_config_root: Option<&Path>) -> Option<PathBuf> {
    global_config_root
        .map(Path::to_path_buf)
        .or_else(|| dirs::home_dir().map(|home| home.join(".claude")))
}

fn collect_directory_chain(workspace_root: &Path, current_working_dir: &Path) -> Vec<PathBuf> {
    let mut chain = Vec::new();
    let mut cursor = if current_working_dir.starts_with(workspace_root) {
        current_working_dir.to_path_buf()
    } else {
        workspace_root.to_path_buf()
    };

    loop {
        chain.push(cursor.clone());
        if cursor == workspace_root {
            break;
        }
        if !cursor.pop() {
            break;
        }
        if !cursor.starts_with(workspace_root) {
            break;
        }
    }

    chain.reverse();
    chain
}

fn load_documents_for_directory(
    directory: Option<&Path>,
    global_root: Option<&Path>,
    workspace_root: &Path,
    is_global: bool,
) -> Vec<PromptDocument> {
    let base_dir = if let Some(root) = global_root {
        root.to_path_buf()
    } else if let Some(directory) = directory {
        directory.to_path_buf()
    } else {
        return Vec::new();
    };
    let is_workspace_root = !is_global && directory == Some(workspace_root);
    let candidates = if is_global {
        vec![
            (PromptDocumentKind::Instruction, base_dir.join("CLAUDE.md")),
            (PromptDocumentKind::Memory, base_dir.join("MEMORY.md")),
        ]
    } else {
        vec![
            (PromptDocumentKind::Instruction, base_dir.join("CLAUDE.md")),
            (PromptDocumentKind::Instruction, base_dir.join("AGENTS.md")),
            (PromptDocumentKind::Memory, base_dir.join("MEMORY.md")),
            (
                PromptDocumentKind::Instruction,
                base_dir.join(".claude").join("CLAUDE.md"),
            ),
            (
                PromptDocumentKind::Instruction,
                base_dir.join(".claude").join("AGENTS.md"),
            ),
            (
                PromptDocumentKind::Memory,
                base_dir.join(".claude").join("MEMORY.md"),
            ),
        ]
    };

    candidates
        .into_iter()
        .filter_map(|(kind, path)| {
            if !path.is_file() {
                return None;
            }

            let raw = std::fs::read_to_string(&path).ok()?;
            let summarized = summarize_text(&raw, MAX_PROJECT_CONTEXT_DOC_CHARS);
            if summarized.is_empty() {
                return None;
            }

            let source = match (kind, is_global, is_workspace_root) {
                (PromptDocumentKind::Instruction, true, _) => {
                    PromptSectionSource::GlobalInstruction
                }
                (PromptDocumentKind::Instruction, false, true) => {
                    PromptSectionSource::WorkspaceInstruction
                }
                (PromptDocumentKind::Instruction, false, false) => {
                    PromptSectionSource::DirectoryInstruction
                }
                (PromptDocumentKind::Memory, true, _) => PromptSectionSource::GlobalMemory,
                (PromptDocumentKind::Memory, false, true) => PromptSectionSource::WorkspaceMemory,
                (PromptDocumentKind::Memory, false, false) => PromptSectionSource::DirectoryMemory,
            };

            Some(PromptDocument {
                kind,
                source,
                cache_scope: if is_global {
                    PromptCacheScope::Global
                } else {
                    PromptCacheScope::Org
                },
                display_path: render_display_path(&path, workspace_root, is_global),
                content: summarized,
            })
        })
        .collect()
}

fn render_display_path(path: &Path, workspace_root: &Path, is_global: bool) -> String {
    if is_global {
        let filename = path
            .file_name()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        return format!("~/.claude/{}", filename);
    }

    path.strip_prefix(workspace_root)
        .ok()
        .map(|value| value.to_string_lossy().replace('\\', "/"))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| path.display().to_string())
}

fn format_attribution_header(entrypoint: &str, version_fingerprint: Option<&str>) -> String {
    let version = match version_fingerprint {
        Some(fingerprint) if !fingerprint.trim().is_empty() => {
            format!("{}#{}", env!("CARGO_PKG_VERSION"), fingerprint.trim())
        }
        _ => env!("CARGO_PKG_VERSION").to_string(),
    };

    format!(
        "cc_version: claude_code_rs/{}\ncc_entrypoint: {}\ncch=00000",
        version, entrypoint
    )
}

fn available_history_tokens(
    budget: PromptBudget,
    system_prompt_tokens: usize,
    tool_definition_tokens: usize,
    extra_context_tokens: usize,
) -> usize {
    budget
        .total_input_tokens
        .saturating_sub(budget.reserved_output_tokens)
        .saturating_sub(system_prompt_tokens)
        .saturating_sub(tool_definition_tokens)
        .saturating_sub(extra_context_tokens)
        .max(96)
}

fn compact_history_messages(
    history: Vec<ChatMessage>,
    recent_message_count: usize,
) -> (Vec<ChatMessage>, PromptTrimReport) {
    let mut report = PromptTrimReport::default();
    let recent_start = history.len().saturating_sub(recent_message_count);

    let compacted = history
        .into_iter()
        .enumerate()
        .map(|(index, mut message)| {
            if index >= recent_start {
                return message;
            }

            if let Some(content) = message.content.clone() {
                if message.role == "tool" {
                    let compacted = compact_tool_message(&content);
                    if compacted != content {
                        message.content = Some(compacted);
                        report.compacted_tool_message_count += 1;
                    }
                } else {
                    let compacted = compact_text_message(&content);
                    if compacted != content {
                        message.content = Some(compacted);
                        report.compacted_text_message_count += 1;
                    }
                }
            }

            message
        })
        .collect();

    (compacted, report)
}

fn trim_history_to_budget(
    history: Vec<ChatMessage>,
    history_budget_tokens: usize,
    recent_message_count: usize,
) -> TrimHistoryResult {
    let mut units = group_history_into_units(&history, recent_message_count);
    let mut result = TrimHistoryResult::default();
    let mut total_tokens = units.iter().map(|unit| unit.token_estimate).sum::<usize>();

    if total_tokens <= history_budget_tokens {
        result.history = flatten_context_units(units);
        return result;
    }

    for index in 0..units.len() {
        if total_tokens <= history_budget_tokens {
            break;
        }

        if units[index].protected {
            continue;
        }

        total_tokens = total_tokens.saturating_sub(units[index].token_estimate);
        result.report.dropped_message_count += units[index].messages.len();
        result
            .dropped_messages
            .extend(units[index].messages.drain(..));
        units[index].token_estimate = 0;
    }

    if total_tokens > history_budget_tokens {
        for index in 0..units.len().saturating_sub(1) {
            if total_tokens <= history_budget_tokens {
                break;
            }

            if units[index].messages.is_empty() {
                continue;
            }

            total_tokens = total_tokens.saturating_sub(units[index].token_estimate);
            result.report.dropped_message_count += units[index].messages.len();
            result
                .dropped_messages
                .extend(units[index].messages.drain(..));
            units[index].token_estimate = 0;
        }
    }

    result.history = flatten_context_units(units);
    result
}

fn group_history_into_units(
    history: &[ChatMessage],
    recent_message_count: usize,
) -> Vec<ContextUnit> {
    let recent_start = history.len().saturating_sub(recent_message_count);
    let mut units = Vec::new();
    let mut index = 0;

    while index < history.len() {
        let mut messages = vec![history[index].clone()];
        let mut protected = index >= recent_start;

        if history[index].role == "assistant"
            && history[index]
                .tool_calls
                .as_ref()
                .map(|tool_calls| !tool_calls.is_empty())
                .unwrap_or(false)
        {
            let mut cursor = index + 1;
            while cursor < history.len() && history[cursor].role == "tool" {
                protected |= cursor >= recent_start;
                messages.push(history[cursor].clone());
                cursor += 1;
            }
            index = cursor;
        } else {
            index += 1;
        }

        let token_estimate = messages.iter().map(estimate_message_tokens).sum();
        units.push(ContextUnit {
            messages,
            token_estimate,
            protected,
        });
    }

    units
}

fn flatten_context_units(units: Vec<ContextUnit>) -> Vec<ChatMessage> {
    units
        .into_iter()
        .flat_map(|unit| unit.messages.into_iter())
        .collect()
}

fn estimate_sections_tokens(sections: &[PromptSection]) -> usize {
    sections
        .iter()
        .map(|section| estimate_text_tokens(&section.content))
        .sum()
}

fn estimate_tool_definition_tokens(tool_definitions: &[ToolDefinition]) -> usize {
    tool_definitions
        .iter()
        .map(|tool_definition| {
            estimate_text_tokens(&serde_json::to_string(tool_definition).unwrap_or_default())
        })
        .sum()
}

fn estimate_message_tokens(message: &ChatMessage) -> usize {
    let mut total = 6;

    if let Some(content) = &message.content {
        total += estimate_text_tokens(content);
    }

    if let Some(reasoning_content) = &message.reasoning_content {
        total += estimate_text_tokens(reasoning_content);
    }

    if let Some(tool_calls) = &message.tool_calls {
        total += estimate_text_tokens(&serde_json::to_string(tool_calls).unwrap_or_default());
    }

    if let Some(tool_call_id) = &message.tool_call_id {
        total += estimate_text_tokens(tool_call_id);
    }

    total
}

fn estimate_text_tokens(value: &str) -> usize {
    let char_count = value.chars().count();
    (char_count / 4).max(1) + 1
}

fn resolve_memory_policy(history: &[ChatMessage]) -> MemoryPolicy {
    for content in history
        .iter()
        .rev()
        .filter(|message| message.role == "user")
        .filter_map(|message| message.content.as_deref())
    {
        let normalized = content.to_ascii_lowercase();
        if normalized.contains("ignore memory")
            || normalized.contains("don't use memory")
            || normalized.contains("do not use memory")
            || normalized.contains("without memory")
            || content.contains("忽略记忆")
            || content.contains("不要用记忆")
            || content.contains("不要使用记忆")
            || content.contains("别用记忆")
        {
            return MemoryPolicy::Ignore;
        }

        if normalized.contains("use memory")
            || normalized.contains("you can use memory")
            || normalized.contains("feel free to use memory")
            || content.contains("使用记忆")
            || content.contains("可以用记忆")
        {
            return MemoryPolicy::Use;
        }
    }

    MemoryPolicy::Use
}

fn compact_tool_message(content: &str) -> String {
    if content.chars().count() <= MAX_COMPACTED_TOOL_MESSAGE_CHARS {
        return content.to_string();
    }

    let summarized = summarize_text(content, MAX_COMPACTED_TOOL_MESSAGE_CHARS);
    format!("[compacted tool result] {}", summarized)
}

fn compact_text_message(content: &str) -> String {
    if content.chars().count() <= MAX_COMPACTED_TEXT_MESSAGE_CHARS {
        return content.to_string();
    }

    let summarized = summarize_text(content, MAX_COMPACTED_TEXT_MESSAGE_CHARS);
    format!("[compacted message] {}", summarized)
}

fn summarize_text(value: &str, max_chars: usize) -> String {
    let normalized = value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string();

    if normalized.chars().count() <= max_chars {
        normalized
    } else {
        normalized.chars().take(max_chars).collect()
    }
}
