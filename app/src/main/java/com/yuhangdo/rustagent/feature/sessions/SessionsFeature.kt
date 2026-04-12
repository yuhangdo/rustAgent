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
import com.yuhangdo.rustagent.data.repository.SelectedSessionRepository
import com.yuhangdo.rustagent.data.repository.SessionRepository
import com.yuhangdo.rustagent.model.ChatSession
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch

data class SessionsUiState(
    val sessions: List<ChatSession> = emptyList(),
    val selectedSessionId: String? = null,
)

sealed interface SessionsAction {
    data object CreateClicked : SessionsAction
    data class SessionSelected(val sessionId: String) : SessionsAction
    data class DeleteClicked(val sessionId: String) : SessionsAction
}

class SessionsViewModel(
    private val sessionRepository: SessionRepository,
    private val selectedSessionRepository: SelectedSessionRepository,
) : ViewModel() {
    val uiState: StateFlow<SessionsUiState> = combine(
        sessionRepository.observeSessions(),
        selectedSessionRepository.observeSelectedSessionId(),
    ) { sessions, selectedSessionId ->
        SessionsUiState(
            sessions = sessions,
            selectedSessionId = selectedSessionId,
        )
    }.stateIn(
        scope = viewModelScope,
        started = SharingStarted.WhileSubscribed(5_000),
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
                    text = "Switch conversations, review previews, or open a fresh thread.",
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
