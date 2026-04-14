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

enum class FakeProviderScenario(
    val displayName: String,
    val description: String,
) {
    SUCCESS_WITH_REASONING(
        displayName = "Success + Reasoning",
        description = "Returns both a reasoning summary and a final answer.",
    ),
    SUCCESS_ANSWER_ONLY(
        displayName = "Answer Only",
        description = "Returns a final answer without reasoning text.",
    ),
    EMPTY_RESPONSE(
        displayName = "Empty Response",
        description = "Returns no content so the console can surface an empty provider result.",
    ),
    DELAYED_SUCCESS(
        displayName = "Delayed Success",
        description = "Returns a successful answer after an artificial delay.",
    ),
    PROVIDER_ERROR(
        displayName = "Provider Error",
        description = "Throws a synthetic provider error for failure-state testing.",
    ),
}

enum class AgentRunStatus(val displayName: String) {
    RUNNING("Running"),
    COMPLETED("Done"),
    FAILED("Failed"),
}

enum class RunEventType(val displayName: String) {
    STARTED("Started"),
    REQUEST_BUILT("Prompt Built"),
    PROVIDER_SELECTED("Provider Selected"),
    REASONING_SUMMARY("Reasoning Summary"),
    ANSWER_RECEIVED("Answer Received"),
    COMPLETED("Completed"),
    FAILED("Failed"),
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
    val fakeScenario: FakeProviderScenario = FakeProviderScenario.SUCCESS_WITH_REASONING,
)

data class ProviderRequest(
    val history: List<ChatMessage>,
    val settings: ProviderSettings,
)

data class ProviderChunk(
    val reasoningDelta: String = "",
    val answerDelta: String = "",
)

data class AgentRun(
    val id: String,
    val sessionId: String,
    val userMessageId: String,
    val assistantMessageId: String,
    val status: AgentRunStatus,
    val providerType: ProviderType,
    val model: String,
    val baseUrlSnapshot: String,
    val startedAt: Long,
    val completedAt: Long?,
    val durationMs: Long?,
    val errorSummary: String?,
)

data class RunEvent(
    val id: String,
    val runId: String,
    val type: RunEventType,
    val title: String,
    val details: String,
    val createdAt: Long,
    val orderIndex: Int,
)

fun suggestedSessionTitle(input: String): String {
    val normalized = input.replace("\n", " ").trim()
    if (normalized.isBlank()) {
        return "New Chat"
    }
    return normalized.take(32)
}

fun summarizeReasoning(input: String, maxChars: Int = 180): String {
    val normalized = input.replace(Regex("\\s+"), " ").trim()
    if (normalized.length <= maxChars) {
        return normalized
    }
    return normalized.take(maxChars).trimEnd() + "..."
}

