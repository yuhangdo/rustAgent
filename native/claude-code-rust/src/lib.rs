//! Claude Code Rust - High-performance CLI for Claude AI
//!
//! A complete Rust implementation of Claude Code, featuring:
//! - Async-first architecture with Tokio
//! - Native terminal UI with Ratatui
//! - MCP protocol support
//! - Voice input support
//! - Memory management and team sync
//! - Plugin system
//! - SSH connection support
//! - Remote execution
//! - Project initialization
//! - WebAssembly support for browser environments
//! - Native GUI with egui/eframe
//! - Plugin marketplace web interface
//! - Multi-language i18n support

pub mod advanced;
pub mod agent_runtime;
pub mod api;
pub mod cli;
pub mod compact;
pub mod config;
pub mod fast_path;
pub mod mcp;
pub mod memory;
#[cfg(feature = "mobile-bridge")]
pub mod mobile_bridge;
pub mod plugins;
pub mod prompting;
pub mod query_engine;
pub mod services;
pub mod session;
pub mod skills;
pub mod state;
pub mod streaming;
pub mod terminal;
pub mod token_budget;
pub mod tools;
pub mod utils;
pub mod voice;

// Feature-gated modules
#[cfg(feature = "gui-egui")]
pub mod gui;
#[cfg(feature = "i18n")]
pub mod i18n;
#[cfg(feature = "wasm")]
pub mod wasm;
#[cfg(feature = "web")]
pub mod web;

pub use agent_runtime::{
    AgentEvent, AgentExecutionOutcome, AgentExecutionRequest, AgentExecutionResult, AgentRuntime,
};
pub use api::{AnthropicClient, ApiClient, ChatMessage};
pub use cli::Cli;
pub use compact::{
    full_compact, micro_compact_history, session_memory_compact, CompactDirection, CompactResult,
    CompactStrategy,
};
pub use config::Settings;
pub use fast_path::{
    build_execution_batches, hard_route_decision, validate_quick_plan,
    validate_quick_plan_for_workspace, validate_read_only_command,
    validate_read_only_command_in_workspace, ExecutionModeHint, HardRouteDecision, QuickRouteInput,
    QuickToolPlan, QuickToolStep,
};
pub use mcp::McpManager;
pub use memory::MemoryManager;
pub use plugins::PluginManager;
pub use prompting::{
    ProjectMemoryCandidate, ProjectMemorySelectionQuery, ProjectMemorySelector, PromptAssembly,
    PromptBudget, PromptBuildRequest, PromptBuilder, PromptCacheScope, PromptSection,
    PromptSectionRole, PromptSectionSource, PromptTrimReport, RenderedPrompt,
    SYSTEM_PROMPT_DYNAMIC_BOUNDARY,
};
pub use query_engine::{
    BudgetDecision, BudgetState, BudgetTracker, FileHistoryStore, ModelUsage, QueryEngine,
    QueryRunStatus, QuerySessionSnapshot, SessionUsageTotals, TranscriptEvent, TranscriptStore,
    UsageRecord,
};
pub use skills::{
    Skill, SkillCategory, SkillContext, SkillError, SkillExecutor, SkillParams, SkillRegistry,
    SkillResult,
};
pub use state::AppState;
pub use token_budget::{
    effective_budget, evaluate_budget_decision, model_capability, provider_kind_for_base_url,
    resolve_context_window, rough_count_message, rough_count_messages, rough_count_text,
    rough_count_tools, BudgetSource, ModelCapability, ProviderKind, TokenBudgetDecision,
    TokenBudgetState, TokenThresholds, AUTOCOMPACT_BUFFER_TOKENS, DEFAULT_CONTEXT_WINDOW_TOKENS,
    DEFAULT_MAX_OUTPUT_TOKENS, ERROR_THRESHOLD_BUFFER_TOKENS, MANUAL_COMPACT_BUFFER_TOKENS,
    MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES, ONE_MILLION_CONTEXT_TOKENS, POST_COMPACT_TOKEN_BUDGET,
    SLOT_RETRY_MAX_TOKENS, WARNING_THRESHOLD_BUFFER_TOKENS,
};
pub use tools::ToolRegistry;
pub use voice::VoiceInput;

// Feature-gated re-exports
#[cfg(feature = "gui-egui")]
pub use gui::ClaudeCodeApp;
#[cfg(feature = "i18n")]
pub use i18n::Translator;
#[cfg(feature = "mobile-bridge")]
pub use mobile_bridge::{BridgeRunRequest, BridgeRunSnapshot, MobileBridgeServer};
#[cfg(feature = "wasm")]
pub use wasm::ClaudeCodeWasm;
#[cfg(feature = "web")]
pub use web::WebServer;
