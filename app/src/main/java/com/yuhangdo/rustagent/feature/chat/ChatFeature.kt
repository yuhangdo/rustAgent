package com.yuhangdo.rustagent.feature.chat

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.weight
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material3.AssistChip
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.yuhangdo.rustagent.data.provider.ChatProviderResolver
import com.yuhangdo.rustagent.data.repository.ChatRepository
import com.yuhangdo.rustagent.data.repository.SelectedSessionRepository
import com.yuhangdo.rustagent.data.repository.SessionRepository
import com.yuhangdo.rustagent.data.repository.SettingsRepository
import com.yuhangdo.rustagent.model.ChatMessage
import com.yuhangdo.rustagent.model.MessageRole
import com.yuhangdo.rustagent.model.ProviderRequest
import com.yuhangdo.rustagent.model.ProviderType
import com.yuhangdo.rustagent.model.suggestedSessionTitle
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.flatMapLatest
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch

data class ChatUiState(
    val sessionId: String? = null,
    val sessionTitle: String = "New Chat",
    val messages: List<ChatMessage> = emptyList(),
    val draftMessage: String = "",
    val isSending: Boolean = false,
    val providerType: ProviderType = ProviderType.FAKE,
    val errorMessage: String? = null,
)

sealed interface ChatAction {
    data class DraftChanged(val value: String) : ChatAction
    data object SendClicked : ChatAction
    data object ErrorDismissed : ChatAction
}

class ChatViewModel(
    private val chatRepository: ChatRepository,
    private val sessionRepository: SessionRepository,
    private val settingsRepository: SettingsRepository,
    private val selectedSessionRepository: SelectedSessionRepository,
    private val providerResolver: ChatProviderResolver,
) : ViewModel() {
    private val draftMessage = MutableStateFlow("")
    private val isSending = MutableStateFlow(false)
    private val errorMessage = MutableStateFlow<String?>(null)

    private val selectedSessionId = selectedSessionRepository.observeSelectedSessionId()
    private val sessions = sessionRepository.observeSessions()
    private val messages = selectedSessionId.flatMapLatest { sessionId ->
        if (sessionId == null) {
            flowOf(emptyList<ChatMessage>())
        } else {
            chatRepository.observeMessages(sessionId)
        }
    }

    val uiState: StateFlow<ChatUiState> = combine(
        selectedSessionId,
        sessions,
        messages,
        settingsRepository.observeSettings(),
        draftMessage,
        isSending,
        errorMessage,
    ) { selectedId, sessionList, messageList, settings, draft, sending, error ->
        val activeSession = sessionList.find { it.id == selectedId }
        ChatUiState(
            sessionId = selectedId,
            sessionTitle = activeSession?.title ?: "New Chat",
            messages = messageList,
            draftMessage = draft,
            isSending = sending,
            providerType = settings.providerType,
            errorMessage = error,
        )
    }.stateIn(
        scope = viewModelScope,
        started = SharingStarted.WhileSubscribed(5_000),
        initialValue = ChatUiState(),
    )

    fun onAction(action: ChatAction) {
        when (action) {
            is ChatAction.DraftChanged -> draftMessage.value = action.value
            ChatAction.SendClicked -> sendMessage()
            ChatAction.ErrorDismissed -> errorMessage.value = null
        }
    }

    private fun sendMessage() {
        val messageContent = draftMessage.value.trim()
        if (messageContent.isBlank() || isSending.value) {
            return
        }

        viewModelScope.launch {
            draftMessage.value = ""
            isSending.value = true
            errorMessage.value = null

            val sessionId = selectedSessionRepository.currentSelectedSessionId()
                ?: sessionRepository.createSession(suggestedSessionTitle(messageContent)).also {
                    selectedSessionRepository.selectSession(it)
                }

            chatRepository.addUserMessage(sessionId, messageContent)
            val history = chatRepository.getMessages(sessionId)
            val assistantMessageId = chatRepository.createAssistantPlaceholder(sessionId)
            val settings = settingsRepository.getSettings()
            val provider = providerResolver.resolve(settings)

            var accumulatedReasoning = ""
            var accumulatedAnswer = ""

            try {
                provider.streamReply(
                    ProviderRequest(
                        history = history,
                        settings = settings,
                    ),
                ).collect { chunk ->
                    accumulatedReasoning += chunk.reasoningDelta
                    accumulatedAnswer += chunk.answerDelta
                    chatRepository.updateAssistantMessage(
                        messageId = assistantMessageId,
                        reasoningContent = accumulatedReasoning,
                        answerContent = accumulatedAnswer,
                    )
                }

                if (accumulatedReasoning.isBlank() && accumulatedAnswer.isBlank()) {
                    chatRepository.updateAssistantMessage(
                        messageId = assistantMessageId,
                        reasoningContent = "",
                        answerContent = "Provider returned an empty response.",
                    )
                }
            } catch (throwable: Throwable) {
                val providerError = throwable.message ?: "Unknown provider error."
                errorMessage.value = providerError
                chatRepository.updateAssistantMessage(
                    messageId = assistantMessageId,
                    reasoningContent = accumulatedReasoning,
                    answerContent = accumulatedAnswer.ifBlank { "Provider error: $providerError" },
                )
            } finally {
                isSending.value = false
            }
        }
    }
}

@Composable
fun ChatScreen(
    uiState: ChatUiState,
    onAction: (ChatAction) -> Unit,
    modifier: Modifier = Modifier,
) {
    val listState = rememberLazyListState()

    LaunchedEffect(uiState.messages.size) {
        if (uiState.messages.isNotEmpty()) {
            listState.animateScrollToItem(uiState.messages.lastIndex)
        }
    }

    Surface(
        modifier = modifier
            .fillMaxSize()
            .background(
                Brush.verticalGradient(
                    colors = listOf(
                        MaterialTheme.colorScheme.surface,
                        MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.35f),
                    ),
                ),
            ),
    ) {
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.SpaceBetween,
            ) {
                Column {
                    Text(
                        text = uiState.sessionTitle,
                        style = MaterialTheme.typography.titleLarge,
                        fontWeight = FontWeight.SemiBold,
                    )
                    Text(
                        text = "reasoningContent + answerContent stay separated in storage and UI.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                AssistChip(
                    onClick = { },
                    label = { Text(uiState.providerType.displayName) },
                )
            }

            if (uiState.errorMessage != null) {
                Text(
                    text = uiState.errorMessage,
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodyMedium,
                )
            }

            if (uiState.messages.isEmpty()) {
                Card(modifier = Modifier.weight(1f)) {
                    Column(
                        modifier = Modifier.padding(20.dp),
                        verticalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        Text(
                            text = "Start a runnable Android session",
                            style = MaterialTheme.typography.titleMedium,
                            fontWeight = FontWeight.SemiBold,
                        )
                        Text(
                            text = "Messages persist in Room. Assistant replies keep reasoning and final answer split into two fields.",
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            } else {
                LazyColumn(
                    state = listState,
                    modifier = Modifier.weight(1f),
                    verticalArrangement = Arrangement.spacedBy(10.dp),
                ) {
                    items(
                        items = uiState.messages,
                        key = { message -> message.id },
                    ) { message ->
                        MessageCard(message)
                    }
                }
            }

            OutlinedTextField(
                value = uiState.draftMessage,
                onValueChange = { onAction(ChatAction.DraftChanged(it)) },
                modifier = Modifier.fillMaxWidth(),
                minLines = 3,
                maxLines = 5,
                label = { Text("Ask the agent") },
                placeholder = { Text("Describe the Android task, bug, or provider behavior...") },
            )

            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.End,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                if (uiState.isSending) {
                    CircularProgressIndicator(
                        modifier = Modifier.size(22.dp),
                        strokeWidth = 2.dp,
                    )
                    Spacer(modifier = Modifier.size(12.dp))
                }
                Button(
                    onClick = { onAction(ChatAction.SendClicked) },
                    enabled = uiState.draftMessage.isNotBlank() && !uiState.isSending,
                ) {
                    Text("Send")
                }
            }
        }
    }
}

@Composable
private fun MessageCard(message: ChatMessage) {
    val roleLabel = when (message.role) {
        MessageRole.USER -> "You"
        MessageRole.ASSISTANT -> "Agent"
        MessageRole.SYSTEM -> "System"
    }

    Card(modifier = Modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                text = roleLabel,
                style = MaterialTheme.typography.labelLarge,
                color = MaterialTheme.colorScheme.primary,
            )

            if (message.reasoningContent.isNotBlank()) {
                Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    Text(
                        text = "Reasoning",
                        style = MaterialTheme.typography.labelMedium,
                        fontWeight = FontWeight.Bold,
                    )
                    Text(
                        text = message.reasoningContent,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }

            if (message.answerContent.isNotBlank()) {
                Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    Text(
                        text = "Answer",
                        style = MaterialTheme.typography.labelMedium,
                        fontWeight = FontWeight.Bold,
                    )
                    Text(
                        text = message.answerContent,
                        style = MaterialTheme.typography.bodyLarge,
                    )
                }
            }

            if (message.answerContent.isBlank() && message.reasoningContent.isBlank()) {
                Text(
                    text = "Waiting for provider output...",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}
