use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::Serialize;
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::api::{ChatMessage, ToolDefinition};

const DEFAULT_CONTEXT_WINDOW_TOKENS: usize = 32_000;
const DEFAULT_RECENT_MESSAGE_COUNT: usize = 8;
const MAX_PROJECT_CONTEXT_DOC_CHARS: usize = 4_000;
const MAX_PROJECT_MEMORY_INDEX_BYTES: usize = 25_000;
const MAX_PROJECT_MEMORY_INDEX_LINES: usize = 200;
const MAX_RELEVANT_PROJECT_MEMORY_CHARS: usize = 12_000;
const MAX_RELEVANT_PROJECT_MEMORY_FILES: usize = 5;
const MAX_RECENT_TOOLS: usize = 8;
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
    ProjectMemoryIndex,
    RelevantProjectMemory,
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
    pub surfaced_memory_paths: Vec<String>,
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
    pub memory_enabled: bool,
    pub auto_memory_directory: Option<PathBuf>,
    pub already_surfaced_memory_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectMemoryCandidate {
    pub path: String,
    pub name: Option<String>,
    pub description: String,
    pub memory_type: Option<String>,
    pub mtime_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectMemorySelectionQuery {
    pub query: String,
    pub memory_index_excerpt: Option<String>,
    pub candidates: Vec<ProjectMemoryCandidate>,
    pub recent_tools: Vec<String>,
    pub already_surfaced_memory_paths: Vec<String>,
}

#[async_trait]
pub trait ProjectMemorySelector: Send + Sync {
    async fn select(&self, query: ProjectMemorySelectionQuery) -> Result<Vec<String>>;
}

pub struct PromptBuilder;

impl PromptBuilder {
    pub fn build(request: PromptBuildRequest) -> Result<PromptAssembly> {
        let project_memory_resolution = resolve_project_memory_resolution_sync(&request)?;
        build_prompt_assembly(request, project_memory_resolution)
    }

    pub async fn build_with_selector(
        request: PromptBuildRequest,
        selector: Option<&dyn ProjectMemorySelector>,
    ) -> Result<PromptAssembly> {
        let project_memory_resolution =
            resolve_project_memory_resolution_async(&request, selector).await?;
        build_prompt_assembly(request, project_memory_resolution)
    }
}

#[derive(Debug, Clone)]
struct ProjectMemoryStore {
    index_excerpt: Option<String>,
    candidates: Vec<ProjectMemoryDocument>,
}

fn resolve_project_memory_resolution_sync(
    request: &PromptBuildRequest,
) -> Result<ProjectMemoryResolution> {
    let store = match load_project_memory_store(request)? {
        Some(store) => store,
        None => return Ok(ProjectMemoryResolution::default()),
    };
    let recent_tools = collect_recent_tools(&request.history);
    let selection_query = build_project_memory_selection_query(request, &store, &recent_tools);
    let selected_paths = select_relevant_project_memory_paths_heuristic(&selection_query);
    Ok(build_project_memory_resolution(store, selected_paths))
}

async fn resolve_project_memory_resolution_async(
    request: &PromptBuildRequest,
    selector: Option<&dyn ProjectMemorySelector>,
) -> Result<ProjectMemoryResolution> {
    let store = match load_project_memory_store(request)? {
        Some(store) => store,
        None => return Ok(ProjectMemoryResolution::default()),
    };
    let recent_tools = collect_recent_tools(&request.history);
    let selection_query = build_project_memory_selection_query(request, &store, &recent_tools);
    let selected_paths = if selection_query.candidates.is_empty() {
        Vec::new()
    } else if let Some(selector) = selector {
        match selector.select(selection_query.clone()).await {
            Ok(paths) => {
                normalize_selected_project_memory_paths(paths, &selection_query.candidates)
            }
            Err(_) => select_relevant_project_memory_paths_heuristic(&selection_query),
        }
    } else {
        select_relevant_project_memory_paths_heuristic(&selection_query)
    };
    Ok(build_project_memory_resolution(store, selected_paths))
}

fn build_project_memory_resolution(
    store: ProjectMemoryStore,
    selected_paths: Vec<String>,
) -> ProjectMemoryResolution {
    let ProjectMemoryStore {
        index_excerpt,
        candidates,
    } = store;
    let selected_path_set = selected_paths.iter().cloned().collect::<HashSet<_>>();
    let relevant_documents = candidates
        .into_iter()
        .filter(|document| selected_path_set.contains(&document.relative_path))
        .collect::<Vec<_>>();
    let bundle = ProjectMemoryBundle {
        index_excerpt,
        relevant_documents,
    };

    ProjectMemoryResolution {
        sections: build_project_memory_sections_from_bundle(&bundle),
        surfaced_memory_paths: selected_paths,
    }
}

fn load_project_memory_store(request: &PromptBuildRequest) -> Result<Option<ProjectMemoryStore>> {
    if !request.memory_enabled {
        return Ok(None);
    }

    let Some(memory_dir) = resolve_project_memory_directory(request) else {
        return Ok(None);
    };

    if !memory_dir.is_dir() {
        return Ok(None);
    }

    let index_excerpt = load_project_memory_index_excerpt(&memory_dir.join("MEMORY.md"))?;
    let candidates = load_project_memory_documents(&memory_dir)?;
    if index_excerpt.is_none() && candidates.is_empty() {
        return Ok(None);
    }

    Ok(Some(ProjectMemoryStore {
        index_excerpt,
        candidates,
    }))
}

fn resolve_project_memory_directory(request: &PromptBuildRequest) -> Option<PathBuf> {
    let candidates = [
        std::env::var_os("CLAUDE_COWORK_MEMORY_PATH_OVERRIDE").map(PathBuf::from),
        request.auto_memory_directory.clone(),
        default_project_memory_directory(request),
    ];

    candidates
        .into_iter()
        .flatten()
        .find_map(|candidate| validate_project_memory_directory(candidate))
}

fn validate_project_memory_directory(candidate: PathBuf) -> Option<PathBuf> {
    let as_string = candidate.to_string_lossy();
    if as_string.contains('\0') || as_string.starts_with("\\\\") {
        return None;
    }
    if !candidate.is_absolute() {
        return None;
    }
    if candidate.parent().is_none() {
        return None;
    }
    Some(candidate)
}

fn default_project_memory_directory(request: &PromptBuildRequest) -> Option<PathBuf> {
    let current_working_dir = request
        .current_working_dir
        .as_deref()
        .filter(|path| path.starts_with(&request.workspace_root))
        .unwrap_or(&request.workspace_root);
    let canonical_git_root = find_canonical_git_root(current_working_dir)?;
    let home = dirs::home_dir()?;
    Some(
        home.join(".claude")
            .join("projects")
            .join(sanitize_project_memory_slug(&canonical_git_root))
            .join("memory"),
    )
}

fn find_canonical_git_root(start: &Path) -> Option<PathBuf> {
    for directory in start.ancestors() {
        let dot_git = directory.join(".git");
        if dot_git.is_dir() {
            return Some(directory.to_path_buf());
        }

        if dot_git.is_file() {
            let content = std::fs::read_to_string(&dot_git).ok()?;
            let git_dir = parse_git_dir_redirect(directory, &content)?;
            if let Some(worktrees_dir) = git_dir.parent().filter(|parent| {
                parent
                    .file_name()
                    .map(|name| name == "worktrees")
                    .unwrap_or(false)
            }) {
                return worktrees_dir
                    .parent()
                    .and_then(Path::parent)
                    .map(Path::to_path_buf);
            }

            if git_dir
                .file_name()
                .map(|name| name == ".git")
                .unwrap_or(false)
            {
                return git_dir.parent().map(Path::to_path_buf);
            }

            return Some(directory.to_path_buf());
        }
    }

    None
}

fn parse_git_dir_redirect(base_dir: &Path, content: &str) -> Option<PathBuf> {
    let redirected = content
        .lines()
        .find_map(|line| line.trim().strip_prefix("gitdir:").map(str::trim))?;
    let candidate = PathBuf::from(redirected);
    Some(if candidate.is_absolute() {
        candidate
    } else {
        base_dir.join(candidate)
    })
}

fn sanitize_project_memory_slug(path: &Path) -> String {
    let display = path.to_string_lossy().replace('\\', "/");
    let basename = path
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".to_string());
    let slug = basename
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    let mut hasher = Sha256::new();
    hasher.update(display.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    format!(
        "{}-{}",
        if slug.is_empty() {
            "project"
        } else {
            slug.as_str()
        },
        &digest[..12]
    )
}

fn load_project_memory_index_excerpt(index_path: &Path) -> Result<Option<String>> {
    if !index_path.is_file() {
        return Ok(None);
    }

    let content = std::fs::read_to_string(index_path)?;
    let warning = "WARNING: MEMORY.md index was truncated to fit the prompt budget.";
    let allowed_bytes = MAX_PROJECT_MEMORY_INDEX_BYTES.saturating_sub(warning.len() + 4);
    let mut collected = Vec::new();
    let mut used_bytes = 0usize;
    let mut truncated = false;

    for (index, line) in content.lines().enumerate() {
        if index >= MAX_PROJECT_MEMORY_INDEX_LINES {
            truncated = true;
            break;
        }

        let line_bytes = line.len() + usize::from(!collected.is_empty());
        if used_bytes + line_bytes > allowed_bytes {
            truncated = true;
            break;
        }

        collected.push(line.to_string());
        used_bytes += line_bytes;
    }

    let mut excerpt = collected.join("\n").trim().to_string();
    if excerpt.is_empty() {
        return Ok(None);
    }

    if truncated {
        excerpt.push_str("\n\n");
        excerpt.push_str(warning);
    }

    Ok(Some(excerpt))
}

fn load_project_memory_documents(memory_dir: &Path) -> Result<Vec<ProjectMemoryDocument>> {
    let mut documents = WalkDir::new(memory_dir)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| {
            entry
                .path()
                .extension()
                .map(|value| value == "md")
                .unwrap_or(false)
        })
        .filter(|entry| entry.file_name() != "MEMORY.md")
        .filter(|entry| {
            !entry
                .path()
                .strip_prefix(memory_dir)
                .ok()
                .map(|relative| {
                    relative
                        .components()
                        .any(|component| component.as_os_str().eq("logs"))
                })
                .unwrap_or(false)
        })
        .filter_map(|entry| load_project_memory_document(memory_dir, entry.path()).ok())
        .collect::<Vec<_>>();

    documents.sort_by(|left, right| {
        right
            .mtime_ms
            .cmp(&left.mtime_ms)
            .then_with(|| left.relative_path.cmp(&right.relative_path))
    });

    Ok(documents)
}

fn load_project_memory_document(memory_dir: &Path, path: &Path) -> Result<ProjectMemoryDocument> {
    let raw = std::fs::read_to_string(path)?;
    let parsed = parse_project_memory_file(&raw);
    let metadata = std::fs::metadata(path)?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_millis() as u64)
        .unwrap_or_default();
    let relative_path = path
        .strip_prefix(memory_dir)
        .map_err(|error| anyhow!("failed to strip memory dir prefix: {}", error))?
        .to_string_lossy()
        .replace('\\', "/");

    let description = parsed
        .description
        .or_else(|| {
            parsed
                .body
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(|line| summarize_text(line, 220))
        })
        .unwrap_or_else(|| relative_path.clone());

    Ok(ProjectMemoryDocument {
        relative_path,
        name: parsed.name,
        description,
        memory_type: parsed.memory_type,
        body: summarize_text(&parsed.body, MAX_RELEVANT_PROJECT_MEMORY_CHARS),
        mtime_ms: modified,
    })
}

fn parse_project_memory_file(raw: &str) -> ParsedProjectMemoryFile {
    let mut lines = raw.lines();
    if !matches!(lines.next().map(str::trim), Some("---")) {
        return ParsedProjectMemoryFile {
            name: None,
            description: None,
            memory_type: None,
            body: raw.trim().to_string(),
        };
    }

    let mut frontmatter_lines = Vec::new();
    let mut body_lines = Vec::new();
    let mut reached_body = false;
    for line in raw.lines().skip(1) {
        if !reached_body && line.trim() == "---" {
            reached_body = true;
            continue;
        }

        if reached_body {
            body_lines.push(line);
        } else {
            frontmatter_lines.push(line);
        }
    }

    let mut name = None;
    let mut description = None;
    let mut memory_type = None;
    for line in frontmatter_lines {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let normalized_value = value.trim().trim_matches('"').trim_matches('\'');
        match key.trim() {
            "name" => name = Some(normalized_value.to_string()),
            "description" => description = Some(normalized_value.to_string()),
            "type" => memory_type = Some(normalized_value.to_string()),
            _ => {}
        }
    }

    ParsedProjectMemoryFile {
        name,
        description,
        memory_type,
        body: body_lines.join("\n").trim().to_string(),
    }
}

fn collect_recent_tools(history: &[ChatMessage]) -> Vec<String> {
    let mut recent_tools = Vec::new();
    let mut seen = HashSet::new();

    for message in history.iter().rev() {
        let Some(tool_calls) = message.tool_calls.as_ref() else {
            continue;
        };

        for tool_call in tool_calls.iter().rev() {
            let name = tool_call.function.name.trim();
            if name.is_empty() || !seen.insert(name.to_string()) {
                continue;
            }
            recent_tools.push(name.to_string());
            if recent_tools.len() >= MAX_RECENT_TOOLS {
                return recent_tools;
            }
        }
    }

    recent_tools
}

fn build_project_memory_selection_query(
    request: &PromptBuildRequest,
    store: &ProjectMemoryStore,
    recent_tools: &[String],
) -> ProjectMemorySelectionQuery {
    let already_surfaced = request
        .already_surfaced_memory_paths
        .iter()
        .cloned()
        .collect::<HashSet<_>>();

    let candidates = store
        .candidates
        .iter()
        .filter(|candidate| !already_surfaced.contains(&candidate.relative_path))
        .filter(|candidate| !is_recent_tool_reference(candidate, recent_tools))
        .map(|candidate| ProjectMemoryCandidate {
            path: candidate.relative_path.clone(),
            name: candidate.name.clone(),
            description: candidate.description.clone(),
            memory_type: candidate.memory_type.clone(),
            mtime_ms: candidate.mtime_ms,
        })
        .collect::<Vec<_>>();

    ProjectMemorySelectionQuery {
        query: latest_user_query(&request.history).unwrap_or_default(),
        memory_index_excerpt: store.index_excerpt.clone(),
        candidates,
        recent_tools: recent_tools.to_vec(),
        already_surfaced_memory_paths: request.already_surfaced_memory_paths.clone(),
    }
}

fn latest_user_query(history: &[ChatMessage]) -> Option<String> {
    history
        .iter()
        .rev()
        .find(|message| message.role == "user")
        .and_then(|message| message.content.clone())
}

fn is_recent_tool_reference(document: &ProjectMemoryDocument, recent_tools: &[String]) -> bool {
    matches!(document.memory_type.as_deref(), Some("reference"))
        && recent_tools.iter().any(|tool| {
            let normalized_tool = tool.to_ascii_lowercase();
            document
                .description
                .to_ascii_lowercase()
                .contains(&normalized_tool)
                || document
                    .relative_path
                    .to_ascii_lowercase()
                    .contains(&normalized_tool)
        })
}

fn normalize_selected_project_memory_paths(
    selected_paths: Vec<String>,
    candidates: &[ProjectMemoryCandidate],
) -> Vec<String> {
    let allowed = candidates
        .iter()
        .map(|candidate| candidate.path.clone())
        .collect::<HashSet<_>>();
    let mut deduped = Vec::new();
    let mut seen = HashSet::new();

    for path in selected_paths {
        if !allowed.contains(&path) || !seen.insert(path.clone()) {
            continue;
        }
        deduped.push(path);
        if deduped.len() >= MAX_RELEVANT_PROJECT_MEMORY_FILES {
            break;
        }
    }

    deduped
}

fn select_relevant_project_memory_paths_heuristic(
    query: &ProjectMemorySelectionQuery,
) -> Vec<String> {
    let terms = collect_query_terms(&format!(
        "{}\n{}",
        query.query,
        query.memory_index_excerpt.clone().unwrap_or_default()
    ));

    let mut ranked = query
        .candidates
        .iter()
        .map(|candidate| {
            let haystack = format!(
                "{} {} {} {}",
                candidate.path,
                candidate.name.clone().unwrap_or_default(),
                candidate.description,
                candidate.memory_type.clone().unwrap_or_default()
            )
            .to_ascii_lowercase();
            let score = terms
                .iter()
                .filter(|term| haystack.contains(term.as_str()))
                .count()
                + usize::from(candidate.memory_type.as_deref() == Some("project")) * 2
                + usize::from(candidate.memory_type.as_deref() == Some("feedback"));
            (candidate, score)
        })
        .filter(|(_, score)| *score > 0)
        .collect::<Vec<_>>();

    ranked.sort_by(
        |(left_candidate, left_score), (right_candidate, right_score)| {
            right_score
                .cmp(left_score)
                .then_with(|| right_candidate.mtime_ms.cmp(&left_candidate.mtime_ms))
                .then_with(|| left_candidate.path.cmp(&right_candidate.path))
        },
    );

    ranked
        .into_iter()
        .take(MAX_RELEVANT_PROJECT_MEMORY_FILES)
        .map(|(candidate, _)| candidate.path.clone())
        .collect()
}

fn collect_query_terms(value: &str) -> Vec<String> {
    let mut terms = value
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|term| term.len() >= 3)
        .map(|term| term.to_ascii_lowercase())
        .collect::<Vec<_>>();
    terms.sort();
    terms.dedup();
    terms
}

fn build_project_memory_sections_from_bundle(bundle: &ProjectMemoryBundle) -> Vec<PromptSection> {
    let mut sections = Vec::new();

    if let Some(index_excerpt) = bundle
        .index_excerpt
        .as_ref()
        .filter(|value| !value.is_empty())
    {
        sections.push(PromptSection {
            id: "project_memory_index".to_string(),
            role: PromptSectionRole::User,
            content: format!("### Project Memory Index\n{}", index_excerpt),
            cache_scope: PromptCacheScope::Org,
            is_dynamic: false,
            source: PromptSectionSource::ProjectMemoryIndex,
        });
    }

    for (index, document) in bundle.relevant_documents.iter().enumerate() {
        sections.push(PromptSection {
            id: format!("relevant_project_memory_{}", index),
            role: PromptSectionRole::User,
            content: format!(
                "### Relevant Project Memory ({})\n{}\n\nPath: {}",
                document
                    .name
                    .clone()
                    .unwrap_or_else(|| document.relative_path.clone()),
                document.body,
                document.relative_path
            ),
            cache_scope: PromptCacheScope::Org,
            is_dynamic: false,
            source: PromptSectionSource::RelevantProjectMemory,
        });
    }

    sections
}

#[derive(Debug, Clone, Default)]
struct ProjectMemoryResolution {
    sections: Vec<PromptSection>,
    surfaced_memory_paths: Vec<String>,
}

#[derive(Debug, Clone)]
struct ProjectMemoryDocument {
    relative_path: String,
    name: Option<String>,
    description: String,
    memory_type: Option<String>,
    body: String,
    mtime_ms: u64,
}

#[derive(Debug, Clone)]
struct ProjectMemoryBundle {
    index_excerpt: Option<String>,
    relevant_documents: Vec<ProjectMemoryDocument>,
}

#[derive(Debug, Clone)]
struct ParsedProjectMemoryFile {
    name: Option<String>,
    description: Option<String>,
    memory_type: Option<String>,
    body: String,
}

fn build_prompt_assembly(
    request: PromptBuildRequest,
    project_memory_resolution: ProjectMemoryResolution,
) -> Result<PromptAssembly> {
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
    let project_memory_sections = if memory_policy == MemoryPolicy::Use && request.memory_enabled {
        if project_memory_resolution.sections.is_empty() {
            build_project_memory_sections(&prompt_documents)
        } else {
            project_memory_resolution.sections.clone()
        }
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
        let trim_result =
            trim_history_to_budget(trimmed_history, history_budget, budget.recent_message_count);

        if trim_result.report.dropped_message_count == 0 {
            return Ok(PromptAssembly {
                system_sections,
                user_context_sections,
                history_messages: trim_result.history,
                dynamic_boundary_index,
                trim_report,
                surfaced_memory_paths: project_memory_resolution.surfaced_memory_paths,
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
        surfaced_memory_paths: project_memory_resolution.surfaced_memory_paths,
    })
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
