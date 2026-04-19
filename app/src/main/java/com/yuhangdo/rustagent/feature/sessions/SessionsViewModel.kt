package com.yuhangdo.rustagent.feature.sessions

import androidx.lifecycle.viewModelScope
import com.yuhangdo.rustagent.core.ui.mvi.MviViewModel
import com.yuhangdo.rustagent.data.repository.RunRepository
import com.yuhangdo.rustagent.data.repository.SelectedSessionRepository
import com.yuhangdo.rustagent.data.repository.SessionRepository
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.launch

class SessionsViewModel(
    private val sessionRepository: SessionRepository,
    private val runRepository: RunRepository,
    private val selectedSessionRepository: SelectedSessionRepository,
) : MviViewModel<
    SessionsContract.Intent,
    SessionsContract.State,
    SessionsContract.Effect,
    SessionsContract.Mutation
>(
    initialState = SessionsContract.State(),
    reducer = SessionsReducer(),
) {
    init {
        observeSessions()
    }

    override fun handleIntent(intent: SessionsContract.Intent) {
        when (intent) {
            SessionsContract.Intent.CreateSession -> createSession()
            is SessionsContract.Intent.SelectSession -> selectSession(intent.sessionId)
            is SessionsContract.Intent.DeleteSession -> deleteSession(intent.sessionId)
        }
    }

    private fun observeSessions() {
        viewModelScope.launch {
            combine(
                sessionRepository.observeSessions(),
                selectedSessionRepository.observeSelectedSessionId(),
                runRepository.observeAllRuns(),
            ) { sessions, selectedSessionId, runs ->
                SessionsContract.Mutation.SnapshotLoaded(
                    sessions = sessions.map { session ->
                        val lastRun = runs.firstOrNull { it.sessionId == session.id }
                        SessionSummaryPresentation(
                            id = session.id,
                            title = session.title,
                            preview = session.lastPreview.ifBlank { "No messages yet." },
                            messageCountLabel = "${session.messageCount} messages",
                            isSelected = session.id == selectedSessionId,
                            lastRunStatusLabel = lastRun?.status?.displayName?.let { status ->
                                lastRun.durationMs?.let { "$status | ${it}ms" } ?: status
                            },
                            modelLabel = lastRun?.model,
                            errorSummary = lastRun?.errorSummary,
                        )
                    },
                    selectedSessionId = selectedSessionId,
                )
            }.collect(::mutate)
        }
    }

    private fun createSession() {
        viewModelScope.launch {
            val sessionId = sessionRepository.createSession()
            selectedSessionRepository.selectSession(sessionId)
            launchEffect(SessionsContract.Effect.OpenChat(sessionId))
        }
    }

    private fun selectSession(sessionId: String) {
        viewModelScope.launch {
            selectedSessionRepository.selectSession(sessionId)
            launchEffect(SessionsContract.Effect.OpenChat(sessionId))
        }
    }

    private fun deleteSession(sessionId: String) {
        viewModelScope.launch {
            val nextSelection = uiState.value.sessions.firstOrNull { it.id != sessionId }?.id
            sessionRepository.deleteSession(sessionId)
            if (uiState.value.selectedSessionId == sessionId) {
                selectedSessionRepository.selectSession(nextSelection)
                nextSelection?.let { launchEffect(SessionsContract.Effect.OpenChat(it)) }
            }
            launchEffect(SessionsContract.Effect.ShowSnackbar("Session deleted."))
        }
    }
}
