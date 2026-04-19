package com.yuhangdo.rustagent.feature.sessions

import com.yuhangdo.rustagent.core.ui.mvi.UiEffect
import com.yuhangdo.rustagent.core.ui.mvi.UiIntent
import com.yuhangdo.rustagent.core.ui.mvi.UiState

object SessionsContract {
    data class State(
        val sessions: List<SessionSummaryPresentation> = emptyList(),
        val selectedSessionId: String? = null,
    ) : UiState

    sealed interface Intent : UiIntent {
        data object CreateSession : Intent
        data class SelectSession(val sessionId: String) : Intent
        data class DeleteSession(val sessionId: String) : Intent
    }

    sealed interface Effect : UiEffect {
        data class OpenChat(val sessionId: String) : Effect
        data class ShowSnackbar(val message: String) : Effect
    }

    sealed interface Mutation {
        data class SnapshotLoaded(
            val sessions: List<SessionSummaryPresentation>,
            val selectedSessionId: String?,
        ) : Mutation
    }
}

data class SessionSummaryPresentation(
    val id: String,
    val title: String,
    val preview: String,
    val messageCountLabel: String,
    val isSelected: Boolean,
    val lastRunStatusLabel: String?,
    val modelLabel: String?,
    val errorSummary: String?,
)
