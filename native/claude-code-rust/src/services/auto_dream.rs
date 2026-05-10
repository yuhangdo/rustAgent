//! AutoDream service.
//!
//! AutoDream is a session-end memory organizer. It does not extract new
//! memories from the active turn; it periodically consolidates the project
//! memory directory so future prompt assembly can load a compact MEMORY.md
//! index plus focused topic files.

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Duration, Utc};
use filetime::{set_file_mtime, FileTime};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;
use tokio::sync::RwLock;
use walkdir::WalkDir;

use crate::config::Settings;
use crate::state::AppState;

const DEFAULT_MIN_HOURS: i64 = 24;
const DEFAULT_MIN_SESSIONS: usize = 5;
const DEFAULT_LOCK_TTL_HOURS: i64 = 1;
const DEFAULT_SESSION_SCAN_INTERVAL_MS: i64 = 10 * 60 * 1000;
const MAX_INDEX_LINES: usize = 200;
const MAX_INDEX_BYTES: usize = 25 * 1024;
const MAX_INDEX_LINE_CHARS: usize = 150;
const MAX_SOURCE_BYTES: usize = 4 * 1024;
const MAX_SOURCES_PER_KIND: usize = 40;

fn default_min_hours() -> i64 {
    DEFAULT_MIN_HOURS
}

fn default_min_sessions() -> usize {
    DEFAULT_MIN_SESSIONS
}

fn default_enabled() -> bool {
    true
}

fn default_lock_ttl_hours() -> i64 {
    DEFAULT_LOCK_TTL_HOURS
}

fn default_session_scan_interval_ms() -> i64 {
    DEFAULT_SESSION_SCAN_INTERVAL_MS
}

/// AutoDream configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoDreamConfig {
    #[serde(default = "default_min_hours")]
    pub min_hours: i64,
    #[serde(default = "default_min_sessions")]
    pub min_sessions: usize,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_lock_ttl_hours")]
    pub lock_ttl_hours: i64,
    #[serde(default = "default_session_scan_interval_ms")]
    pub session_scan_interval_ms: i64,
    #[serde(default)]
    pub memory_dir: Option<PathBuf>,
    #[serde(default)]
    pub sessions_dir: Option<PathBuf>,
    #[serde(default)]
    pub state_path: Option<PathBuf>,
    #[serde(default)]
    pub workspace_root: Option<PathBuf>,
    #[serde(default)]
    pub current_working_dir: Option<PathBuf>,
}

impl Default for AutoDreamConfig {
    fn default() -> Self {
        Self {
            min_hours: DEFAULT_MIN_HOURS,
            min_sessions: DEFAULT_MIN_SESSIONS,
            enabled: true,
            lock_ttl_hours: DEFAULT_LOCK_TTL_HOURS,
            session_scan_interval_ms: DEFAULT_SESSION_SCAN_INTERVAL_MS,
            memory_dir: None,
            sessions_dir: None,
            state_path: None,
            workspace_root: None,
            current_working_dir: None,
        }
    }
}

/// Persisted consolidation state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidationState {
    pub last_consolidated_at: DateTime<Utc>,
    pub session_count: usize,
    pub is_consolidating: bool,
    pub last_session_scan: DateTime<Utc>,
    #[serde(default)]
    pub last_skip_reason: Option<String>,
    #[serde(default)]
    pub last_report: Option<AutoDreamRunReport>,
}

impl ConsolidationState {
    fn default_at(now: DateTime<Utc>, min_hours: i64) -> Self {
        Self {
            last_consolidated_at: now - Duration::hours(min_hours.max(0) + 1),
            session_count: 0,
            is_consolidating: false,
            last_session_scan: now - Duration::milliseconds(DEFAULT_SESSION_SCAN_INTERVAL_MS + 1),
            last_skip_reason: None,
            last_report: None,
        }
    }
}

impl Default for ConsolidationState {
    fn default() -> Self {
        Self::default_at(Utc::now(), DEFAULT_MIN_HOURS)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoDreamPaths {
    pub memory_dir: PathBuf,
    pub sessions_dir: PathBuf,
    pub state_path: PathBuf,
    pub lock_path: PathBuf,
    pub prompt_path: PathBuf,
    pub report_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoDreamRunReport {
    pub manual: bool,
    pub run_at: DateTime<Utc>,
    pub memory_dir: PathBuf,
    pub prompt_path: PathBuf,
    pub report_path: PathBuf,
    pub session_count: usize,
    pub topic_files_seen: usize,
    pub log_files_seen: usize,
    pub session_files_seen: usize,
    pub index_lines: usize,
    pub index_bytes: usize,
}

/// AutoDream service.
pub struct AutoDreamService {
    state: Arc<RwLock<AppState>>,
    config: AutoDreamConfig,
    consolidation_state: Arc<RwLock<ConsolidationState>>,
}

impl AutoDreamService {
    pub fn new(state: Arc<RwLock<AppState>>, config: Option<AutoDreamConfig>) -> Self {
        Self {
            state,
            config: config.unwrap_or_default(),
            consolidation_state: Arc::new(RwLock::new(ConsolidationState::default())),
        }
    }

    pub fn for_query_context(
        mut settings: Settings,
        workspace_root: PathBuf,
        sessions_dir: PathBuf,
    ) -> Self {
        settings.working_dir = workspace_root.clone();
        let min_hours = settings.memory.consolidation_interval as i64;
        let enabled = settings.memory.enabled;
        let state = Arc::new(RwLock::new(AppState::new(settings)));
        Self::new(
            state,
            Some(AutoDreamConfig {
                min_hours,
                min_sessions: DEFAULT_MIN_SESSIONS,
                enabled,
                sessions_dir: Some(sessions_dir),
                workspace_root: Some(workspace_root.clone()),
                current_working_dir: Some(workspace_root),
                ..AutoDreamConfig::default()
            }),
        )
    }

    pub fn with_config(mut self, config: AutoDreamConfig) -> Self {
        self.config = config;
        self
    }

    pub async fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    pub async fn check_and_run(&self) -> Result<bool> {
        self.check_and_run_at(Utc::now()).await
    }

    pub async fn check_and_run_at(&self, now: DateTime<Utc>) -> Result<bool> {
        if !self.config.enabled {
            self.remember_skip("disabled").await;
            return Ok(false);
        }

        let paths = self.resolve_paths().await?;
        let mut state = self.load_state_at(&paths, now).await?;

        if state.is_consolidating {
            self.persist_skip(&paths, &mut state, "busy").await?;
            return Ok(false);
        }

        let hours_since = (now - state.last_consolidated_at).num_hours();
        if hours_since < self.config.min_hours {
            self.persist_skip(&paths, &mut state, "time").await?;
            return Ok(false);
        }

        let scan_interval = Duration::milliseconds(self.config.session_scan_interval_ms.max(0));
        if self.config.session_scan_interval_ms > 0 && now - state.last_session_scan < scan_interval
        {
            self.persist_skip(&paths, &mut state, "scan_interval")
                .await?;
            return Ok(false);
        }

        state.last_session_scan = now;
        let session_count = self.count_recent_sessions(&paths, state.last_consolidated_at)?;
        state.session_count = session_count;
        if session_count < self.config.min_sessions {
            self.persist_skip(&paths, &mut state, "sessions").await?;
            return Ok(false);
        }

        let Some(lock) = self.try_acquire_lock(&paths, now)? else {
            self.persist_skip(&paths, &mut state, "locked").await?;
            return Ok(false);
        };

        state.is_consolidating = true;
        state.last_skip_reason = None;
        self.persist_state(&paths, &state).await?;

        match self
            .run_dream_with_lock(&paths, lock, now, false, session_count)
            .await
        {
            Ok(report) => {
                state.is_consolidating = false;
                state.last_consolidated_at = now;
                state.session_count = session_count;
                state.last_session_scan = now;
                state.last_skip_reason = None;
                state.last_report = Some(report);
                self.persist_state(&paths, &state).await?;
                Ok(true)
            }
            Err(error) => {
                state.is_consolidating = false;
                state.last_skip_reason = Some(format!("error: {}", error));
                self.persist_state(&paths, &state).await?;
                Err(error)
            }
        }
    }

    pub async fn force_consolidation(&self) -> Result<()> {
        self.force_consolidation_at(Utc::now()).await.map(|_| ())
    }

    pub async fn force_consolidation_at(&self, now: DateTime<Utc>) -> Result<AutoDreamRunReport> {
        let paths = self.resolve_paths().await?;
        let mut state = self.load_state_at(&paths, now).await?;
        let session_count = self.count_recent_sessions(&paths, state.last_consolidated_at)?;
        let Some(lock) = self.try_acquire_lock(&paths, now)? else {
            state.session_count = session_count;
            self.persist_skip(&paths, &mut state, "locked").await?;
            return Err(anyhow!("AutoDream consolidation is already locked"));
        };

        state.is_consolidating = true;
        state.last_skip_reason = None;
        self.persist_state(&paths, &state).await?;

        match self
            .run_dream_with_lock(&paths, lock, now, true, session_count)
            .await
        {
            Ok(report) => {
                state.is_consolidating = false;
                state.last_consolidated_at = now;
                state.session_count = session_count;
                state.last_session_scan = now;
                state.last_skip_reason = None;
                state.last_report = Some(report.clone());
                self.persist_state(&paths, &state).await?;
                Ok(report)
            }
            Err(error) => {
                state.is_consolidating = false;
                state.last_skip_reason = Some(format!("error: {}", error));
                self.persist_state(&paths, &state).await?;
                Err(error)
            }
        }
    }

    pub async fn get_status(&self) -> AutoDreamStatus {
        let now = Utc::now();
        if !self.config.enabled {
            let state = self.consolidation_state.read().await.clone();
            return AutoDreamStatus {
                enabled: false,
                is_consolidating: state.is_consolidating,
                last_consolidation: state.last_consolidated_at,
                hours_since_last: (now - state.last_consolidated_at).num_hours(),
                sessions_accumulated: state.session_count,
                next_consolidation_in: 0,
                last_skip_reason: state.last_skip_reason,
                memory_dir: self.config.memory_dir.clone(),
                sessions_dir: self.config.sessions_dir.clone(),
                lock_present: false,
                last_report: state.last_report,
            };
        }

        let resolved = self.resolve_paths().await.ok();
        let state = if let Some(paths) = resolved.as_ref() {
            match self.load_state_at(paths, now).await {
                Ok(state) => state,
                Err(_) => self.consolidation_state.read().await.clone(),
            }
        } else {
            self.consolidation_state.read().await.clone()
        };
        let hours_since = (now - state.last_consolidated_at).num_hours();
        let lock_present = resolved
            .as_ref()
            .map(|paths| paths.lock_path.exists())
            .unwrap_or(false);

        AutoDreamStatus {
            enabled: self.config.enabled,
            is_consolidating: state.is_consolidating,
            last_consolidation: state.last_consolidated_at,
            hours_since_last: hours_since,
            sessions_accumulated: state.session_count,
            next_consolidation_in: (self.config.min_hours - hours_since).max(0),
            last_skip_reason: state.last_skip_reason,
            memory_dir: resolved.as_ref().map(|paths| paths.memory_dir.clone()),
            sessions_dir: resolved.as_ref().map(|paths| paths.sessions_dir.clone()),
            lock_present,
            last_report: state.last_report,
        }
    }

    async fn resolve_paths(&self) -> Result<AutoDreamPaths> {
        let settings = self.state.read().await.settings.clone();
        let memory_dir = self
            .config
            .memory_dir
            .clone()
            .or_else(|| std::env::var_os("CLAUDE_COWORK_MEMORY_PATH_OVERRIDE").map(PathBuf::from))
            .or(settings.memory.auto_memory_directory.clone())
            .or_else(|| {
                default_project_memory_directory(
                    self.config
                        .current_working_dir
                        .as_deref()
                        .unwrap_or(&settings.working_dir),
                    self.config
                        .workspace_root
                        .as_deref()
                        .unwrap_or(&settings.working_dir),
                )
            })
            .map(absolutize)
            .ok_or_else(|| anyhow!("unable to resolve AutoDream memory directory"))?;

        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let sessions_dir = self
            .config
            .sessions_dir
            .clone()
            .unwrap_or_else(|| {
                home.join(".claude-code")
                    .join("query-engine")
                    .join("sessions")
            })
            .pipe(absolutize);
        let state_path = self
            .config
            .state_path
            .clone()
            .unwrap_or_else(|| memory_dir.join(".autodream_state.json"))
            .pipe(absolutize);

        Ok(AutoDreamPaths {
            lock_path: memory_dir.join(".consolidate-lock"),
            prompt_path: memory_dir.join(".last-dream-prompt.md"),
            report_path: memory_dir.join(".last-dream-report.json"),
            memory_dir,
            sessions_dir,
            state_path,
        })
    }

    async fn load_state_at(
        &self,
        paths: &AutoDreamPaths,
        now: DateTime<Utc>,
    ) -> Result<ConsolidationState> {
        if paths.state_path.is_file() {
            let content = fs::read_to_string(&paths.state_path)?;
            if let Ok(state) = serde_json::from_str::<ConsolidationState>(&content) {
                *self.consolidation_state.write().await = state.clone();
                return Ok(state);
            }
        }

        let mut state = ConsolidationState::default_at(now, self.config.min_hours);
        if let Ok(metadata) = fs::metadata(&paths.lock_path) {
            if let Ok(modified) = metadata.modified() {
                if let Ok(content) = fs::read_to_string(&paths.lock_path) {
                    if !content.trim().starts_with("pid:") {
                        state.last_consolidated_at = DateTime::<Utc>::from(modified);
                    }
                }
            }
        }

        *self.consolidation_state.write().await = state.clone();
        Ok(state)
    }

    async fn persist_state(
        &self,
        paths: &AutoDreamPaths,
        state: &ConsolidationState,
    ) -> Result<()> {
        if let Some(parent) = paths.state_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&paths.state_path, serde_json::to_string_pretty(state)?)?;
        *self.consolidation_state.write().await = state.clone();
        Ok(())
    }

    async fn remember_skip(&self, reason: &str) {
        let mut state = self.consolidation_state.write().await;
        state.is_consolidating = false;
        state.last_skip_reason = Some(reason.to_string());
    }

    async fn persist_skip(
        &self,
        paths: &AutoDreamPaths,
        state: &mut ConsolidationState,
        reason: &str,
    ) -> Result<()> {
        state.is_consolidating = false;
        state.last_skip_reason = Some(reason.to_string());
        self.persist_state(paths, state).await
    }

    fn count_recent_sessions(&self, paths: &AutoDreamPaths, since: DateTime<Utc>) -> Result<usize> {
        if !paths.sessions_dir.exists() {
            return Ok(0);
        }

        let mut sessions = BTreeSet::new();
        for entry in WalkDir::new(&paths.sessions_dir)
            .max_depth(3)
            .into_iter()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().is_file())
        {
            let path = entry.path();
            let is_query_transcript = path
                .file_name()
                .map(|name| name == "transcript.jsonl")
                .unwrap_or(false);
            let is_legacy_session = path
                .extension()
                .map(|extension| extension == "json")
                .unwrap_or(false)
                && path.parent() == Some(paths.sessions_dir.as_path());
            if !is_query_transcript && !is_legacy_session {
                continue;
            }
            let modified = entry.metadata()?.modified()?;
            let modified = DateTime::<Utc>::from(modified);
            if modified > since {
                let session_key = if is_query_transcript {
                    path.parent().unwrap_or(path).to_path_buf()
                } else {
                    path.to_path_buf()
                };
                sessions.insert(session_key);
            }
        }

        Ok(sessions.len())
    }

    fn try_acquire_lock(
        &self,
        paths: &AutoDreamPaths,
        now: DateTime<Utc>,
    ) -> Result<Option<AutoDreamLock>> {
        fs::create_dir_all(&paths.memory_dir)?;

        let previous = LockSnapshot::read(&paths.lock_path)?;
        if previous.is_active(now, self.config.lock_ttl_hours) {
            return Ok(None);
        }

        let holder = format!("pid:{}", process::id());
        fs::write(&paths.lock_path, &holder)?;
        set_file_mtime(&paths.lock_path, file_time_from_datetime(now))?;

        let verified = fs::read_to_string(&paths.lock_path).unwrap_or_default();
        if verified.trim() != holder {
            return Ok(None);
        }

        Ok(Some(AutoDreamLock {
            path: paths.lock_path.clone(),
            previous,
        }))
    }

    async fn run_dream_with_lock(
        &self,
        paths: &AutoDreamPaths,
        lock: AutoDreamLock,
        now: DateTime<Utc>,
        manual: bool,
        session_count: usize,
    ) -> Result<AutoDreamRunReport> {
        match self.run_dream(paths, now, manual, session_count).await {
            Ok(report) => {
                lock.release_success(now)?;
                Ok(report)
            }
            Err(error) => {
                let rollback_error = lock.release_failure().err();
                if let Some(rollback_error) = rollback_error {
                    Err(error).context(format!(
                        "AutoDream failed and lock rollback also failed: {}",
                        rollback_error
                    ))
                } else {
                    Err(error)
                }
            }
        }
    }

    async fn run_dream(
        &self,
        paths: &AutoDreamPaths,
        now: DateTime<Utc>,
        manual: bool,
        session_count: usize,
    ) -> Result<AutoDreamRunReport> {
        fs::create_dir_all(&paths.memory_dir)?;
        let index_path = paths.memory_dir.join("MEMORY.md");
        if index_path.exists() && !index_path.is_file() {
            return Err(anyhow!(
                "{} must be a file for AutoDream consolidation",
                index_path.display()
            ));
        }

        let sources = gather_sources(paths)?;
        let prompt = build_dream_prompt(paths, &sources, now);
        fs::write(&paths.prompt_path, prompt)?;

        let index = build_pruned_index(paths, &sources, now)?;
        fs::write(&index_path, index.as_bytes())?;

        let report = AutoDreamRunReport {
            manual,
            run_at: now,
            memory_dir: paths.memory_dir.clone(),
            prompt_path: paths.prompt_path.clone(),
            report_path: paths.report_path.clone(),
            session_count,
            topic_files_seen: sources.count_kind(SourceKind::Topic),
            log_files_seen: sources.count_kind(SourceKind::Log),
            session_files_seen: sources.count_kind(SourceKind::Session),
            index_lines: index.lines().count(),
            index_bytes: index.len(),
        };

        fs::write(&paths.report_path, serde_json::to_string_pretty(&report)?)?;
        Ok(report)
    }
}

#[derive(Debug, Clone)]
struct LockSnapshot {
    content: Option<String>,
    mtime: Option<FileTime>,
}

impl LockSnapshot {
    fn read(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self {
                content: None,
                mtime: None,
            });
        }

        let content = Some(fs::read_to_string(path)?);
        let mtime = fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .map(FileTime::from_system_time);

        Ok(Self { content, mtime })
    }

    fn is_active(&self, now: DateTime<Utc>, ttl_hours: i64) -> bool {
        let Some(content) = self.content.as_deref() else {
            return false;
        };
        if !content.trim().starts_with("pid:") {
            return false;
        }
        let Some(mtime) = self.mtime else {
            return true;
        };
        let Some(modified) =
            DateTime::<Utc>::from_timestamp(mtime.unix_seconds(), mtime.nanoseconds())
        else {
            return true;
        };
        now - modified < Duration::hours(ttl_hours.max(1))
    }
}

#[derive(Debug)]
struct AutoDreamLock {
    path: PathBuf,
    previous: LockSnapshot,
}

impl AutoDreamLock {
    fn release_success(self, now: DateTime<Utc>) -> Result<()> {
        fs::write(&self.path, format!("last:{}", process::id()))?;
        set_file_mtime(&self.path, file_time_from_datetime(now))?;
        Ok(())
    }

    fn release_failure(self) -> Result<()> {
        match self.previous.content {
            Some(content) => {
                fs::write(&self.path, content)?;
                if let Some(mtime) = self.previous.mtime {
                    set_file_mtime(&self.path, mtime)?;
                }
            }
            None => {
                if self.path.exists() {
                    fs::remove_file(&self.path)?;
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SourceKind {
    Topic,
    Log,
    Session,
}

impl SourceKind {
    fn label(self) -> &'static str {
        match self {
            SourceKind::Topic => "topic",
            SourceKind::Log => "log",
            SourceKind::Session => "session",
        }
    }
}

#[derive(Debug, Clone)]
struct DreamSource {
    kind: SourceKind,
    relative_path: String,
    excerpt: String,
}

#[derive(Debug, Clone, Default)]
struct DreamSources {
    items: Vec<DreamSource>,
}

impl DreamSources {
    fn count_kind(&self, kind: SourceKind) -> usize {
        self.items.iter().filter(|item| item.kind == kind).count()
    }

    fn by_kind(&self) -> BTreeMap<SourceKind, Vec<&DreamSource>> {
        let mut grouped: BTreeMap<SourceKind, Vec<&DreamSource>> = BTreeMap::new();
        for source in &self.items {
            grouped.entry(source.kind).or_default().push(source);
        }
        grouped
    }
}

fn gather_sources(paths: &AutoDreamPaths) -> Result<DreamSources> {
    let mut sources = Vec::new();
    if paths.memory_dir.exists() {
        for entry in WalkDir::new(&paths.memory_dir)
            .into_iter()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().is_file())
        {
            let path = entry.path();
            let Some(extension) = path.extension() else {
                continue;
            };
            if extension != "md" {
                continue;
            }
            let relative = relative_path(&paths.memory_dir, path)?;
            if relative == "MEMORY.md" || relative.starts_with('.') {
                continue;
            }
            let kind = if relative.starts_with("logs/") {
                SourceKind::Log
            } else {
                SourceKind::Topic
            };
            if sources
                .iter()
                .filter(|source: &&DreamSource| source.kind == kind)
                .count()
                >= MAX_SOURCES_PER_KIND
            {
                continue;
            }
            sources.push(DreamSource {
                kind,
                relative_path: relative,
                excerpt: summarize_source(path)?,
            });
        }
    }

    if paths.sessions_dir.exists() {
        for entry in WalkDir::new(&paths.sessions_dir)
            .max_depth(3)
            .into_iter()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().is_file())
        {
            let path = entry.path();
            let is_transcript = path
                .file_name()
                .map(|name| name == "transcript.jsonl")
                .unwrap_or(false);
            let is_legacy_json = path
                .extension()
                .map(|extension| extension == "json")
                .unwrap_or(false)
                && path.parent() == Some(paths.sessions_dir.as_path());
            if !is_transcript && !is_legacy_json {
                continue;
            }
            if sources
                .iter()
                .filter(|source| source.kind == SourceKind::Session)
                .count()
                >= MAX_SOURCES_PER_KIND
            {
                continue;
            }
            sources.push(DreamSource {
                kind: SourceKind::Session,
                relative_path: format!("sessions/{}", relative_path(&paths.sessions_dir, path)?),
                excerpt: summarize_source(path)?,
            });
        }
    }

    sources.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.relative_path.cmp(&right.relative_path))
    });

    Ok(DreamSources { items: sources })
}

fn build_dream_prompt(
    paths: &AutoDreamPaths,
    sources: &DreamSources,
    now: DateTime<Utc>,
) -> String {
    let mut prompt = String::new();
    prompt.push_str("# Auto Dream Memory Consolidation\n\n");
    prompt.push_str("Do not extract new memories from the active turn. Organize and prune the existing project memory directory only.\n\n");
    prompt.push_str(&format!(
        "Memory directory: {}\n",
        paths.memory_dir.display()
    ));
    prompt.push_str(&format!("Run date: {}\n\n", now.format("%Y-%m-%d")));
    prompt.push_str("Phase 1: Orient\n");
    prompt.push_str("- List the memory directory.\n");
    prompt.push_str("- Read MEMORY.md first.\n");
    prompt.push_str("- Browse existing topic files before creating or referencing duplicates.\n\n");
    prompt.push_str("Phase 2: Gather\n");
    prompt.push_str("- Prioritize logs/YYYY/MM/YYYY-MM-DD.md and KAIROS append logs.\n");
    prompt.push_str("- Reconcile outdated memories contradicted by current project state.\n");
    prompt.push_str("- Use narrow JSONL session records; never read every session wholesale.\n\n");
    prompt.push_str("Phase 3: Consolidate\n");
    prompt.push_str("- Merge new signals into existing topic files.\n");
    prompt
        .push_str("- Convert relative dates such as yesterday and last week to absolute dates.\n");
    prompt.push_str("- Delete or stop surfacing overturned facts.\n\n");
    prompt.push_str("Phase 4: Prune and Index\n");
    prompt.push_str("- Keep MEMORY.md at or below 200 lines and 25KB.\n");
    prompt.push_str("- Keep each index item on one line and at or below 150 characters.\n");
    prompt.push_str("- Remove stale, wrong, or replaced pointers.\n\n");
    prompt.push_str("Memory policy\n");
    prompt.push_str("- Keep user, feedback, project, and reference memories.\n");
    prompt.push_str("- Do not save code patterns, architecture, file paths, Git history, or debugging plans that are derivable from source control or code.\n\n");
    prompt.push_str("Discovered sources\n");
    for (kind, items) in sources.by_kind() {
        prompt.push_str(&format!("- {}: {}\n", kind.label(), items.len()));
        for item in items.iter().take(10) {
            prompt.push_str(&format!("  - {}\n", item.relative_path));
        }
    }
    prompt
}

fn build_pruned_index(
    paths: &AutoDreamPaths,
    sources: &DreamSources,
    now: DateTime<Utc>,
) -> Result<String> {
    let mut lines = Vec::new();
    lines.push("# Project Memory Index".to_string());
    lines.push(truncate_chars(
        &format!(
            "- Auto Dream updated {}; scanned {} topic files, {} logs, {} session records.",
            now.format("%Y-%m-%d"),
            sources.count_kind(SourceKind::Topic),
            sources.count_kind(SourceKind::Log),
            sources.count_kind(SourceKind::Session)
        ),
        MAX_INDEX_LINE_CHARS,
    ));

    let existing_index = paths.memory_dir.join("MEMORY.md");
    for source in &sources.items {
        let excerpt = normalize_relative_dates(&source.excerpt, now);
        let line = truncate_chars(
            &format!(
                "- {} [{}] {}",
                source.relative_path,
                source.kind.label(),
                excerpt
            ),
            MAX_INDEX_LINE_CHARS,
        );
        push_unique_line(&mut lines, line);
    }

    if existing_index.is_file() {
        let existing = fs::read_to_string(existing_index)?;
        for line in existing.lines() {
            let normalized = normalize_relative_dates(line.trim(), now);
            if normalized.is_empty() || normalized.starts_with("# Project Memory Index") {
                continue;
            }
            push_unique_line(
                &mut lines,
                truncate_chars(&normalized, MAX_INDEX_LINE_CHARS),
            );
        }
    }

    Ok(enforce_index_budget(lines))
}

fn push_unique_line(lines: &mut Vec<String>, line: String) {
    let cleaned = line.trim();
    if cleaned.is_empty() {
        return;
    }
    if !lines.iter().any(|existing| existing == cleaned) {
        lines.push(cleaned.to_string());
    }
}

fn enforce_index_budget(lines: Vec<String>) -> String {
    let mut output = String::new();
    for line in lines.into_iter().take(MAX_INDEX_LINES) {
        let line = truncate_chars(&line, MAX_INDEX_LINE_CHARS);
        let additional = line.len() + usize::from(!output.is_empty());
        if output.len() + additional > MAX_INDEX_BYTES {
            break;
        }
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&line);
    }
    output
}

fn summarize_source(path: &Path) -> Result<String> {
    let content = read_limited(path, MAX_SOURCE_BYTES)?;
    let mut fallback = None;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with('{')
            || trimmed.starts_with('}')
            || trimmed.starts_with("---")
        {
            continue;
        }
        let cleaned = trimmed.trim_start_matches('#').trim();
        if cleaned.is_empty() {
            continue;
        }
        if fallback.is_none() {
            fallback = Some(cleaned.to_string());
        }
        if cleaned.starts_with("summary:") || cleaned.starts_with("- ") {
            return Ok(cleaned.to_string());
        }
    }

    Ok(fallback.unwrap_or_else(|| "recent memory signal".to_string()))
}

fn read_limited(path: &Path, max_bytes: usize) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(max_bytes as u64)
        .read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes)
        .replace('\r', "")
        .replace('\n', " "))
}

fn normalize_relative_dates(input: &str, now: DateTime<Utc>) -> String {
    let yesterday = (now - Duration::days(1)).format("%Y-%m-%d").to_string();
    let last_week = (now - Duration::days(7)).format("%Y-%m-%d").to_string();
    input
        .replace("Yesterday", &yesterday)
        .replace("yesterday", &yesterday)
        .replace("昨天", &yesterday)
        .replace("Last week", &last_week)
        .replace("last week", &last_week)
        .replace("上周", &last_week)
}

fn truncate_chars(input: &str, max_chars: usize) -> String {
    let mut output = String::new();
    for character in input.chars().take(max_chars) {
        output.push(character);
    }
    output
}

fn relative_path(root: &Path, path: &Path) -> Result<String> {
    Ok(path
        .strip_prefix(root)?
        .to_string_lossy()
        .replace('\\', "/"))
}

fn default_project_memory_directory(
    current_working_dir: &Path,
    workspace_root: &Path,
) -> Option<PathBuf> {
    let cwd = if current_working_dir.is_absolute() {
        current_working_dir.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(current_working_dir)
    };
    let workspace = if workspace_root.is_absolute() {
        workspace_root.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(workspace_root)
    };
    let current = if cwd.starts_with(&workspace) {
        cwd
    } else {
        workspace
    };
    let project_root = find_canonical_git_root(&current).unwrap_or(current);
    let home = dirs::home_dir()?;
    Some(
        home.join(".claude")
            .join("projects")
            .join(sanitize_project_memory_slug(&project_root))
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
            let content = fs::read_to_string(&dot_git).ok()?;
            let redirected = content
                .lines()
                .find_map(|line| line.trim().strip_prefix("gitdir:").map(str::trim))?;
            let candidate = PathBuf::from(redirected);
            let git_dir = if candidate.is_absolute() {
                candidate
            } else {
                directory.join(candidate)
            };
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
            return git_dir.parent().map(Path::to_path_buf);
        }
    }

    None
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

fn absolutize(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn file_time_from_datetime(value: DateTime<Utc>) -> FileTime {
    FileTime::from_unix_time(value.timestamp(), value.timestamp_subsec_nanos())
}

trait Pipe: Sized {
    fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T {
        f(self)
    }
}

impl<T> Pipe for T {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: String,
    pub memory_type: String,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    pub metadata: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidatedInsight {
    pub topic: String,
    pub summary: String,
    pub memory_count: usize,
    pub last_updated: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AutoDreamStatus {
    pub enabled: bool,
    pub is_consolidating: bool,
    pub last_consolidation: DateTime<Utc>,
    pub hours_since_last: i64,
    pub sessions_accumulated: usize,
    pub next_consolidation_in: i64,
    pub last_skip_reason: Option<String>,
    pub memory_dir: Option<PathBuf>,
    pub sessions_dir: Option<PathBuf>,
    pub lock_present: bool,
    pub last_report: Option<AutoDreamRunReport>,
}
