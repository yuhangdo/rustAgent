package com.yuhangdo.rustagent.model

const val DEFAULT_SYSTEM_PROMPT =
    "You are Rust Agent Mobile. Keep reasoning concise and make the final answer actionable."

enum class MessageRole {
    USER,
    ASSISTANT,
    SYSTEM,
}

enum class ProviderType(val displayName: String) {
    FAKE("Fake Provider"),
    OPENAI_COMPATIBLE("OpenAI-Compatible"),
}

val MessageRole.apiValue: String
    get() = when (this) {
        MessageRole.USER -> "user"
        MessageRole.ASSISTANT -> "assistant"
        MessageRole.SYSTEM -> "system"
    }

data class ChatMessage(
    val id: String,
    val sessionId: String,
    val role: MessageRole,
    val reasoningContent: String,
    val answerContent: String,
    val createdAt: Long,
)

data class ChatSession(
    val id: String,
    val title: String,
    val createdAt: Long,
    val updatedAt: Long,
    val lastPreview: String,
    val messageCount: Int,
)

data class ProviderSettings(
    val providerType: ProviderType = ProviderType.FAKE,
    val baseUrl: String = "",
    val apiKey: String = "",
    val model: String = "gpt-4o-mini",
    val systemPrompt: String = DEFAULT_SYSTEM_PROMPT,
)

data class ProviderRequest(
    val history: List<ChatMessage>,
    val settings: ProviderSettings,
)

data class ProviderChunk(
    val reasoningDelta: String = "",
    val answerDelta: String = "",
)

fun suggestedSessionTitle(input: String): String {
    val normalized = input.replace("\n", " ").trim()
    if (normalized.isBlank()) {
        return "New Chat"
    }
    return normalized.take(32)
}

