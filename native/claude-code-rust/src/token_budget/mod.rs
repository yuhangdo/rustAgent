use crate::api::{ChatMessage, ToolDefinition};

pub const DEFAULT_CONTEXT_WINDOW_TOKENS: usize = 200_000;
pub const ONE_MILLION_CONTEXT_TOKENS: usize = 1_000_000;
pub const DEFAULT_MAX_OUTPUT_TOKENS: usize = 8_000;
pub const AUTOCOMPACT_BUFFER_TOKENS: usize = 13_000;
pub const WARNING_THRESHOLD_BUFFER_TOKENS: usize = 20_000;
pub const ERROR_THRESHOLD_BUFFER_TOKENS: usize = 20_000;
pub const MANUAL_COMPACT_BUFFER_TOKENS: usize = 3_000;
pub const MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES: usize = 3;
pub const SLOT_RETRY_MAX_TOKENS: usize = 64_000;
pub const POST_COMPACT_TOKEN_BUDGET: usize = 50_000;
const DEFAULT_MESSAGE_ENVELOPE_TOKENS: usize = 6;
const FIXED_ATTACHMENT_TOKEN_COST: usize = 2_000;
const BLOCKING_OVERFLOW_TOKENS: usize = 17_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    AnthropicNative,
    OpenAICompatible,
    GeminiCompatible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelCapability {
    pub max_input_tokens: usize,
    pub max_output_tokens: usize,
    pub supports_one_million_context: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenThresholds {
    pub warning_tokens: usize,
    pub autocompact_tokens: usize,
    pub blocking_tokens: usize,
    pub manual_compact_tokens: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetSource {
    Normal,
    Compact,
    SessionMemory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenBudgetDecision {
    Proceed,
    Warn,
    AutoCompact,
    Block,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenBudgetState {
    pub context_window_tokens: usize,
    pub effective_budget_tokens: usize,
    pub latest_rough_count: usize,
    pub latest_exact_count: Option<usize>,
    pub warning_emitted: bool,
    pub blocked: bool,
    pub consecutive_autocompact_failures: usize,
}

impl TokenBudgetState {
    pub fn new(context_window_tokens: usize, max_output_tokens: usize) -> Self {
        Self {
            context_window_tokens,
            effective_budget_tokens: effective_budget(context_window_tokens, max_output_tokens),
            latest_rough_count: 0,
            latest_exact_count: None,
            warning_emitted: false,
            blocked: false,
            consecutive_autocompact_failures: 0,
        }
    }

    pub fn with_counts(mut self, rough_count: usize, exact_count: Option<usize>) -> Self {
        self.latest_rough_count = rough_count;
        self.latest_exact_count = exact_count;
        self
    }

    pub fn with_consecutive_failures(mut self, failures: usize) -> Self {
        self.consecutive_autocompact_failures = failures;
        self
    }

    pub fn active_count(&self) -> usize {
        self.latest_exact_count.unwrap_or(self.latest_rough_count)
    }

    pub fn thresholds(&self) -> TokenThresholds {
        thresholds_for_budget(self.effective_budget_tokens)
    }
}

pub fn provider_kind_for_base_url(base_url: &str) -> ProviderKind {
    let normalized = base_url.to_ascii_lowercase();
    if normalized.contains("anthropic.com") {
        ProviderKind::AnthropicNative
    } else if normalized.contains("generativelanguage")
        || normalized.contains("googleapis.com")
        || normalized.contains("gemini")
    {
        ProviderKind::GeminiCompatible
    } else {
        ProviderKind::OpenAICompatible
    }
}

pub fn model_capability(model: &str) -> ModelCapability {
    let normalized = model.to_ascii_lowercase();

    if normalized.contains("haiku") {
        return ModelCapability {
            max_input_tokens: DEFAULT_CONTEXT_WINDOW_TOKENS,
            max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
            supports_one_million_context: false,
        };
    }

    if normalized.contains("sonnet") || normalized.contains("opus") || normalized.contains("claude")
    {
        return ModelCapability {
            max_input_tokens: DEFAULT_CONTEXT_WINDOW_TOKENS,
            max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
            supports_one_million_context: normalized.contains("sonnet-4")
                || normalized.contains("4-sonnet")
                || normalized.contains("claude-sonnet-4"),
        };
    }

    if normalized.contains("gpt-5")
        || normalized.contains("gpt-4")
        || normalized.contains("o3")
        || normalized.contains("o4")
        || normalized.contains("gemini")
    {
        return ModelCapability {
            max_input_tokens: 128_000,
            max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
            supports_one_million_context: false,
        };
    }

    ModelCapability {
        max_input_tokens: DEFAULT_CONTEXT_WINDOW_TOKENS,
        max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
        supports_one_million_context: false,
    }
}

pub fn resolve_context_window(
    model: &str,
    provider_kind: ProviderKind,
    beta_headers: &[String],
) -> usize {
    if let Some(override_tokens) = std::env::var("CLAUDE_CODE_MAX_CONTEXT_TOKENS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
    {
        return override_tokens;
    }

    if model.to_ascii_lowercase().contains("[1m]") {
        return ONE_MILLION_CONTEXT_TOKENS;
    }

    let capability = model_capability(model);
    if provider_kind == ProviderKind::AnthropicNative
        && capability.supports_one_million_context
        && beta_headers
            .iter()
            .any(|header| header.to_ascii_lowercase().contains("1m"))
    {
        return ONE_MILLION_CONTEXT_TOKENS;
    }

    capability
        .max_input_tokens
        .max(DEFAULT_CONTEXT_WINDOW_TOKENS)
}

pub fn effective_budget(context_window_tokens: usize, max_output_tokens: usize) -> usize {
    context_window_tokens.saturating_sub(max_output_tokens.min(20_000))
}

pub fn thresholds_for_budget(effective_budget_tokens: usize) -> TokenThresholds {
    let autocompact_tokens = effective_budget_tokens.saturating_sub(AUTOCOMPACT_BUFFER_TOKENS);
    TokenThresholds {
        warning_tokens: autocompact_tokens.saturating_sub(WARNING_THRESHOLD_BUFFER_TOKENS),
        autocompact_tokens,
        blocking_tokens: effective_budget_tokens.saturating_add(BLOCKING_OVERFLOW_TOKENS),
        manual_compact_tokens: effective_budget_tokens.saturating_sub(MANUAL_COMPACT_BUFFER_TOKENS),
    }
}

pub fn evaluate_budget_decision(
    state: &TokenBudgetState,
    auto_compact_enabled: bool,
    source: BudgetSource,
) -> TokenBudgetDecision {
    let active_count = state.active_count();
    let thresholds = state.thresholds();

    if active_count >= thresholds.blocking_tokens {
        return TokenBudgetDecision::Block;
    }

    if !auto_compact_enabled
        || matches!(source, BudgetSource::Compact | BudgetSource::SessionMemory)
        || state.consecutive_autocompact_failures >= MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES
    {
        if !state.warning_emitted && active_count >= thresholds.warning_tokens {
            return TokenBudgetDecision::Warn;
        }
        return TokenBudgetDecision::Proceed;
    }

    if active_count >= thresholds.autocompact_tokens {
        return TokenBudgetDecision::AutoCompact;
    }

    if !state.warning_emitted && active_count >= thresholds.warning_tokens {
        return TokenBudgetDecision::Warn;
    }

    TokenBudgetDecision::Proceed
}

pub fn rough_count_tools(tool_definitions: &[ToolDefinition]) -> usize {
    tool_definitions
        .iter()
        .map(|tool| rough_count_text(&serde_json::to_string(tool).unwrap_or_default(), false))
        .sum()
}

pub fn rough_count_messages(messages: &[ChatMessage]) -> usize {
    messages.iter().map(rough_count_message).sum()
}

pub fn rough_count_message(message: &ChatMessage) -> usize {
    let mut total = DEFAULT_MESSAGE_ENVELOPE_TOKENS;

    if let Some(content) = &message.content {
        total += rough_count_text(content, false);
    }

    if let Some(reasoning_content) = &message.reasoning_content {
        total += rough_count_text(reasoning_content, false);
    }

    if let Some(tool_calls) = &message.tool_calls {
        for tool_call in tool_calls {
            total += rough_count_text(&tool_call.function.name, false);
            total += rough_count_text(&tool_call.function.arguments, true);
        }
    }

    if let Some(tool_call_id) = &message.tool_call_id {
        total += rough_count_text(tool_call_id, false);
    }

    total
}

pub fn rough_count_text(value: &str, force_json_weighting: bool) -> usize {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return 0;
    }

    if trimmed.eq_ignore_ascii_case("[image]") || trimmed.eq_ignore_ascii_case("[document]") {
        return FIXED_ATTACHMENT_TOKEN_COST;
    }

    if trimmed.contains("[image]") || trimmed.contains("[document]") {
        return FIXED_ATTACHMENT_TOKEN_COST + base_char_tokens(trimmed, false);
    }

    if force_json_weighting || looks_like_json(trimmed) || looks_like_jsonl(trimmed) {
        return base_char_tokens(trimmed, true);
    }

    base_char_tokens(trimmed, false)
}

fn base_char_tokens(value: &str, json_weighting: bool) -> usize {
    let char_count = value.chars().count();
    if json_weighting {
        (char_count / 2).max(1) + 1
    } else {
        (char_count / 4).max(1) + 1
    }
}

fn looks_like_json(value: &str) -> bool {
    (value.starts_with('{') && value.ends_with('}'))
        || (value.starts_with('[') && value.ends_with(']'))
}

fn looks_like_jsonl(value: &str) -> bool {
    let mut saw_line = false;
    for line in value.lines().map(str::trim).filter(|line| !line.is_empty()) {
        saw_line = true;
        if !looks_like_json(line) {
            return false;
        }
    }
    saw_line
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Mutex, OnceLock};

    fn test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env test lock")
    }

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

    #[test]
    fn context_window_prefers_environment_override() {
        let _guard = test_lock();
        std::env::set_var("CLAUDE_CODE_MAX_CONTEXT_TOKENS", "777777");

        let resolved = resolve_context_window(
            "claude-sonnet-4-20250514",
            ProviderKind::AnthropicNative,
            &["context-1m-2025-08-07".to_string()],
        );

        std::env::remove_var("CLAUDE_CODE_MAX_CONTEXT_TOKENS");
        assert_eq!(resolved, 777_777);
    }

    #[test]
    fn context_window_honors_model_suffix_before_capability_table() {
        let _guard = test_lock();
        std::env::remove_var("CLAUDE_CODE_MAX_CONTEXT_TOKENS");

        let resolved = resolve_context_window(
            "claude-sonnet-4-20250514[1m]",
            ProviderKind::AnthropicNative,
            &[],
        );

        assert_eq!(resolved, ONE_MILLION_CONTEXT_TOKENS);
    }

    #[test]
    fn context_window_uses_beta_header_for_supported_anthropic_models() {
        let _guard = test_lock();
        std::env::remove_var("CLAUDE_CODE_MAX_CONTEXT_TOKENS");

        let resolved = resolve_context_window(
            "claude-sonnet-4-20250514",
            ProviderKind::AnthropicNative,
            &["context-1m-2025-08-07".to_string()],
        );

        assert_eq!(resolved, ONE_MILLION_CONTEXT_TOKENS);
    }

    #[test]
    fn effective_budget_reserves_at_most_twenty_thousand_output_tokens() {
        assert_eq!(effective_budget(200_000, 64_000), 180_000);
        assert_eq!(effective_budget(200_000, 8_000), 192_000);
    }

    #[test]
    fn rough_count_uses_json_weighting_and_attachment_sentinels() {
        let json_text = r#"{"path":"src","pattern":"auth","flags":["i","g"]}"#;
        let plain_text = "inspect the auth flow and validate rollout";

        assert!(rough_count_text(json_text, false) > rough_count_text(plain_text, false));
        assert_eq!(
            rough_count_text("[image]", false),
            FIXED_ATTACHMENT_TOKEN_COST
        );
        assert_eq!(
            rough_count_text("[document]", false),
            FIXED_ATTACHMENT_TOKEN_COST
        );
    }

    #[test]
    fn rough_count_messages_includes_tool_calls_and_reasoning() {
        let message = ChatMessage {
            role: "assistant".to_string(),
            content: Some("done".to_string()),
            reasoning_content: Some("need to inspect auth".to_string()),
            tool_calls: Some(vec![crate::api::ToolCall {
                id: "call_1".to_string(),
                r#type: "function".to_string(),
                function: crate::api::ToolCallFunction {
                    name: "search".to_string(),
                    arguments: r#"{"path":"src","pattern":"auth"}"#.to_string(),
                },
            }]),
            tool_call_id: None,
        };

        assert!(rough_count_message(&message) > DEFAULT_MESSAGE_ENVELOPE_TOKENS);
    }

    #[test]
    fn thresholds_emit_warning_then_autocompact_then_block() {
        let state = TokenBudgetState::new(200_000, 8_000);
        let thresholds = state.thresholds();

        let warn = state.clone().with_counts(thresholds.warning_tokens, None);
        let compact = state
            .clone()
            .with_counts(thresholds.autocompact_tokens, None);
        let block = state.clone().with_counts(thresholds.blocking_tokens, None);

        assert_eq!(
            evaluate_budget_decision(&warn, true, BudgetSource::Normal),
            TokenBudgetDecision::Warn
        );
        assert_eq!(
            evaluate_budget_decision(&compact, true, BudgetSource::Normal),
            TokenBudgetDecision::AutoCompact
        );
        assert_eq!(
            evaluate_budget_decision(&block, true, BudgetSource::Normal),
            TokenBudgetDecision::Block
        );
    }

    #[test]
    fn circuit_breaker_disables_autocompact_after_three_failures() {
        let state = TokenBudgetState::new(200_000, 8_000)
            .with_counts(190_000, Some(190_000))
            .with_consecutive_failures(MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES);

        assert_eq!(
            evaluate_budget_decision(&state, true, BudgetSource::Normal),
            TokenBudgetDecision::Warn
        );
    }

    #[test]
    fn rough_count_tools_serializes_tool_schema() {
        let count = rough_count_tools(&[tool_definition("search"), tool_definition("file_read")]);
        assert!(count > 0);
    }
}
