package com.yuhangdo.rustagent.feature.chat

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
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
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.yuhangdo.rustagent.data.repository.ChatRepository
import com.yuhangdo.rustagent.data.repository.RunRepository
import com.yuhangdo.rustagent.data.repository.SelectedSessionRepository
import com.yuhangdo.rustagent.data.repository.SessionRepository
import com.yuhangdo.rustagent.data.repository.SettingsRepository
import com.yuhangdo.rustagent.data.runtime.AgentRuntimeEvent
import com.yuhangdo.rustagent.data.runtime.AgentRuntimeRequest
import com.yuhangdo.rustagent.data.runtime.AgentRuntimeResolver
import com.yuhangdo.rustagent.model.AgentRun
import com.yuhangdo.rustagent.model.AgentRunStatus
import com.yuhangdo.rustagent.model.ChatMessage
import com.yuhangdo.rustagent.model.MessageRole
import com.yuhangdo.rustagent.model.ProviderSettings
import com.yuhangdo.rustagent.model.ProviderType
import com.yuhangdo.rustagent.model.RunEvent
import com.yuhangdo.rustagent.model.RunEventType
import com.yuhangdo.rustagent.model.suggestedSessionTitle
import com.yuhangdo.rustagent.model.summarizeReasoning
import kotlinx.coroutines.flow.collect
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.flatMapLatest
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch

data class MessageRunSummaryUiState(
    val runId: String,
    val status: AgentRunStatus,
    val durationMs: Long?,
    val errorSummary: String?,
    val providerLabel: String,
)

data class RunInspectorUiState(
    val run: AgentRun,
    val events: List<RunEvent>,
)

data class ChatUiState(
    val sessionId: String? = null,
    val sessionTitle: String = "New Chat",
    val messages: List<ChatMessage> = emptyList(),
    val draftMessage: String = "",
    val isSending: Boolean = false,
    val providerType: ProviderType = ProviderType.FAKE,
    val errorMessage: String? = null,
    val runSummariesByAssistantMessageId: Map<String, MessageRunSummaryUiState> = emptyMap(),
    val selectedRun: RunInspectorUiState? = null,
    val activeRunCount: Int = 0,
)

sealed interface ChatAction {
    data class DraftChanged(val value: String) : ChatAction
    data object SendClicked : ChatAction
    data object ErrorDismissed : ChatAction
    data class RunSelected(val runId: String) : ChatAction
    data object RunInspectorDismissed : ChatAction
    data class RetryRunClicked(val runId: String) : ChatAction
    data class CancelRunClicked(val runId: String) : ChatAction
}

class ChatViewModel(
    private val chatRepository: ChatRepository,
    private val runRepository: RunRepository,
    private val sessionRepository: SessionRepository,
    private val settingsRepository: SettingsRepository,
    private val selectedSessionRepository: SelectedSessionRepository,
    private val runtimeResolver: AgentRuntimeResolver,
) : ViewModel() {
    private val draftMessage = MutableStateFlow("")
    private val isSending = MutableStateFlow(false)
    private val errorMessage = MutableStateFlow<String?>(null)
    private val selectedRunId = MutableStateFlow<String?>(null)

    private val selectedSessionId = selectedSessionRepository.observeSelectedSessionId()
    private val sessions = sessionRepository.observeSessions()
    private val messages = selectedSessionId.flatMapLatest { sessionId ->
        if (sessionId == null) {
            flowOf(emptyList())
        } else {
            chatRepository.observeMessages(sessionId)
        }
    }
    private val runs = selectedSessionId.flatMapLatest { sessionId ->
        if (sessionId == null) {
            flowOf(emptyList())
        } else {
            runRepository.observeRunsForSession(sessionId)
        }
    }
    private val selectedRun = selectedRunId.flatMapLatest { runId ->
        if (runId == null) {
            flowOf(null)
        } else {
            combine(
                runRepository.observeRun(runId),
                runRepository.observeEventsForRun(runId),
            ) { run, events ->
                run?.let {
                    RunInspectorUiState(
                        run = it,
                        events = events,
                    )
                }
            }
        }
    }
    private val chatContext = combine(
        selectedSessionId,
        sessions,
        messages,
        runs,
        settingsRepository.observeSettings(),
    ) { selectedId, sessionList, messageList, runList, settings ->
        ChatContext(
            selectedId = selectedId,
            sessionList = sessionList,
            messageList = messageList,
            runList = runList,
            settings = settings,
        )
    }

    val uiState: StateFlow<ChatUiState> = combine(
        chatContext,
        draftMessage,
        isSending,
        errorMessage,
        selectedRun,
    ) { context, draft, sending, error, selectedRunValue ->
        val activeSession = context.sessionList.find { it.id == context.selectedId }
        ChatUiState(
            sessionId = context.selectedId,
            sessionTitle = activeSession?.title ?: "New Chat",
            messages = context.messageList,
            draftMessage = draft,
            isSending = sending,
            providerType = context.settings.providerType,
            errorMessage = error,
            runSummariesByAssistantMessageId = context.runList.associate { run ->
                run.assistantMessageId to MessageRunSummaryUiState(
                    runId = run.id,
                    status = run.status,
                    durationMs = run.durationMs,
                    errorSummary = run.errorSummary,
                    providerLabel = "${run.providerType.displayName} | ${run.model}",
                )
            },
            selectedRun = selectedRunValue,
            activeRunCount = context.runList.count { it.status == AgentRunStatus.RUNNING },
        )
    }.stateIn(
        scope = viewModelScope,
        started = SharingStarted.Eagerly,
        initialValue = ChatUiState(),
    )

    private data class ChatContext(
        val selectedId: String?,
        val sessionList: List<com.yuhangdo.rustagent.model.ChatSession>,
        val messageList: List<ChatMessage>,
        val runList: List<AgentRun>,
        val settings: ProviderSettings,
    )

    fun onAction(action: ChatAction) {
        when (action) {
            is ChatAction.DraftChanged -> draftMessage.value = action.value
            ChatAction.SendClicked -> sendMessage()
            ChatAction.ErrorDismissed -> errorMessage.value = null
            is ChatAction.RunSelected -> selectedRunId.value = action.runId
            ChatAction.RunInspectorDismissed -> selectedRunId.value = null
            is ChatAction.RetryRunClicked -> retryRun(action.runId)
            is ChatAction.CancelRunClicked -> cancelRun(action.runId)
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

            try {
                val sessionId = selectedSessionRepository.currentSelectedSessionId()
                    ?: sessionRepository.createSession(suggestedSessionTitle(messageContent)).also {
                        selectedSessionRepository.selectSession(it)
                    }

                val userMessage = chatRepository.addUserMessage(sessionId, messageContent)
                val history = chatRepository.getMessages(sessionId)
                val assistantMessageId = chatRepository.createAssistantPlaceholder(sessionId)
                val settings = settingsRepository.getSettings()

                executeRun(
                    sessionId = sessionId,
                    userMessage = userMessage,
                    assistantMessageId = assistantMessageId,
                    history = history,
                    settings = settings,
                    triggerLabel = "User message submitted.",
                )
            } finally {
                isSending.value = false
            }
        }
    }

    private fun retryRun(runId: String) {
        if (isSending.value) {
            return
        }

        viewModelScope.launch {
            isSending.value = true
            errorMessage.value = null

            try {
                val originalRun = runRepository.getRun(runId)
                val originalUserMessage = originalRun?.let { chatRepository.getMessageById(it.userMessageId) }
                if (originalRun == null || originalUserMessage == null) {
                    errorMessage.value = "The original run could not be loaded for retry."
                    return@launch
                }

                selectedSessionRepository.selectSession(originalRun.sessionId)
                val assistantMessageId = chatRepository.createAssistantPlaceholder(originalRun.sessionId)
                val history = chatRepository.getHistoryThroughMessage(
                    sessionId = originalRun.sessionId,
                    messageId = originalRun.userMessageId,
                )
                val settings = settingsRepository.getSettings()

                executeRun(
                    sessionId = originalRun.sessionId,
                    userMessage = originalUserMessage,
                    assistantMessageId = assistantMessageId,
                    history = history,
                    settings = settings,
                    triggerLabel = "Retry requested from an existing run.",
                )
            } finally {
                isSending.value = false
            }
        }
    }

    private fun cancelRun(runId: String) {
        viewModelScope.launch {
            try {
                val run = runRepository.getRun(runId)
                if (run == null) {
                    errorMessage.value = "The selected run could not be loaded for cancellation."
                    return@launch
                }

                val currentSettings = settingsRepository.getSettings()
                val runtime = runtimeResolver.resolve(
                    currentSettings.copy(
                        providerType = run.providerType,
                        baseUrl = run.baseUrlSnapshot,
                        model = run.model,
                    ),
                )
                runtime.cancel(runId)
            } catch (throwable: Throwable) {
                errorMessage.value = throwable.message ?: "Unable to cancel the current run."
            }
        }
    }

    private suspend fun executeRun(
        sessionId: String,
        userMessage: ChatMessage,
        assistantMessageId: String,
        history: List<ChatMessage>,
        settings: ProviderSettings,
        triggerLabel: String,
    ) {
        val run = runRepository.createRun(
            sessionId = sessionId,
            userMessageId = userMessage.id,
            assistantMessageId = assistantMessageId,
            settings = settings,
        )

        val runtime = runtimeResolver.resolve(settings)
        var accumulatedReasoning = ""
        var accumulatedAnswer = ""
        var terminalEventSeen = false

        try {
            runtime.execute(
                AgentRuntimeRequest(
                    runId = run.id,
                    triggerLabel = triggerLabel,
                    history = history,
                    settings = settings,
                ),
            ).collect { event ->
                when (event) {
                    is AgentRuntimeEvent.OutputUpdate -> {
                        accumulatedReasoning = event.reasoningContent
                        accumulatedAnswer = event.answerContent
                        persistAssistantOutput(
                            assistantMessageId = assistantMessageId,
                            reasoningContent = accumulatedReasoning,
                            answerContent = accumulatedAnswer,
                        )
                    }

                    is AgentRuntimeEvent.RunUpdate -> {
                        when (event.type) {
                            RunEventType.COMPLETED -> {
                                terminalEventSeen = true
                                val completedRun = runRepository.markCompleted(run.id)
                                runRepository.appendEvent(
                                    runId = run.id,
                                    type = event.type,
                                    title = event.title,
                                    details = event.details.ifBlank {
                                        completedRun?.durationMs?.let { "Completed in ${it}ms." } ?: "Completed."
                                    },
                                )
                            }

                            RunEventType.FAILED -> {
                                terminalEventSeen = true
                                errorMessage.value = event.details
                                if (accumulatedAnswer.isBlank()) {
                                    accumulatedAnswer = "Agent run failed: ${event.details}"
                                }
                                persistAssistantOutput(
                                    assistantMessageId = assistantMessageId,
                                    reasoningContent = accumulatedReasoning,
                                    answerContent = accumulatedAnswer,
                                )
                                runRepository.markFailed(run.id, event.details)
                                runRepository.appendEvent(
                                    runId = run.id,
                                    type = event.type,
                                    title = event.title,
                                    details = event.details,
                                )
                            }

                            RunEventType.CANCELLED -> {
                                terminalEventSeen = true
                                if (accumulatedAnswer.isBlank()) {
                                    accumulatedAnswer = "Agent run cancelled."
                                }
                                persistAssistantOutput(
                                    assistantMessageId = assistantMessageId,
                                    reasoningContent = accumulatedReasoning,
                                    answerContent = accumulatedAnswer,
                                )
                                runRepository.markCancelled(run.id, event.details)
                                runRepository.appendEvent(
                                    runId = run.id,
                                    type = event.type,
                                    title = event.title,
                                    details = event.details.ifBlank { "Cancelled from the UI." },
                                )
                            }

                            else -> {
                                runRepository.appendEvent(
                                    runId = run.id,
                                    type = event.type,
                                    title = event.title,
                                    details = event.details,
                                )
                            }
                        }
                    }
                }
            }

            if (!terminalEventSeen) {
                val completedRun = runRepository.markCompleted(run.id)
                runRepository.appendEvent(
                    runId = run.id,
                    type = RunEventType.COMPLETED,
                    details = completedRun?.durationMs?.let { "Completed in ${it}ms." } ?: "Completed.",
                )
            }
        } catch (throwable: Throwable) {
            val runtimeError = throwable.message ?: "Unknown runtime error."
            errorMessage.value = runtimeError
            persistAssistantOutput(
                assistantMessageId = assistantMessageId,
                reasoningContent = accumulatedReasoning,
                answerContent = accumulatedAnswer.ifBlank { "Agent run failed: $runtimeError" },
            )
            runRepository.markFailed(run.id, runtimeError)
            runRepository.appendEvent(
                runId = run.id,
                type = RunEventType.FAILED,
                details = runtimeError,
            )
        }
    }

    private suspend fun persistAssistantOutput(
        assistantMessageId: String,
        reasoningContent: String,
        answerContent: String,
    ) {
        chatRepository.updateAssistantMessage(
            messageId = assistantMessageId,
            reasoningContent = summarizeReasoning(reasoningContent),
            answerContent = answerContent,
        )
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
                Column(
                    modifier = Modifier.weight(1f),
                    verticalArrangement = Arrangement.spacedBy(4.dp),
                ) {
                    Text(
                        text = uiState.sessionTitle,
                        style = MaterialTheme.typography.titleLarge,
                        fontWeight = FontWeight.SemiBold,
                    )
                    Text(
                        text = if (uiState.activeRunCount > 0) {
                            "${uiState.activeRunCount} run(s) active. Transcript and runtime traces are stored separately."
                        } else {
                            "Transcript and runtime traces are stored separately for debugging."
                        },
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                Column(
                    horizontalAlignment = Alignment.End,
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    AssistChip(
                        onClick = { },
                        label = { Text(uiState.providerType.displayName) },
                    )
                    if (uiState.activeRunCount > 0) {
                        AssistChip(
                            onClick = { },
                            label = { Text("Running") },
                        )
                    }
                }
            }

            if (uiState.errorMessage != null) {
                Card(modifier = Modifier.fillMaxWidth()) {
                    Text(
                        text = uiState.errorMessage,
                        modifier = Modifier.padding(14.dp),
                        color = MaterialTheme.colorScheme.error,
                        style = MaterialTheme.typography.bodyMedium,
                    )
                }
            }

            if (uiState.messages.isEmpty()) {
                Card(modifier = Modifier.weight(1f)) {
                    Column(
                        modifier = Modifier.padding(20.dp),
                        verticalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        Text(
                            text = "Run and inspect agent sessions",
                            style = MaterialTheme.typography.titleMedium,
                            fontWeight = FontWeight.SemiBold,
                        )
                        Text(
                            text = "Each reply now carries a linked run record, event timeline, and retry path so you can debug providers like an operator instead of just reading chat bubbles.",
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
                        MessageCard(
                            message = message,
                            runSummary = uiState.runSummariesByAssistantMessageId[message.id],
                            onViewRun = { runId -> onAction(ChatAction.RunSelected(runId)) },
                        )
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
                placeholder = { Text("Describe the bug, provider issue, or debugging task...") },
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

    uiState.selectedRun?.let { selectedRun ->
        RunInspectorDialog(
            selectedRun = selectedRun,
            onDismiss = { onAction(ChatAction.RunInspectorDismissed) },
            onRetry = { onAction(ChatAction.RetryRunClicked(selectedRun.run.id)) },
            onCancel = { onAction(ChatAction.CancelRunClicked(selectedRun.run.id)) },
        )
    }
}

@Composable
private fun MessageCard(
    message: ChatMessage,
    runSummary: MessageRunSummaryUiState?,
    onViewRun: (String) -> Unit,
) {
    val roleLabel = when (message.role) {
        MessageRole.USER -> "You"
        MessageRole.ASSISTANT -> "Agent"
        MessageRole.SYSTEM -> "System"
    }
    var showReasoning by rememberSaveable(message.id) { mutableStateOf(false) }

    Card(modifier = Modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    text = roleLabel,
                    style = MaterialTheme.typography.labelLarge,
                    color = MaterialTheme.colorScheme.primary,
                )
                if (runSummary != null) {
                    AssistChip(
                        onClick = { onViewRun(runSummary.runId) },
                        label = {
                            Text(
                                text = runSummary.durationMs?.let {
                                    "${runSummary.status.displayName} | ${it}ms"
                                } ?: runSummary.status.displayName,
                            )
                        },
                    )
                }
            }

            if (runSummary != null) {
                Text(
                    text = runSummary.providerLabel,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }

            if (message.reasoningContent.isNotBlank()) {
                Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.SpaceBetween,
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Text(
                            text = "Reasoning Summary",
                            style = MaterialTheme.typography.labelMedium,
                            fontWeight = FontWeight.Bold,
                        )
                        TextButton(onClick = { showReasoning = !showReasoning }) {
                            Text(if (showReasoning) "Collapse" else "Expand")
                        }
                    }
                    Text(
                        text = if (showReasoning) message.reasoningContent else summarizeReasoning(message.reasoningContent, 120),
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

            if (runSummary != null) {
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    TextButton(onClick = { onViewRun(runSummary.runId) }) {
                        Text("View Run")
                    }
                }
            }
        }
    }
}

@Composable
private fun RunInspectorDialog(
    selectedRun: RunInspectorUiState,
    onDismiss: () -> Unit,
    onRetry: () -> Unit,
    onCancel: () -> Unit,
) {
    Dialog(onDismissRequest = onDismiss) {
        Surface(
            shape = MaterialTheme.shapes.extraLarge,
            tonalElevation = 8.dp,
            modifier = Modifier
                .fillMaxWidth()
                .fillMaxHeight(0.9f),
        ) {
            Column(
                modifier = Modifier
                    .fillMaxSize()
                    .padding(20.dp),
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Text(
                    text = "Run Inspector",
                    style = MaterialTheme.typography.headlineSmall,
                    fontWeight = FontWeight.SemiBold,
                )

                Text(
                    text = "${selectedRun.run.providerType.displayName} | ${selectedRun.run.model}",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )

                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    AssistChip(
                        onClick = { },
                        label = { Text(selectedRun.run.status.displayName) },
                    )
                    selectedRun.run.durationMs?.let { duration ->
                        AssistChip(
                            onClick = { },
                            label = { Text("${duration}ms") },
                        )
                    }
                }

                if (selectedRun.run.errorSummary != null) {
                    Card(modifier = Modifier.fillMaxWidth()) {
                        Text(
                            text = selectedRun.run.errorSummary,
                            modifier = Modifier.padding(14.dp),
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.error,
                        )
                    }
                }

                Text(
                    text = "Timeline",
                    style = MaterialTheme.typography.titleMedium,
                    fontWeight = FontWeight.SemiBold,
                )

                if (selectedRun.events.isEmpty()) {
                    Box(
                        modifier = Modifier
                            .weight(1f)
                            .fillMaxWidth(),
                        contentAlignment = Alignment.Center,
                    ) {
                        Text(
                            text = "No events recorded for this run yet.",
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                } else {
                    LazyColumn(
                        modifier = Modifier.weight(1f),
                        verticalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        items(
                            items = selectedRun.events,
                            key = { event -> event.id },
                        ) { event ->
                            Card(modifier = Modifier.fillMaxWidth()) {
                                Column(
                                    modifier = Modifier.padding(14.dp),
                                    verticalArrangement = Arrangement.spacedBy(4.dp),
                                ) {
                                    Text(
                                        text = event.title,
                                        style = MaterialTheme.typography.labelLarge,
                                        fontWeight = FontWeight.SemiBold,
                                    )
                                    Text(
                                        text = event.details,
                                        style = MaterialTheme.typography.bodyMedium,
                                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    )
                                }
                            }
                        }
                    }
                }

                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween,
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    TextButton(onClick = onDismiss) {
                        Text("Close")
                    }
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        if (selectedRun.run.status == AgentRunStatus.RUNNING) {
                            TextButton(onClick = onCancel) {
                                Text("Cancel Run")
                            }
                        }
                        Button(onClick = onRetry) {
                            Text("Retry Run")
                        }
                    }
                }
            }
        }
    }
}
