package com.yuhangdo.rustagent.feature.chat

import com.google.common.truth.Truth.assertThat
import com.yuhangdo.rustagent.FakeAppSettingsDao
import com.yuhangdo.rustagent.FakeAgentRunDao
import com.yuhangdo.rustagent.FakeChatMessageDao
import com.yuhangdo.rustagent.FakeRunEventDao
import com.yuhangdo.rustagent.FakeSessionDao
import com.yuhangdo.rustagent.MainDispatcherRule
import com.yuhangdo.rustagent.StaticChatProvider
import com.yuhangdo.rustagent.StaticChatProviderResolver
import com.yuhangdo.rustagent.data.repository.ChatRepository
import com.yuhangdo.rustagent.data.repository.RunRepository
import com.yuhangdo.rustagent.data.repository.SelectedSessionRepository
import com.yuhangdo.rustagent.data.repository.SessionRepository
import com.yuhangdo.rustagent.data.repository.SettingsRepository
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
        val providerResolver = StaticChatProviderResolver(
            StaticChatProvider(
                reasoning = "Reasoning trace",
                answer = "Final answer",
            ),
        )
        val viewModel = ChatViewModel(
            chatRepository = chatRepository,
            runRepository = runRepository,
            sessionRepository = sessionRepository,
            settingsRepository = settingsRepository,
            selectedSessionRepository = selectedSessionRepository,
            providerResolver = providerResolver,
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
        val providerResolver = StaticChatProviderResolver(
            StaticChatProvider(
                reasoning = "Retry reasoning",
                answer = "Retry answer",
            ),
        )
        val viewModel = ChatViewModel(
            chatRepository = chatRepository,
            runRepository = runRepository,
            sessionRepository = sessionRepository,
            settingsRepository = settingsRepository,
            selectedSessionRepository = selectedSessionRepository,
            providerResolver = providerResolver,
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
}

