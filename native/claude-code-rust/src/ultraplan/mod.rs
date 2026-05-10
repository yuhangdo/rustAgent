//! Ultraplan enhanced planning feature.
//!
//! Ultraplan is a hidden planning accelerator guarded by
//! `FEATURE_ULTRAPLAN=1` for keyword-triggered routing. Explicit `/ultraplan`
//! commands are accepted directly so scripted callers can opt into the mode.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::plan_mode::{PlanMode, PlanModeSession, PlanModeStatus};

pub const ULTRAPLAN_COMMAND: &str = "/ultraplan";
pub const ULTRAPLAN_FEATURE_FLAG: &str = "FEATURE_ULTRAPLAN";
pub const ULTRAPLAN_PROMPT: &str = include_str!("prompt.txt");
const KEYWORD: &str = "ultraplan";
const DEFAULT_CCR_POLL_INTERVAL: Duration = Duration::from_secs(3);
const DEFAULT_CCR_TIMEOUT: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UltraplanLaunchMode {
    Local,
    Remote,
}

impl UltraplanLaunchMode {
    pub fn as_str(self) -> &'static str {
        match self {
            UltraplanLaunchMode::Local => "local",
            UltraplanLaunchMode::Remote => "remote",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UltraplanTriggerPosition {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UltraplanRoute {
    pub launch_mode: UltraplanLaunchMode,
    pub original_input: String,
    pub cleaned_prompt: String,
    pub explicit_command: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum UltraplanInputAction {
    Normal,
    Route(UltraplanRoute),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UltraplanInputDecision {
    pub action: UltraplanInputAction,
    pub feature_enabled: bool,
    pub trigger_positions: Vec<UltraplanTriggerPosition>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UltraplanHighlightSpan {
    pub text: String,
    pub rainbow: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UltraplanCommandResult {
    pub launch_mode: UltraplanLaunchMode,
    pub status: PlanModeStatus,
    pub system_prompt: String,
    pub remote_session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CcrSessionState {
    Created,
    Teleported,
    Polling,
    Approved,
    TimedOut,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CcrSession {
    pub id: String,
    pub launch_mode: UltraplanLaunchMode,
    pub state: CcrSessionState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub teleport_target: Option<String>,
    pub poll_attempts: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExitPlanModeScanResult {
    pub approved: bool,
    pub timed_out: bool,
    pub poll_attempts: usize,
    pub plan_file_path: Option<PathBuf>,
    pub status: PlanModeStatus,
}

pub fn is_ultraplan_feature_enabled() -> bool {
    std::env::var(ULTRAPLAN_FEATURE_FLAG)
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

pub fn find_ultraplan_trigger_positions(text: &str) -> Vec<UltraplanTriggerPosition> {
    let trimmed = text.trim_start();
    if trimmed.starts_with('/') && !starts_with_ultraplan_command(trimmed) {
        return Vec::new();
    }

    let lower = text.to_ascii_lowercase();
    let mut positions = Vec::new();
    let mut search_from = 0;

    while let Some(relative) = lower[search_from..].find(KEYWORD) {
        let start = search_from + relative;
        let end = start + KEYWORD.len();
        if is_valid_keyword_trigger(text, start, end) {
            positions.push(UltraplanTriggerPosition { start, end });
        }
        search_from = end;
    }

    positions
}

pub fn replace_ultraplan_keyword(text: &str) -> String {
    let positions = find_ultraplan_trigger_positions(text);
    if positions.is_empty() {
        return text.to_string();
    }

    let mut cleaned = text.to_string();
    for position in positions.iter().rev() {
        cleaned.replace_range(position.start..position.end, "");
    }
    normalize_spaces(&cleaned)
}

pub fn process_ultraplan_input(text: &str) -> UltraplanInputDecision {
    let feature_enabled = is_ultraplan_feature_enabled();
    let trimmed = text.trim();
    if starts_with_ultraplan_command(trimmed) {
        let (launch_mode, cleaned_prompt) = parse_ultraplan_command(trimmed);
        return UltraplanInputDecision {
            action: UltraplanInputAction::Route(UltraplanRoute {
                launch_mode,
                original_input: text.to_string(),
                cleaned_prompt,
                explicit_command: true,
            }),
            feature_enabled,
            trigger_positions: vec![UltraplanTriggerPosition {
                start: text.find(KEYWORD).unwrap_or(0),
                end: text.find(KEYWORD).unwrap_or(0) + KEYWORD.len(),
            }],
        };
    }

    let trigger_positions = find_ultraplan_trigger_positions(text);
    if feature_enabled && !trigger_positions.is_empty() {
        return UltraplanInputDecision {
            action: UltraplanInputAction::Route(UltraplanRoute {
                launch_mode: UltraplanLaunchMode::Local,
                original_input: text.to_string(),
                cleaned_prompt: replace_ultraplan_keyword(text),
                explicit_command: false,
            }),
            feature_enabled,
            trigger_positions,
        };
    }

    UltraplanInputDecision {
        action: UltraplanInputAction::Normal,
        feature_enabled,
        trigger_positions,
    }
}

pub fn highlight_ultraplan_keyword(text: &str) -> Vec<UltraplanHighlightSpan> {
    let positions = find_ultraplan_trigger_positions(text);
    if positions.is_empty() {
        return vec![UltraplanHighlightSpan {
            text: text.to_string(),
            rainbow: false,
        }];
    }

    let mut spans = Vec::new();
    let mut cursor = 0;
    for position in positions {
        if cursor < position.start {
            spans.push(UltraplanHighlightSpan {
                text: text[cursor..position.start].to_string(),
                rainbow: false,
            });
        }
        spans.push(UltraplanHighlightSpan {
            text: text[position.start..position.end].to_string(),
            rainbow: true,
        });
        cursor = position.end;
    }
    if cursor < text.len() {
        spans.push(UltraplanHighlightSpan {
            text: text[cursor..].to_string(),
            rainbow: false,
        });
    }
    spans
}

pub fn render_rainbow_ultraplan_highlight(text: &str) -> String {
    const COLORS: [&str; 6] = ["31", "33", "32", "36", "34", "35"];
    let mut rendered = String::new();
    for span in highlight_ultraplan_keyword(text) {
        if span.rainbow {
            for (index, character) in span.text.chars().enumerate() {
                rendered.push_str(&format!(
                    "\x1b[{}m{}\x1b[0m",
                    COLORS[index % COLORS.len()],
                    character
                ));
            }
        } else {
            rendered.push_str(&span.text);
        }
    }
    rendered
}

pub fn ultraplan_system_prompt(base_system_prompt: &str, status: &PlanModeStatus) -> String {
    let goal = status
        .ultraplan
        .as_ref()
        .map(|ultraplan| ultraplan.cleaned_prompt.as_str())
        .unwrap_or("");
    format!(
        "{base_system_prompt}\n\nULTRAPLAN enhanced planning is active.\n{prompt}\n\n\
         Ultraplan goal: {goal}\n\
         Required behavior:\n\
         - Stay read-only until the plan is approved.\n\
         - Inspect the problem deeply before proposing implementation.\n\
         - Produce a deep implementation plan with file-level steps, test strategy, risks, rollback notes, and approval boundaries.\n\
         - For remote CCR mode, wait for exit_plan_mode approval to return through the scanner before implementation.\n\
         - Do not edit files, run mutating commands, commit, or push while Ultraplan is active.",
        prompt = ULTRAPLAN_PROMPT.trim()
    )
}

#[derive(Debug, Clone)]
pub struct UltraplanCommandHandler {
    session: PlanModeSession,
}

impl UltraplanCommandHandler {
    pub fn new(session: PlanModeSession) -> Self {
        Self { session }
    }

    pub async fn execute(&self, route: UltraplanRoute) -> Result<UltraplanCommandResult> {
        let remote_session_id = (route.launch_mode == UltraplanLaunchMode::Remote)
            .then(|| format!("ccr-{}", Uuid::new_v4().simple()));
        let status = self
            .session
            .enter_ultraplan(
                "default",
                route.launch_mode.as_str(),
                route.original_input.clone(),
                route.cleaned_prompt.clone(),
                remote_session_id.clone(),
            )
            .await?;
        let system_prompt = ultraplan_system_prompt("", &status);
        Ok(UltraplanCommandResult {
            launch_mode: route.launch_mode,
            status,
            system_prompt,
            remote_session_id,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ExitPlanModeScanner {
    session: PlanModeSession,
    poll_interval: Duration,
    timeout: Duration,
}

impl ExitPlanModeScanner {
    pub fn new(session: PlanModeSession) -> Self {
        Self {
            session,
            poll_interval: DEFAULT_CCR_POLL_INTERVAL,
            timeout: DEFAULT_CCR_TIMEOUT,
        }
    }

    pub fn with_poll_interval(mut self, poll_interval: Duration) -> Self {
        self.poll_interval = poll_interval;
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub async fn poll_for_approved_exit_plan_mode(&self) -> Result<ExitPlanModeScanResult> {
        let started = Instant::now();
        let mut attempts = 0usize;

        loop {
            attempts += 1;
            let status = self.session.status().await;
            if status.mode == PlanMode::AwaitingApproval && status.awaiting_approval {
                return Ok(ExitPlanModeScanResult {
                    approved: true,
                    timed_out: false,
                    poll_attempts: attempts,
                    plan_file_path: Some(status.plan_file_path.clone()),
                    status,
                });
            }

            if started.elapsed() >= self.timeout {
                return Ok(ExitPlanModeScanResult {
                    approved: false,
                    timed_out: true,
                    poll_attempts: attempts,
                    plan_file_path: None,
                    status,
                });
            }

            tokio::time::sleep(self.poll_interval).await;
        }
    }
}

impl CcrSession {
    pub fn new(launch_mode: UltraplanLaunchMode, teleport_target: Option<String>) -> Self {
        let now = Utc::now();
        Self {
            id: format!("ccr-{}", Uuid::new_v4().simple()),
            launch_mode,
            state: CcrSessionState::Created,
            created_at: now,
            updated_at: now,
            teleport_target,
            poll_attempts: 0,
        }
    }

    pub fn teleport_to_remote(mut self) -> Self {
        self.state = CcrSessionState::Teleported;
        self.updated_at = Utc::now();
        self
    }

    pub fn mark_polling(mut self, poll_attempts: usize) -> Self {
        self.state = CcrSessionState::Polling;
        self.poll_attempts = poll_attempts;
        self.updated_at = Utc::now();
        self
    }
}

fn starts_with_ultraplan_command(trimmed: &str) -> bool {
    trimmed == ULTRAPLAN_COMMAND
        || trimmed
            .strip_prefix(ULTRAPLAN_COMMAND)
            .map(|rest| rest.starts_with(char::is_whitespace))
            .unwrap_or(false)
}

fn parse_ultraplan_command(trimmed: &str) -> (UltraplanLaunchMode, String) {
    let mut launch_mode = UltraplanLaunchMode::Local;
    let mut prompt_parts = Vec::new();

    for part in trimmed
        .strip_prefix(ULTRAPLAN_COMMAND)
        .unwrap_or("")
        .split_whitespace()
    {
        match part {
            "--remote" | "remote" => launch_mode = UltraplanLaunchMode::Remote,
            "--local" | "local" => launch_mode = UltraplanLaunchMode::Local,
            value => prompt_parts.push(value),
        }
    }

    (launch_mode, prompt_parts.join(" "))
}

fn is_valid_keyword_trigger(text: &str, start: usize, end: usize) -> bool {
    if !is_word_boundary(text, start, end) {
        return false;
    }
    if is_inside_quote(text, start) {
        return false;
    }
    if is_path_component(text, start, end) {
        return false;
    }
    true
}

fn is_word_boundary(text: &str, start: usize, end: usize) -> bool {
    let before = text[..start].chars().next_back();
    let after = text[end..].chars().next();
    !before.map(is_identifier_char).unwrap_or(false)
        && !after.map(is_identifier_char).unwrap_or(false)
}

fn is_identifier_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_' || character == '-'
}

fn is_inside_quote(text: &str, index: usize) -> bool {
    let mut double = false;
    let mut single = false;
    let mut backtick = false;
    let mut escaped = false;

    for character in text[..index].chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        match character {
            '"' if !single && !backtick => double = !double,
            '\'' if !double && !backtick => single = !single,
            '`' if !single && !double => backtick = !backtick,
            _ => {}
        }
    }

    double || single || backtick
}

fn is_path_component(text: &str, start: usize, end: usize) -> bool {
    let before = text[..start].chars().next_back();
    let after = text[end..].chars().next();
    if before.map(is_path_separator).unwrap_or(false)
        || after.map(is_path_separator).unwrap_or(false)
    {
        return true;
    }

    let token_start = text[..start]
        .rfind(char::is_whitespace)
        .map(|index| index + 1)
        .unwrap_or(0);
    let token_end = text[end..]
        .find(char::is_whitespace)
        .map(|index| end + index)
        .unwrap_or(text.len());
    let token = &text[token_start..token_end];
    token.contains('/') || token.contains('\\')
}

fn is_path_separator(character: char) -> bool {
    character == '/' || character == '\\'
}

fn normalize_spaces(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_prefix_requires_boundary() {
        assert!(starts_with_ultraplan_command("/ultraplan"));
        assert!(starts_with_ultraplan_command("/ultraplan test"));
        assert!(!starts_with_ultraplan_command("/ultraplanish test"));
    }
}
