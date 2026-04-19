package com.yuhangdo.rustagent.feature.chat

import com.yuhangdo.rustagent.core.ui.mvi.UiEffect
import com.yuhangdo.rustagent.core.ui.mvi.UiIntent
import com.yuhangdo.rustagent.core.ui.mvi.UiState

object ChatContract {
    data class State(
        val sessionId: String? = null,
        val sessionTitle: String = "New Chat",
        val messages: List<ChatMessagePresentation> = emptyList(),
        val draftMessage: String = "",
        val isSending: Boolean = false,
        val providerTypeLabel: String = "Fake Provider",
        val errorMessage: String? = null,
        val activeRunCount: Int = 0,
        val deepThinkingPanel: DeepThinkingPanelState? = null,
    ) : UiState

    sealed interface Intent : UiIntent {
        data class DraftChanged(val value: String) : Intent
        data object SendClicked : Intent
        data object DismissError : Intent
        data class OpenDeepThinking(val runId: String) : Intent
        data object CloseDeepThinking : Intent
        data class RetryRun(val runId: String) : Intent
        data class CancelRun(val runId: String) : Intent
    }

    sealed interface Effect : UiEffect {
        data class ShowSnackbar(val message: String) : Effect
    }

    sealed interface Mutation {
        data class SnapshotLoaded(
            val sessionId: String?,
            val sessionTitle: String,
            val messages: List<ChatMessagePresentation>,
            val providerTypeLabel: String,
            val activeRunCount: Int,
        ) : Mutation

        data class DraftChanged(val value: String) : Mutation
        data class SendingChanged(val value: Boolean) : Mutation
        data class ErrorChanged(val value: String?) : Mutation
        data class DeepThinkingChanged(val panel: DeepThinkingPanelState?) : Mutation
    }
}
