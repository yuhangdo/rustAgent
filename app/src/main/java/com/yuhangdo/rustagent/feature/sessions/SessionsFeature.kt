package com.yuhangdo.rustagent.feature.sessions

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.yuhangdo.rustagent.data.repository.RunRepository
import com.yuhangdo.rustagent.data.repository.SelectedSessionRepository
import com.yuhangdo.rustagent.data.repository.SessionRepository
import com.yuhangdo.rustagent.model.AgentRun
import com.yuhangdo.rustagent.model.AgentRunStatus
import com.yuhangdo.rustagent.model.ChatSession
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch

data class SessionRunSummaryUiState(
    val status: AgentRunStatus,
    val durationMs: Long?,
    val model: String,
    val errorSummary: String?,
)

data class SessionsUiState(
    val sessions: List<ChatSession> = emptyList(),
    val selectedSessionId: String? = null,
    val runSummaryBySessionId: Map<String, SessionRunSummaryUiState> = emptyMap(),
)

sealed interface SessionsAction {
    data object CreateClicked : SessionsAction
    data class SessionSelected(val sessionId: String) : SessionsAction
    data class DeleteClicked(val sessionId: String) : SessionsAction
}

class SessionsViewModel(
    private val sessionRepository: SessionRepository,
    private val runRepository: RunRepository,
    private val selectedSessionRepository: SelectedSessionRepository,
) : ViewModel() {
    val uiState: StateFlow<SessionsUiState> = combine(
        sessionRepository.observeSessions(),
        selectedSessionRepository.observeSelectedSessionId(),
        runRepository.observeAllRuns(),
    ) { sessions, selectedSessionId, runs ->
        SessionsUiState(
            sessions = sessions,
            selectedSessionId = selectedSessionId,
            runSummaryBySessionId = runs.toLatestRunSummaries(),
        )
    }.stateIn(
        scope = viewModelScope,
        started = SharingStarted.Eagerly,
        initialValue = SessionsUiState(),
    )

    fun onAction(action: SessionsAction) {
        when (action) {
            SessionsAction.CreateClicked -> createSession()
            is SessionsAction.SessionSelected -> selectSession(action.sessionId)
            is SessionsAction.DeleteClicked -> deleteSession(action.sessionId)
        }
    }

    private fun createSession() {
        viewModelScope.launch {
            val sessionId = sessionRepository.createSession()
            selectedSessionRepository.selectSession(sessionId)
        }
    }

    private fun selectSession(sessionId: String) {
        viewModelScope.launch {
            selectedSessionRepository.selectSession(sessionId)
        }
    }

    private fun deleteSession(sessionId: String) {
        viewModelScope.launch {
            val nextSelection = uiState.value.sessions.firstOrNull { it.id != sessionId }?.id
            sessionRepository.deleteSession(sessionId)
            if (uiState.value.selectedSessionId == sessionId) {
                selectedSessionRepository.selectSession(nextSelection)
            }
        }
    }
}

private fun List<AgentRun>.toLatestRunSummaries(): Map<String, SessionRunSummaryUiState> {
    val summaries = linkedMapOf<String, SessionRunSummaryUiState>()
    for (run in this) {
        if (summaries.containsKey(run.sessionId)) {
            continue
        }
        summaries[run.sessionId] = SessionRunSummaryUiState(
            status = run.status,
            durationMs = run.durationMs,
            model = run.model,
            errorSummary = run.errorSummary,
        )
    }
    return summaries
}

@Composable
fun SessionsScreen(
    uiState: SessionsUiState,
    onAction: (SessionsAction) -> Unit,
    onOpenChat: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier
            .fillMaxSize()
            .padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Column {
                Text(
                    text = "Sessions",
                    style = MaterialTheme.typography.titleLarge,
                    fontWeight = FontWeight.SemiBold,
                )
                Text(
                    text = "Switch conversations and inspect the latest run health for each thread.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Button(onClick = { onAction(SessionsAction.CreateClicked) }) {
                Text("New Session")
            }
        }

        if (uiState.sessions.isEmpty()) {
            Card(modifier = Modifier.fillMaxWidth()) {
                Text(
                    text = "No sessions yet. Create one here or send your first message on the Chat tab.",
                    modifier = Modifier.padding(20.dp),
                    style = MaterialTheme.typography.bodyMedium,
                )
            }
        } else {
            LazyColumn(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                items(
                    items = uiState.sessions,
                    key = { session -> session.id },
                ) { session ->
                    SessionCard(
                        session = session,
                        runSummary = uiState.runSummaryBySessionId[session.id],
                        isSelected = session.id == uiState.selectedSessionId,
                        onSelect = {
                            onAction(SessionsAction.SessionSelected(session.id))
                            onOpenChat()
                        },
                        onDelete = { onAction(SessionsAction.DeleteClicked(session.id)) },
                    )
                }
            }
        }
    }
}

@Composable
private fun SessionCard(
    session: ChatSession,
    runSummary: SessionRunSummaryUiState?,
    isSelected: Boolean,
    onSelect: () -> Unit,
    onDelete: () -> Unit,
) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    text = session.title,
                    style = MaterialTheme.typography.titleMedium,
                    fontWeight = FontWeight.SemiBold,
                )
                if (isSelected) {
                    Text(
                        text = "Active",
                        color = MaterialTheme.colorScheme.primary,
                        style = MaterialTheme.typography.labelLarge,
                    )
                }
            }

            Text(
                text = session.lastPreview.ifBlank { "No messages yet." },
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            Text(
                text = "${session.messageCount} messages",
                style = MaterialTheme.typography.labelMedium,
            )

            if (runSummary != null) {
                val durationLabel = runSummary.durationMs?.let { " | ${it}ms" }.orEmpty()
                Text(
                    text = "Last run: ${runSummary.status.displayName}$durationLabel",
                    style = MaterialTheme.typography.labelLarge,
                    color = when (runSummary.status) {
                        AgentRunStatus.FAILED -> MaterialTheme.colorScheme.error
                        AgentRunStatus.RUNNING -> MaterialTheme.colorScheme.primary
                        AgentRunStatus.COMPLETED -> MaterialTheme.colorScheme.primary
                        AgentRunStatus.CANCELLED -> MaterialTheme.colorScheme.onSurfaceVariant
                    },
                )
                Text(
                    text = "Model: ${runSummary.model}",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                if (runSummary.errorSummary != null) {
                    Text(
                        text = runSummary.errorSummary,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.error,
                    )
                }
            }

            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(onClick = onSelect) {
                    Text("Open")
                }
                OutlinedButton(onClick = onDelete) {
                    Text("Delete")
                }
            }
        }
    }
}
