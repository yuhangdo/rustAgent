package com.yuhangdo.rustagent.feature.chat

import com.google.common.truth.Truth.assertThat
import com.yuhangdo.rustagent.CancellableAgentRuntime
import com.yuhangdo.rustagent.FakeAppSettingsDao
import com.yuhangdo.rustagent.FakeAgentRunDao
import com.yuhangdo.rustagent.FakeChatMessageDao
import com.yuhangdo.rustagent.FakeRunEventDao
import com.yuhangdo.rustagent.FakeSessionDao
import com.yuhangdo.rustagent.MainDispatcherRule
import com.yuhangdo.rustagent.ScriptedAgentRuntime
import com.yuhangdo.rustagent.ScriptedAgentRuntimeResolver
import com.yuhangdo.rustagent.data.repository.ChatRepository
import com.yuhangdo.rustagent.data.repository.RunRepository
import com.yuhangdo.rustagent.data.repository.SelectedSessionRepository
import com.yuhangdo.rustagent.data.repository.SessionRepository
import com.yuhangdo.rustagent.data.repository.SettingsRepository
import com.yuhangdo.rustagent.data.runtime.AgentRuntimeEvent
import com.yuhangdo.rustagent.model.AgentRunStatus
import com.yuhangdo.rustagent.model.RunEventType
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.Rule
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class ChatViewModelTest {
    @get:Rule
    val mainDispatcherRule = MainDispatcherRule()

    @Test
    fun sendMessage_persists_reasoning_and_answer_in_separate_fields() = runTest {
        val sessionDao = FakeSessionDao()
        val messageDao = FakeChatMessageDao()
        val sessionRepository = SessionRepository(sessionDao)
        val chatRepository = ChatRepository(messageDao, sessionRepository)
        val runRepository = RunRepository(FakeAgentRunDao(), FakeRunEventDao())
        val settingsRepository = SettingsRepository(FakeAppSettingsDao())
        val selectedSessionRepository = SelectedSessionRepository()
        val runtimeResolver = ScriptedAgentRuntimeResolver(
            ScriptedAgentRuntime(
                events = listOf(
                    AgentRuntimeEvent.RunUpdate(RunEventType.STARTED, "User message submitted."),
                    AgentRuntimeEvent.RunUpdate(RunEventType.REQUEST_BUILT, "Built prompt context from 1 transcript messages."),
                    AgentRuntimeEvent.RunUpdate(RunEventType.PROVIDER_SELECTED, "Fake Provider | gpt-4o-mini"),
                    AgentRuntimeEvent.OutputUpdate(
                        reasoningContent = "Reasoning trace",
                        answerContent = "Final answer",
                    ),
                    AgentRuntimeEvent.RunUpdate(RunEventType.REASONING_SUMMARY, "Reasoning trace"),
                    AgentRuntimeEvent.RunUpdate(RunEventType.ANSWER_RECEIVED, "Final answer"),
                    AgentRuntimeEvent.RunUpdate(RunEventType.COMPLETED, "Completed in 12ms."),
                ),
            ),
        )
        val viewModel = ChatViewModel(
            chatRepository = chatRepository,
            runRepository = runRepository,
            sessionRepository = sessionRepository,
            settingsRepository = settingsRepository,
            selectedSessionRepository = selectedSessionRepository,
            runtimeResolver = runtimeResolver,
        )

        viewModel.onAction(ChatAction.DraftChanged("Build android-v1"))
        viewModel.onAction(ChatAction.SendClicked)
        advanceUntilIdle()

        val state = viewModel.uiState.value
        assertThat(state.messages).hasSize(2)
        assertThat(state.messages[1].reasoningContent).isEqualTo("Reasoning trace")
        assertThat(state.messages[1].answerContent).isEqualTo("Final answer")

        val runs = runRepository.observeAllRuns().first()
        assertThat(runs).hasSize(1)
        assertThat(runs.first().status).isEqualTo(AgentRunStatus.COMPLETED)

        val events = runRepository.getEventsForRun(runs.first().id)
        assertThat(events.map { it.type }).containsExactly(
            RunEventType.STARTED,
            RunEventType.REQUEST_BUILT,
            RunEventType.PROVIDER_SELECTED,
            RunEventType.REASONING_SUMMARY,
            RunEventType.ANSWER_RECEIVED,
            RunEventType.COMPLETED,
        ).inOrder()
    }

    @Test
    fun retryRun_creates_new_run_without_new_user_message() = runTest {
        val sessionDao = FakeSessionDao()
        val messageDao = FakeChatMessageDao()
        val sessionRepository = SessionRepository(sessionDao)
        val chatRepository = ChatRepository(messageDao, sessionRepository)
        val runRepository = RunRepository(FakeAgentRunDao(), FakeRunEventDao())
        val settingsRepository = SettingsRepository(FakeAppSettingsDao())
        val selectedSessionRepository = SelectedSessionRepository()
        val runtimeResolver = ScriptedAgentRuntimeResolver(
            ScriptedAgentRuntime(
                events = listOf(
                    AgentRuntimeEvent.RunUpdate(RunEventType.STARTED, "Retry requested from an existing run."),
                    AgentRuntimeEvent.RunUpdate(RunEventType.REQUEST_BUILT, "Built prompt context from 1 transcript messages."),
                    AgentRuntimeEvent.RunUpdate(RunEventType.PROVIDER_SELECTED, "Fake Provider | gpt-4o-mini"),
                    AgentRuntimeEvent.OutputUpdate(
                        reasoningContent = "Retry reasoning",
                        answerContent = "Retry answer",
                    ),
                    AgentRuntimeEvent.RunUpdate(RunEventType.REASONING_SUMMARY, "Retry reasoning"),
                    AgentRuntimeEvent.RunUpdate(RunEventType.ANSWER_RECEIVED, "Retry answer"),
                    AgentRuntimeEvent.RunUpdate(RunEventType.COMPLETED, "Completed in 14ms."),
                ),
            ),
        )
        val viewModel = ChatViewModel(
            chatRepository = chatRepository,
            runRepository = runRepository,
            sessionRepository = sessionRepository,
            settingsRepository = settingsRepository,
            selectedSessionRepository = selectedSessionRepository,
            runtimeResolver = runtimeResolver,
        )

        viewModel.onAction(ChatAction.DraftChanged("Retry this run"))
        viewModel.onAction(ChatAction.SendClicked)
        advanceUntilIdle()

        val firstRunId = runRepository.observeAllRuns().first().first().id
        viewModel.onAction(ChatAction.RetryRunClicked(firstRunId))
        advanceUntilIdle()

        val finalState = viewModel.uiState.value
        assertThat(finalState.messages).hasSize(3)
        assertThat(finalState.messages.count { it.role.name == "USER" }).isEqualTo(1)
        assertThat(finalState.messages.count { it.role.name == "ASSISTANT" }).isEqualTo(2)
        assertThat(runRepository.observeAllRuns().first()).hasSize(2)
    }

    @Test
    fun sendMessage_whenRuntimeReportsFailure_marks_run_failed_and_updates_assistant() = runTest {
        val sessionDao = FakeSessionDao()
        val messageDao = FakeChatMessageDao()
        val sessionRepository = SessionRepository(sessionDao)
        val chatRepository = ChatRepository(messageDao, sessionRepository)
        val runRepository = RunRepository(FakeAgentRunDao(), FakeRunEventDao())
        val settingsRepository = SettingsRepository(FakeAppSettingsDao())
        val selectedSessionRepository = SelectedSessionRepository()
        val runtimeResolver = ScriptedAgentRuntimeResolver(
            ScriptedAgentRuntime(
                events = listOf(
                    AgentRuntimeEvent.RunUpdate(RunEventType.STARTED, "User message submitted."),
                    AgentRuntimeEvent.RunUpdate(RunEventType.REQUEST_BUILT, "Built prompt context from 1 transcript messages."),
                    AgentRuntimeEvent.RunUpdate(RunEventType.PROVIDER_SELECTED, "Embedded Rust Agent | gpt-4o-mini"),
                    AgentRuntimeEvent.RunUpdate(RunEventType.FAILED, "Embedded runtime unavailable."),
                ),
            ),
        )
        val viewModel = ChatViewModel(
            chatRepository = chatRepository,
            runRepository = runRepository,
            sessionRepository = sessionRepository,
            settingsRepository = settingsRepository,
            selectedSessionRepository = selectedSessionRepository,
            runtimeResolver = runtimeResolver,
        )

        viewModel.onAction(ChatAction.DraftChanged("Use embedded runtime"))
        viewModel.onAction(ChatAction.SendClicked)
        advanceUntilIdle()

        val state = viewModel.uiState.value
        assertThat(state.errorMessage).isEqualTo("Embedded runtime unavailable.")
        assertThat(state.messages).hasSize(2)
        assertThat(state.messages[1].answerContent).isEqualTo("Agent run failed: Embedded runtime unavailable.")

        val run = runRepository.observeAllRuns().first().first()
        assertThat(run.status).isEqualTo(AgentRunStatus.FAILED)
        assertThat(run.errorSummary).isEqualTo("Embedded runtime unavailable.")
        assertThat(runRepository.getEventsForRun(run.id).last().type).isEqualTo(RunEventType.FAILED)
    }

    @Test
    fun cancelRun_requests_runtime_cancellation_and_marks_run_cancelled() = runTest {
        val sessionDao = FakeSessionDao()
        val messageDao = FakeChatMessageDao()
        val sessionRepository = SessionRepository(sessionDao)
        val chatRepository = ChatRepository(messageDao, sessionRepository)
        val runRepository = RunRepository(FakeAgentRunDao(), FakeRunEventDao())
        val settingsRepository = SettingsRepository(FakeAppSettingsDao())
        val selectedSessionRepository = SelectedSessionRepository()
        val runtime = CancellableAgentRuntime()
        val runtimeResolver = ScriptedAgentRuntimeResolver(runtime)
        val viewModel = ChatViewModel(
            chatRepository = chatRepository,
            runRepository = runRepository,
            sessionRepository = sessionRepository,
            settingsRepository = settingsRepository,
            selectedSessionRepository = selectedSessionRepository,
            runtimeResolver = runtimeResolver,
        )

        viewModel.onAction(ChatAction.DraftChanged("Cancel this run"))
        viewModel.onAction(ChatAction.SendClicked)
        advanceUntilIdle()

        val runId = runRepository.observeAllRuns().first().first().id
        viewModel.onAction(ChatAction.CancelRunClicked(runId))
        advanceUntilIdle()

        assertThat(runtime.cancelledRuns).contains(runId)

        val run = runRepository.observeAllRuns().first().first()
        assertThat(run.status).isEqualTo(AgentRunStatus.CANCELLED)
        assertThat(run.errorSummary).isEqualTo("Cancelled from the UI.")

        val assistantMessage = viewModel.uiState.value.messages.last()
        assertThat(assistantMessage.answerContent).isEqualTo("Agent run cancelled.")
        assertThat(runRepository.getEventsForRun(runId).last().type).isEqualTo(RunEventType.CANCELLED)
    }
}

