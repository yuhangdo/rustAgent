package com.yuhangdo.rustagent.data.runtime

import com.yuhangdo.rustagent.model.ChatMessage
import com.yuhangdo.rustagent.model.ProviderSettings
import com.yuhangdo.rustagent.model.RunEventType
import kotlinx.coroutines.flow.Flow

data class AgentRuntimeRequest(
    val runId: String,
    val sessionId: String,
    val triggerLabel: String,
    val history: List<ChatMessage>,
    val settings: ProviderSettings,
)

sealed interface AgentRuntimeEvent {
    data class RunUpdate(
        val type: RunEventType,
        val details: String,
        val title: String = type.displayName,
    ) : AgentRuntimeEvent

    data class OutputUpdate(
        val reasoningContent: String,
        val answerContent: String,
    ) : AgentRuntimeEvent
}

interface AgentRuntime {
    fun execute(request: AgentRuntimeRequest): Flow<AgentRuntimeEvent>

    suspend fun cancel(runId: String) = Unit
}

fun interface AgentRuntimeResolver {
    fun resolve(settings: ProviderSettings): AgentRuntime
}
