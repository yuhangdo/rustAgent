package com.yuhangdo.rustagent.feature.chat

import androidx.lifecycle.viewModelScope
import com.yuhangdo.rustagent.core.ui.mvi.MviViewModel
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
import com.yuhangdo.rustagent.model.RunEvent
import com.yuhangdo.rustagent.model.RunEventType
import com.yuhangdo.rustagent.model.suggestedSessionTitle
import com.yuhangdo.rustagent.model.summarizeReasoning
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.flatMapLatest
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.launch

class ChatViewModel(
    private val chatRepository: ChatRepository,
    private val runRepository: RunRepository,
    private val sessionRepository: SessionRepository,
    private val settingsRepository: SettingsRepository,
    private val selectedSessionRepository: SelectedSessionRepository,
    private val runtimeResolver: AgentRuntimeResolver,
) : MviViewModel<
    ChatContract.Intent,
    ChatContract.State,
    ChatContract.Effect,
    ChatContract.Mutation
>(
    initialState = ChatContract.State(),
    reducer = ChatReducer(),
) {
    private val selectedRunId = MutableStateFlow<String?>(null)

    init {
        observeChatSnapshots()
        observeDeepThinkingPanel()
    }

    override fun handleIntent(intent: ChatContract.Intent) {
        when (intent) {
            is ChatContract.Intent.DraftChanged -> mutate(
                ChatContract.Mutation.DraftChanged(intent.value),
            )

            ChatContract.Intent.SendClicked -> sendMessage()
            ChatContract.Intent.DismissError -> mutate(ChatContract.Mutation.ErrorChanged(null))
            is ChatContract.Intent.OpenDeepThinking -> {
                selectedRunId.value = intent.runId
            }

            ChatContract.Intent.CloseDeepThinking -> {
                selectedRunId.value = null
            }

            is ChatContract.Intent.RetryRun -> retryRun(intent.runId)
            is ChatContract.Intent.CancelRun -> cancelRun(intent.runId)
        }
    }

    private fun observeChatSnapshots() {
        viewModelScope.launch {
            val selectedSessionId = selectedSessionRepository.observeSelectedSessionId()
            val messages = selectedSessionId.flatMapLatest { sessionId ->
                if (sessionId == null) {
                    flowOf(emptyList())
                } else {
                    chatRepository.observeMessages(sessionId)
                }
            }
            val runs = selectedSessionId.flatMapLatest { sessionId ->
                if (sessionId == null) {
                    flowOf(emptyList())
                } else {
                    runRepository.observeRunsForSession(sessionId)
                }
            }

            combine(
                selectedSessionId,
                sessionRepository.observeSessions(),
                messages,
                runs,
                settingsRepository.observeSettings(),
            ) { selectedId, sessions, messageList, runList, settings ->
                ChatContract.Mutation.SnapshotLoaded(
                    sessionId = selectedId,
                    sessionTitle = sessions.find { it.id == selectedId }?.title ?: "New Chat",
                    messages = buildPresentations(messageList, runList),
                    providerTypeLabel = settings.providerType.displayName,
                    activeRunCount = runList.count { it.status == AgentRunStatus.RUNNING },
                )
            }.collect(::mutate)
        }
    }

    private fun observeDeepThinkingPanel() {
        viewModelScope.launch {
            selectedRunId.flatMapLatest { runId ->
                if (runId == null) {
                    flowOf(ChatContract.Mutation.DeepThinkingChanged(null))
                } else {
                    combine(
                        runRepository.observeRun(runId),
                        runRepository.observeEventsForRun(runId),
                    ) { run, events ->
                        ChatContract.Mutation.DeepThinkingChanged(
                            panel = run?.toDeepThinkingPanel(events),
                        )
                    }
                }
            }.collect(::mutate)
        }
    }

    private fun sendMessage() {
        val messageContent = uiState.value.draftMessage.trim()
        if (messageContent.isBlank() || uiState.value.isSending) {
            return
        }

        viewModelScope.launch {
            mutate(ChatContract.Mutation.DraftChanged(""))
            mutate(ChatContract.Mutation.SendingChanged(true))
            mutate(ChatContract.Mutation.ErrorChanged(null))

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
                mutate(ChatContract.Mutation.SendingChanged(false))
            }
        }
    }

    private fun retryRun(runId: String) {
        if (uiState.value.isSending) {
            return
        }

        viewModelScope.launch {
            mutate(ChatContract.Mutation.SendingChanged(true))
            mutate(ChatContract.Mutation.ErrorChanged(null))

            try {
                val originalRun = runRepository.getRun(runId)
                val originalUserMessage = originalRun?.let { chatRepository.getMessageById(it.userMessageId) }
                if (originalRun == null || originalUserMessage == null) {
                    mutate(ChatContract.Mutation.ErrorChanged("The original run could not be loaded for retry."))
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
                mutate(ChatContract.Mutation.SendingChanged(false))
            }
        }
    }

    private fun cancelRun(runId: String) {
        viewModelScope.launch {
            try {
                val run = runRepository.getRun(runId)
                if (run == null) {
                    mutate(ChatContract.Mutation.ErrorChanged("The selected run could not be loaded for cancellation."))
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
                mutate(
                    ChatContract.Mutation.ErrorChanged(
                        throwable.message ?: "Unable to cancel the current run.",
                    ),
                )
            }
        }
    }

    private suspend fun executeRun(
        sessionId: String,
        userMessage: ChatMessage,
        assistantMessageId: String,
        history: List<ChatMessage>,
        settings: com.yuhangdo.rustagent.model.ProviderSettings,
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
                    sessionId = sessionId,
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
                                mutate(ChatContract.Mutation.ErrorChanged(event.details))
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
            mutate(ChatContract.Mutation.ErrorChanged(runtimeError))
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

    private fun buildPresentations(
        messages: List<ChatMessage>,
        runs: List<AgentRun>,
    ): List<ChatMessagePresentation> {
        val runByAssistantMessage = runs.associateBy { it.assistantMessageId }
        return messages.map { message ->
            val linkedRun = runByAssistantMessage[message.id]
            ChatMessagePresentation(
                id = message.id,
                role = message.role,
                answerContent = message.answerContent,
                reasoningPreview = message.reasoningContent,
                providerLabel = linkedRun?.let { "${it.providerType.displayName} | ${it.model}" },
                runId = linkedRun?.id,
                runStatus = linkedRun?.status,
                durationMs = linkedRun?.durationMs,
                errorSummary = linkedRun?.errorSummary,
                isStreaming = linkedRun?.status == AgentRunStatus.RUNNING,
            )
        }
    }

    private fun AgentRun.toDeepThinkingPanel(events: List<RunEvent>): DeepThinkingPanelState {
        val items = if (events.isEmpty()) {
            listOf(
                RunTraceItem.TimelineEntry(
                    id = "$id-empty",
                    title = "No run events yet",
                    details = "The runtime has not produced any diagnostic events for this run.",
                ),
            )
        } else {
            events.map { event ->
                RunTraceItem.TimelineEntry(
                    id = event.id,
                    title = event.title,
                    details = event.details,
                )
            }
        }
        return DeepThinkingPanelState(
            runId = id,
            providerLabel = "${providerType.displayName} | $model",
            status = status,
            durationMs = durationMs,
            errorSummary = errorSummary,
            items = items,
        )
    }
}
