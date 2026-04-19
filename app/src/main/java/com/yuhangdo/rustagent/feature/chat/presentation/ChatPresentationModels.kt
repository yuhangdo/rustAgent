package com.yuhangdo.rustagent.feature.chat

import com.yuhangdo.rustagent.model.AgentRunStatus
import com.yuhangdo.rustagent.model.MessageRole

data class ChatMessagePresentation(
    val id: String,
    val role: MessageRole,
    val answerContent: String,
    val reasoningPreview: String,
    val providerLabel: String? = null,
    val runId: String? = null,
    val runStatus: AgentRunStatus? = null,
    val durationMs: Long? = null,
    val errorSummary: String? = null,
    val isStreaming: Boolean = false,
)

data class DeepThinkingPanelState(
    val runId: String,
    val providerLabel: String,
    val status: AgentRunStatus,
    val durationMs: Long?,
    val errorSummary: String?,
    val items: List<RunTraceItem>,
)

sealed interface RunTraceItem {
    data class TimelineEntry(
        val id: String,
        val title: String,
        val details: String,
    ) : RunTraceItem
}
