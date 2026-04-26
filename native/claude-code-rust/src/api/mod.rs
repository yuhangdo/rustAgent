//! API Module - OpenAI/DeepSeek compatible API Client

use crate::config::Settings;
use crate::token_budget::{
    provider_kind_for_base_url, ProviderKind, DEFAULT_MAX_OUTPUT_TOKENS, SLOT_RETRY_MAX_TOKENS,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Clone)]
pub struct ApiClient {
    settings: Settings,
    http_client: std::sync::Arc<Client>,
}

impl ApiClient {
    pub fn new(settings: Settings) -> Self {
        let http_client = Client::builder()
            .timeout(Duration::from_secs(settings.api.timeout))
            .build()
            .unwrap_or_default();

        Self {
            settings,
            http_client: std::sync::Arc::new(http_client),
        }
    }

    pub fn get_api_key(&self) -> Option<String> {
        self.settings.api.get_api_key()
    }

    pub fn get_base_url(&self) -> String {
        self.settings.api.get_base_url()
    }

    pub fn get_model(&self) -> &str {
        &self.settings.model
    }

    pub fn streaming_enabled(&self) -> bool {
        self.settings.api.streaming
    }

    pub fn beta_headers(&self) -> &[String] {
        &self.settings.api.beta_headers
    }

    pub fn timeout_seconds(&self) -> u64 {
        self.settings.api.timeout
    }

    pub fn provider_kind(&self) -> ProviderKind {
        provider_kind_for_base_url(&self.get_base_url())
    }

    pub async fn chat(
        &self,
        messages: Vec<ChatMessage>,
        tools: Option<Vec<ToolDefinition>>,
    ) -> anyhow::Result<ChatResponse> {
        self.chat_with_overrides_and_metadata(messages, tools, None, None, None, None)
            .await
    }

    pub async fn chat_with_overrides(
        &self,
        messages: Vec<ChatMessage>,
        tools: Option<Vec<ToolDefinition>>,
        model_override: Option<&str>,
        max_tokens_override: Option<usize>,
        temperature_override: Option<f32>,
    ) -> anyhow::Result<ChatResponse> {
        self.chat_with_overrides_and_metadata(
            messages,
            tools,
            model_override,
            max_tokens_override,
            temperature_override,
            None,
        )
        .await
    }

    pub async fn chat_with_overrides_and_metadata(
        &self,
        messages: Vec<ChatMessage>,
        tools: Option<Vec<ToolDefinition>>,
        model_override: Option<&str>,
        max_tokens_override: Option<usize>,
        temperature_override: Option<f32>,
        prompt_cache_metadata: Option<&PromptCacheMetadata>,
    ) -> anyhow::Result<ChatResponse> {
        let resolved_model = self
            .settings
            .api
            .get_model_id(model_override.unwrap_or(self.settings.model.as_str()));
        let response = self
            .send_chat_request(
                messages,
                tools,
                &resolved_model,
                max_tokens_override.unwrap_or(self.settings.api.max_tokens),
                temperature_override.unwrap_or(0.7),
                false,
                prompt_cache_metadata,
            )
            .await?;
        self.parse_chat_response(response, self.provider_kind())
            .await
    }

    pub async fn chat_stream(
        &self,
        messages: Vec<ChatMessage>,
        tools: Option<Vec<ToolDefinition>>,
    ) -> anyhow::Result<reqwest::Response> {
        self.chat_stream_with_metadata(messages, tools, None).await
    }

    pub async fn chat_stream_with_metadata(
        &self,
        messages: Vec<ChatMessage>,
        tools: Option<Vec<ToolDefinition>>,
        prompt_cache_metadata: Option<&PromptCacheMetadata>,
    ) -> anyhow::Result<reqwest::Response> {
        let resolved_model = self.settings.api.get_model_id(&self.settings.model);
        self.send_chat_request(
            messages,
            tools,
            &resolved_model,
            self.settings.api.max_tokens,
            0.7,
            true,
            prompt_cache_metadata,
        )
        .await
    }

    pub async fn chat_with_slot_strategy(
        &self,
        messages: Vec<ChatMessage>,
        tools: Option<Vec<ToolDefinition>>,
        model_override: Option<&str>,
        temperature_override: Option<f32>,
    ) -> anyhow::Result<SlotStrategyResponse> {
        self.chat_with_slot_strategy_and_metadata(
            messages,
            tools,
            model_override,
            temperature_override,
            None,
        )
        .await
    }

    pub async fn chat_with_slot_strategy_and_metadata(
        &self,
        messages: Vec<ChatMessage>,
        tools: Option<Vec<ToolDefinition>>,
        model_override: Option<&str>,
        temperature_override: Option<f32>,
        prompt_cache_metadata: Option<&PromptCacheMetadata>,
    ) -> anyhow::Result<SlotStrategyResponse> {
        let resolved_model = self
            .settings
            .api
            .get_model_id(model_override.unwrap_or(self.settings.model.as_str()));
        let initial_max_tokens = self.settings.api.max_tokens.min(DEFAULT_MAX_OUTPUT_TOKENS);
        let first = self
            .chat_with_overrides_and_metadata(
                messages.clone(),
                tools.clone(),
                Some(&resolved_model),
                Some(initial_max_tokens),
                temperature_override,
                prompt_cache_metadata,
            )
            .await?;

        if should_retry_with_large_output(&first) {
            let retry = self
                .chat_with_overrides_and_metadata(
                    messages,
                    tools,
                    Some(&resolved_model),
                    Some(SLOT_RETRY_MAX_TOKENS),
                    temperature_override,
                    prompt_cache_metadata,
                )
                .await?;
            Ok(SlotStrategyResponse {
                response: retry,
                used_max_tokens: SLOT_RETRY_MAX_TOKENS,
                retried_with_large_slot: true,
            })
        } else {
            Ok(SlotStrategyResponse {
                response: first,
                used_max_tokens: initial_max_tokens,
                retried_with_large_slot: false,
            })
        }
    }

    pub async fn count_tokens(
        &self,
        messages: Vec<ChatMessage>,
        tools: Option<Vec<ToolDefinition>>,
        model_override: Option<&str>,
    ) -> anyhow::Result<Option<usize>> {
        self.count_tokens_with_metadata(messages, tools, model_override, None)
            .await
    }

    pub async fn count_tokens_with_metadata(
        &self,
        messages: Vec<ChatMessage>,
        tools: Option<Vec<ToolDefinition>>,
        model_override: Option<&str>,
        prompt_cache_metadata: Option<&PromptCacheMetadata>,
    ) -> anyhow::Result<Option<usize>> {
        if self.provider_kind() != ProviderKind::AnthropicNative {
            return Ok(None);
        }

        let resolved_model = self
            .settings
            .api
            .get_model_id(model_override.unwrap_or(self.settings.model.as_str()));
        let api_key = self
            .get_api_key()
            .ok_or_else(|| anyhow::anyhow!("API key not configured"))?;
        let request = build_anthropic_count_tokens_request(
            &resolved_model,
            messages,
            tools,
            cache_enabled(),
            prompt_cache_metadata,
        )?;
        let url = build_anthropic_count_tokens_url(&self.get_base_url());
        let mut request_builder = self
            .http_client
            .post(&url)
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json");
        if !self.settings.api.beta_headers.is_empty() {
            request_builder =
                request_builder.header("anthropic-beta", self.settings.api.beta_headers.join(","));
        }

        let response = request_builder.json(&request).send().await?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("API error ({}): {}", status, body));
        }

        let parsed: AnthropicCountTokensResponse = response.json().await?;
        Ok(Some(parsed.input_tokens))
    }

    async fn send_chat_request(
        &self,
        messages: Vec<ChatMessage>,
        tools: Option<Vec<ToolDefinition>>,
        model: &str,
        max_tokens: usize,
        temperature: f32,
        stream: bool,
        prompt_cache_metadata: Option<&PromptCacheMetadata>,
    ) -> anyhow::Result<reqwest::Response> {
        match self.provider_kind() {
            ProviderKind::AnthropicNative => {
                self.send_anthropic_request(
                    messages,
                    tools,
                    model,
                    max_tokens,
                    temperature,
                    stream,
                    prompt_cache_metadata,
                )
                .await
            }
            ProviderKind::OpenAICompatible | ProviderKind::GeminiCompatible => {
                let request = ChatRequest {
                    model: model.to_string(),
                    messages,
                    max_tokens,
                    stream,
                    temperature,
                    tools,
                };
                self.send_openai_compatible_request(&request, stream).await
            }
        }
    }

    async fn send_openai_compatible_request(
        &self,
        request: &ChatRequest,
        stream: bool,
    ) -> anyhow::Result<reqwest::Response> {
        let api_key = self
            .get_api_key()
            .ok_or_else(|| anyhow::anyhow!("API key not configured"))?;
        let url = build_chat_completions_url(&self.get_base_url());
        let mut request_builder = self
            .http_client
            .post(&url)
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json");
        if stream {
            request_builder = request_builder.header("Accept", "text/event-stream");
        }

        let response = request_builder.json(request).send().await?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("API error ({}): {}", status, body));
        }

        Ok(response)
    }

    async fn send_anthropic_request(
        &self,
        messages: Vec<ChatMessage>,
        tools: Option<Vec<ToolDefinition>>,
        model: &str,
        max_tokens: usize,
        temperature: f32,
        stream: bool,
        prompt_cache_metadata: Option<&PromptCacheMetadata>,
    ) -> anyhow::Result<reqwest::Response> {
        let api_key = self
            .get_api_key()
            .ok_or_else(|| anyhow::anyhow!("API key not configured"))?;
        let url = build_anthropic_messages_url(&self.get_base_url());
        let request = build_anthropic_messages_request(
            model,
            messages,
            tools,
            max_tokens,
            temperature,
            stream,
            cache_enabled(),
            prompt_cache_metadata,
        )?;
        let mut request_builder = self
            .http_client
            .post(&url)
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json");
        if stream {
            request_builder = request_builder.header("Accept", "text/event-stream");
        }
        if !self.settings.api.beta_headers.is_empty() {
            request_builder =
                request_builder.header("anthropic-beta", self.settings.api.beta_headers.join(","));
        }

        let response = request_builder.json(&request).send().await?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("API error ({}): {}", status, body));
        }

        Ok(response)
    }

    async fn parse_chat_response(
        &self,
        response: reqwest::Response,
        provider_kind: ProviderKind,
    ) -> anyhow::Result<ChatResponse> {
        match provider_kind {
            ProviderKind::AnthropicNative => {
                let anthropic: AnthropicMessageResponse = response.json().await?;
                Ok(anthropic_response_to_chat_response(anthropic))
            }
            ProviderKind::OpenAICompatible | ProviderKind::GeminiCompatible => {
                let chat_response: ChatResponse = response.json().await?;
                Ok(chat_response)
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub r#type: String,
    pub function: ToolFunction,
}

impl ToolDefinition {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
        Self {
            r#type: "function".to_string(),
            function: ToolFunction {
                name: name.into(),
                description: description.into(),
                parameters,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolFunction {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub r#type: String,
    pub function: ToolCallFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallFunction {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: Option<String>,
    #[serde(
        default,
        alias = "reasoningContent",
        skip_serializing_if = "Option::is_none"
    )]
    pub reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiPromptCacheScope {
    None,
    Global,
    Org,
}

#[derive(Debug, Clone)]
pub struct ApiPromptCacheTextBlock {
    pub text: String,
    pub cache_scope: ApiPromptCacheScope,
}

#[derive(Debug, Clone)]
pub struct PromptCacheMetadata {
    pub system_blocks: Vec<ApiPromptCacheTextBlock>,
    pub prepended_user_context_blocks: Vec<ApiPromptCacheTextBlock>,
    pub explicit_tool_cache_breakpoint: bool,
    pub top_level_auto_cache: bool,
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: Some(content.into()),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: Some(content.into()),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn assistant_with_tools(tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: None,
            reasoning_content: None,
            tool_calls: Some(tool_calls),
            tool_call_id: None,
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".to_string(),
            content: Some(content.into()),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn tool(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".to_string(),
            content: Some(content.into()),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    max_tokens: usize,
    stream: bool,
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ToolDefinition>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChatResponse {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub choices: Vec<Choice>,
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Choice {
    pub index: i32,
    pub message: ChatMessage,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Usage {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub total_tokens: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StreamChunk {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub choices: Vec<StreamChoice>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StreamChoice {
    pub index: i32,
    pub delta: Delta,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Delta {
    pub role: Option<String>,
    pub content: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SlotStrategyResponse {
    pub response: ChatResponse,
    pub used_max_tokens: usize,
    pub retried_with_large_slot: bool,
}

#[derive(Debug, Clone, Serialize)]
struct AnthropicMessagesRequest {
    model: String,
    max_tokens: usize,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<Vec<AnthropicSystemBlock>>,
    stream: bool,
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<AnthropicToolDefinition>>,
}

#[derive(Debug, Clone, Serialize)]
struct AnthropicCountTokensRequest {
    model: String,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<Vec<AnthropicSystemBlock>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<AnthropicToolDefinition>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AnthropicCacheControl {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Debug, Clone, Serialize)]
struct AnthropicToolDefinition {
    name: String,
    description: String,
    input_schema: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<AnthropicCacheControl>,
}

#[derive(Debug, Clone, Serialize)]
struct AnthropicSystemBlock {
    #[serde(rename = "type")]
    kind: String,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<AnthropicCacheControl>,
}

#[derive(Debug, Clone, Serialize)]
struct AnthropicMessage {
    role: String,
    content: Vec<AnthropicContentBlock>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicContentBlock {
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<AnthropicCacheControl>,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<AnthropicCacheControl>,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<AnthropicCacheControl>,
    },
}

#[derive(Debug, Clone, Deserialize)]
struct AnthropicCountTokensResponse {
    input_tokens: usize,
}

#[derive(Debug, Clone, Deserialize)]
struct AnthropicMessageResponse {
    id: String,
    model: String,
    #[serde(default)]
    content: Vec<Value>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    usage: Option<AnthropicUsageResponse>,
}

#[derive(Debug, Clone, Deserialize)]
struct AnthropicUsageResponse {
    #[serde(default)]
    input_tokens: usize,
    #[serde(default)]
    output_tokens: usize,
    #[serde(default)]
    cache_creation_input_tokens: Option<usize>,
    #[serde(default)]
    cache_read_input_tokens: Option<usize>,
}

pub type AnthropicClient = ApiClient;

pub fn build_chat_completions_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with("/chat/completions") {
        trimmed.to_string()
    } else if trimmed.ends_with("/v1") {
        format!("{}/chat/completions", trimmed)
    } else {
        format!("{}/v1/chat/completions", trimmed)
    }
}

fn build_anthropic_messages_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with("/messages") {
        trimmed.to_string()
    } else if trimmed.ends_with("/v1") {
        format!("{}/messages", trimmed)
    } else {
        format!("{}/v1/messages", trimmed)
    }
}

fn build_anthropic_count_tokens_url(base_url: &str) -> String {
    format!(
        "{}/count_tokens",
        build_anthropic_messages_url(base_url).trim_end_matches('/')
    )
}

fn build_anthropic_messages_request(
    model: &str,
    messages: Vec<ChatMessage>,
    tools: Option<Vec<ToolDefinition>>,
    max_tokens: usize,
    temperature: f32,
    stream: bool,
    cache_enabled: bool,
    prompt_cache_metadata: Option<&PromptCacheMetadata>,
) -> anyhow::Result<AnthropicMessagesRequest> {
    let auto_cache = cache_enabled
        && prompt_cache_metadata
            .map(|metadata| metadata.top_level_auto_cache)
            .unwrap_or(true);
    let (system, messages) = split_anthropic_prompt(messages, prompt_cache_metadata, auto_cache)?;
    Ok(AnthropicMessagesRequest {
        model: model.to_string(),
        max_tokens,
        messages,
        system,
        stream,
        temperature,
        tools: convert_anthropic_tools(
            tools,
            cache_enabled
                && prompt_cache_metadata
                    .map(|metadata| metadata.explicit_tool_cache_breakpoint)
                    .unwrap_or(false),
        ),
    })
}

fn build_anthropic_count_tokens_request(
    model: &str,
    messages: Vec<ChatMessage>,
    tools: Option<Vec<ToolDefinition>>,
    cache_enabled: bool,
    prompt_cache_metadata: Option<&PromptCacheMetadata>,
) -> anyhow::Result<AnthropicCountTokensRequest> {
    let auto_cache = cache_enabled
        && prompt_cache_metadata
            .map(|metadata| metadata.top_level_auto_cache)
            .unwrap_or(true);
    let (system, messages) = split_anthropic_prompt(messages, prompt_cache_metadata, auto_cache)?;
    Ok(AnthropicCountTokensRequest {
        model: model.to_string(),
        messages,
        system,
        tools: convert_anthropic_tools(
            tools,
            cache_enabled
                && prompt_cache_metadata
                    .map(|metadata| metadata.explicit_tool_cache_breakpoint)
                    .unwrap_or(false),
        ),
    })
}

fn split_anthropic_prompt(
    messages: Vec<ChatMessage>,
    prompt_cache_metadata: Option<&PromptCacheMetadata>,
    auto_cache: bool,
) -> anyhow::Result<(Option<Vec<AnthropicSystemBlock>>, Vec<AnthropicMessage>)> {
    let mut system_blocks = Vec::new();
    let mut anthropic_messages = Vec::new();

    if let Some(metadata) = prompt_cache_metadata {
        system_blocks = metadata
            .system_blocks
            .iter()
            .enumerate()
            .map(|(index, block)| AnthropicSystemBlock {
                kind: "text".to_string(),
                text: block.text.clone(),
                cache_control: system_block_cache_breakpoint(metadata, index),
            })
            .collect::<Vec<_>>();
        if !metadata.prepended_user_context_blocks.is_empty() {
            let content = metadata
                .prepended_user_context_blocks
                .iter()
                .enumerate()
                .map(|(index, block)| AnthropicContentBlock::Text {
                    text: block.text.clone(),
                    cache_control: should_mark_org_cache_block(metadata, index)
                        .then(|| anthropic_cache_breakpoint()),
                })
                .collect::<Vec<_>>();
            anthropic_messages.push(AnthropicMessage {
                role: "user".to_string(),
                content,
            });
        }
    }

    for message in messages {
        match message.role.as_str() {
            "system" => {
                if prompt_cache_metadata.is_some() {
                    continue;
                }
                let text = message.content.unwrap_or_default().trim().to_string();
                if !text.is_empty() {
                    system_blocks.push(AnthropicSystemBlock {
                        kind: "text".to_string(),
                        text,
                        cache_control: None,
                    });
                }
            }
            "user" => {
                let text = message.content.unwrap_or_default();
                if !text.trim().is_empty() {
                    push_anthropic_message(
                        &mut anthropic_messages,
                        "user",
                        AnthropicContentBlock::Text {
                            text,
                            cache_control: None,
                        },
                    );
                }
            }
            "assistant" => {
                if let Some(reasoning) = message.reasoning_content {
                    if !reasoning.trim().is_empty() {
                        push_anthropic_message(
                            &mut anthropic_messages,
                            "assistant",
                            AnthropicContentBlock::Text {
                                text: format!("[thinking]\n{}", reasoning),
                                cache_control: None,
                            },
                        );
                    }
                }

                if let Some(content) = message.content {
                    if !content.trim().is_empty() {
                        push_anthropic_message(
                            &mut anthropic_messages,
                            "assistant",
                            AnthropicContentBlock::Text {
                                text: content,
                                cache_control: None,
                            },
                        );
                    }
                }

                if let Some(tool_calls) = message.tool_calls {
                    for tool_call in tool_calls {
                        let input = parse_tool_arguments_value(&tool_call.function.arguments);
                        push_anthropic_message(
                            &mut anthropic_messages,
                            "assistant",
                            AnthropicContentBlock::ToolUse {
                                id: tool_call.id,
                                name: tool_call.function.name,
                                input,
                                cache_control: None,
                            },
                        );
                    }
                }
            }
            "tool" => {
                let tool_use_id = message.tool_call_id.unwrap_or_default();
                let content = message.content.unwrap_or_default();
                if tool_use_id.trim().is_empty() {
                    if !content.trim().is_empty() {
                        push_anthropic_message(
                            &mut anthropic_messages,
                            "user",
                            AnthropicContentBlock::Text {
                                text: content,
                                cache_control: None,
                            },
                        );
                    }
                } else {
                    push_anthropic_message(
                        &mut anthropic_messages,
                        "user",
                        AnthropicContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            cache_control: None,
                        },
                    );
                }
            }
            _ => {
                if let Some(content) = message.content {
                    if !content.trim().is_empty() {
                        push_anthropic_message(
                            &mut anthropic_messages,
                            "user",
                            AnthropicContentBlock::Text {
                                text: content,
                                cache_control: None,
                            },
                        );
                    }
                }
            }
        }
    }

    if auto_cache {
        apply_auto_cache_breakpoint(&mut system_blocks, &mut anthropic_messages);
    }

    Ok((
        (!system_blocks.is_empty()).then_some(system_blocks),
        anthropic_messages,
    ))
}

fn push_anthropic_message(
    messages: &mut Vec<AnthropicMessage>,
    role: &str,
    block: AnthropicContentBlock,
) {
    if let Some(last_message) = messages.last_mut() {
        if last_message.role == role {
            last_message.content.push(block);
            return;
        }
    }

    messages.push(AnthropicMessage {
        role: role.to_string(),
        content: vec![block],
    });
}

fn convert_anthropic_tools(
    tools: Option<Vec<ToolDefinition>>,
    explicit_cache_breakpoint: bool,
) -> Option<Vec<AnthropicToolDefinition>> {
    tools.map(|tools| {
        let last_index = tools.len().saturating_sub(1);
        tools
            .into_iter()
            .enumerate()
            .map(|(index, tool)| AnthropicToolDefinition {
                name: tool.function.name,
                description: tool.function.description,
                input_schema: tool.function.parameters,
                cache_control: (explicit_cache_breakpoint && index == last_index)
                    .then(|| anthropic_cache_breakpoint()),
            })
            .collect()
    })
}

fn cache_enabled() -> bool {
    !std::env::var("DISABLE_PROMPT_CACHE")
        .map(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

fn anthropic_cache_breakpoint() -> AnthropicCacheControl {
    AnthropicCacheControl {
        kind: "ephemeral".to_string(),
    }
}

fn apply_auto_cache_breakpoint(
    system_blocks: &mut [AnthropicSystemBlock],
    messages: &mut [AnthropicMessage],
) {
    if let Some(last_message) = messages.last_mut() {
        if let Some(last_block) = last_message.content.last_mut() {
            mark_content_block_cache_breakpoint(last_block);
            return;
        }
    }

    if let Some(last_system_block) = system_blocks.last_mut() {
        last_system_block.cache_control = Some(anthropic_cache_breakpoint());
    }
}

fn mark_content_block_cache_breakpoint(block: &mut AnthropicContentBlock) {
    match block {
        AnthropicContentBlock::Text { cache_control, .. }
        | AnthropicContentBlock::ToolUse { cache_control, .. }
        | AnthropicContentBlock::ToolResult { cache_control, .. } => {
            *cache_control = Some(anthropic_cache_breakpoint());
        }
    }
}

fn should_mark_global_cache_block(blocks: &[ApiPromptCacheTextBlock], index: usize) -> bool {
    blocks
        .iter()
        .rposition(|block| block.cache_scope == ApiPromptCacheScope::Global)
        == Some(index)
}

fn should_mark_org_system_cache_block(metadata: &PromptCacheMetadata, index: usize) -> bool {
    metadata.prepended_user_context_blocks.is_empty()
        && metadata
            .system_blocks
            .iter()
            .rposition(|block| block.cache_scope == ApiPromptCacheScope::Org)
            == Some(index)
}

fn should_mark_org_cache_block(metadata: &PromptCacheMetadata, index: usize) -> bool {
    metadata
        .prepended_user_context_blocks
        .iter()
        .rposition(|block| block.cache_scope == ApiPromptCacheScope::Org)
        == Some(index)
}

fn system_block_cache_breakpoint(
    metadata: &PromptCacheMetadata,
    index: usize,
) -> Option<AnthropicCacheControl> {
    if should_mark_global_cache_block(&metadata.system_blocks, index)
        || should_mark_org_system_cache_block(metadata, index)
    {
        Some(anthropic_cache_breakpoint())
    } else {
        None
    }
}

fn parse_tool_arguments_value(arguments: &str) -> Value {
    serde_json::from_str(arguments).unwrap_or_else(|_| json!({ "raw": arguments }))
}

fn anthropic_response_to_chat_response(response: AnthropicMessageResponse) -> ChatResponse {
    let mut answer_chunks = Vec::new();
    let mut reasoning_chunks = Vec::new();
    let mut tool_calls = Vec::new();

    for block in response.content {
        let block_type = block
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match block_type {
            "text" => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    answer_chunks.push(text.to_string());
                }
            }
            "thinking" => {
                if let Some(thinking) = block.get("thinking").and_then(Value::as_str) {
                    reasoning_chunks.push(thinking.to_string());
                }
            }
            "tool_use" => {
                let id = block
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let input = block.get("input").cloned().unwrap_or(Value::Null);
                tool_calls.push(ToolCall {
                    id,
                    r#type: "function".to_string(),
                    function: ToolCallFunction {
                        name,
                        arguments: serde_json::to_string(&input)
                            .unwrap_or_else(|_| "{}".to_string()),
                    },
                });
            }
            _ => {}
        }
    }

    let usage = response.usage.map(|usage| {
        let prompt_tokens = usage.input_tokens
            + usage.cache_creation_input_tokens.unwrap_or_default()
            + usage.cache_read_input_tokens.unwrap_or_default();
        let completion_tokens = usage.output_tokens;
        Usage {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
        }
    });

    ChatResponse {
        id: response.id,
        object: "chat.completion".to_string(),
        created: chrono::Utc::now().timestamp(),
        model: response.model,
        choices: vec![Choice {
            index: 0,
            message: ChatMessage {
                role: "assistant".to_string(),
                content: (!answer_chunks.is_empty()).then(|| answer_chunks.join("")),
                reasoning_content: (!reasoning_chunks.is_empty())
                    .then(|| reasoning_chunks.join("")),
                tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
                tool_call_id: None,
            },
            finish_reason: response.stop_reason,
        }],
        usage,
    }
}

fn should_retry_with_large_output(response: &ChatResponse) -> bool {
    response.choices.iter().any(|choice| {
        matches!(
            choice.finish_reason.as_deref(),
            Some("length" | "max_tokens" | "max_output_tokens")
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token_budget::ProviderKind;

    #[test]
    fn provider_kind_detects_anthropic_and_fallback_urls() {
        assert_eq!(
            provider_kind_for_base_url("https://api.anthropic.com"),
            ProviderKind::AnthropicNative
        );
        assert_eq!(
            provider_kind_for_base_url("https://api.openai.com/v1"),
            ProviderKind::OpenAICompatible
        );
        assert_eq!(
            provider_kind_for_base_url("https://generativelanguage.googleapis.com"),
            ProviderKind::GeminiCompatible
        );
    }

    #[test]
    fn anthropic_count_tokens_request_serializes_system_messages_and_cache_control() {
        let request = build_anthropic_count_tokens_request(
            "claude-sonnet-4-20250514",
            vec![
                ChatMessage::system("system"),
                ChatMessage::user("hello"),
                ChatMessage::assistant_with_tools(vec![ToolCall {
                    id: "call_1".to_string(),
                    r#type: "function".to_string(),
                    function: ToolCallFunction {
                        name: "search".to_string(),
                        arguments: r#"{"path":"src"}"#.to_string(),
                    },
                }]),
            ],
            Some(vec![ToolDefinition::new(
                "search",
                "Find text",
                json!({"type":"object","properties":{"path":{"type":"string"}}}),
            )]),
            true,
            None,
        )
        .expect("count tokens request");

        let serialized = serde_json::to_value(&request).expect("serialized request");
        assert_eq!(
            serialized["system"][0]["text"],
            Value::String("system".to_string())
        );
        assert_eq!(serialized["messages"][0]["role"], "user");
        assert_eq!(serialized["messages"][1]["content"][0]["type"], "tool_use");
        assert_eq!(serialized["tools"][0]["name"], "search");
        assert_eq!(
            serialized["messages"][1]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
    }

    #[test]
    fn anthropic_request_applies_explicit_cache_breakpoints_from_metadata() {
        let metadata = PromptCacheMetadata {
            system_blocks: vec![
                ApiPromptCacheTextBlock {
                    text: "global rules".to_string(),
                    cache_scope: ApiPromptCacheScope::Global,
                },
                ApiPromptCacheTextBlock {
                    text: "dynamic runtime".to_string(),
                    cache_scope: ApiPromptCacheScope::None,
                },
            ],
            prepended_user_context_blocks: vec![ApiPromptCacheTextBlock {
                text: "project memory".to_string(),
                cache_scope: ApiPromptCacheScope::Org,
            }],
            explicit_tool_cache_breakpoint: true,
            top_level_auto_cache: true,
        };

        let request = build_anthropic_messages_request(
            "claude-sonnet-4-20250514",
            vec![ChatMessage::user("latest question")],
            Some(vec![ToolDefinition::new(
                "search",
                "Find text",
                json!({"type":"object","properties":{"path":{"type":"string"}}}),
            )]),
            1024,
            0.0,
            false,
            true,
            Some(&metadata),
        )
        .expect("anthropic request");

        let serialized = serde_json::to_value(&request).expect("serialized request");
        assert_eq!(serialized["tools"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(
            serialized["system"][0]["cache_control"]["type"],
            "ephemeral"
        );
        assert_eq!(serialized["messages"][0]["role"], "user");
        assert_eq!(
            serialized["messages"][0]["content"][0]["text"],
            "project memory"
        );
        assert_eq!(
            serialized["messages"][0]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
        assert_eq!(
            serialized["messages"][0]["content"][1]["text"],
            "latest question"
        );
        assert_eq!(
            serialized["messages"][0]["content"][1]["cache_control"]["type"],
            "ephemeral"
        );
    }

    #[test]
    fn anthropic_response_parses_text_reasoning_tool_use_and_usage() {
        let response = AnthropicMessageResponse {
            id: "msg_1".to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            content: vec![
                json!({"type":"thinking","thinking":"Need more context. "}),
                json!({"type":"text","text":"Here is the answer. "}),
                json!({"type":"tool_use","id":"toolu_1","name":"search","input":{"path":"src","pattern":"auth"}}),
            ],
            stop_reason: Some("tool_use".to_string()),
            usage: Some(AnthropicUsageResponse {
                input_tokens: 200,
                output_tokens: 35,
                cache_creation_input_tokens: Some(12),
                cache_read_input_tokens: Some(8),
            }),
        };

        let parsed = anthropic_response_to_chat_response(response);
        let choice = &parsed.choices[0];

        assert_eq!(
            choice.message.content.as_deref(),
            Some("Here is the answer. ")
        );
        assert_eq!(
            choice.message.reasoning_content.as_deref(),
            Some("Need more context. ")
        );
        assert_eq!(choice.message.tool_calls.as_ref().map(Vec::len), Some(1));
        assert_eq!(parsed.usage.unwrap().total_tokens, 255);
    }

    #[test]
    fn slot_retry_only_triggers_on_length_finish_reasons() {
        let length_response = ChatResponse {
            id: "resp_1".to_string(),
            object: "chat.completion".to_string(),
            created: 0,
            model: "claude-sonnet-4".to_string(),
            choices: vec![Choice {
                index: 0,
                message: ChatMessage::assistant("partial"),
                finish_reason: Some("length".to_string()),
            }],
            usage: None,
        };
        let stop_response = ChatResponse {
            choices: vec![Choice {
                finish_reason: Some("stop".to_string()),
                ..length_response.choices[0].clone()
            }],
            ..length_response.clone()
        };

        assert!(should_retry_with_large_output(&length_response));
        assert!(!should_retry_with_large_output(&stop_response));
    }
}
