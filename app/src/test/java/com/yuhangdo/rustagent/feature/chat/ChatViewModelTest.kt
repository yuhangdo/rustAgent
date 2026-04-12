package com.yuhangdo.rustagent.feature.chat

import com.google.common.truth.Truth.assertThat
import com.yuhangdo.rustagent.FakeAppSettingsDao
import com.yuhangdo.rustagent.FakeChatMessageDao
import com.yuhangdo.rustagent.FakeSessionDao
import com.yuhangdo.rustagent.MainDispatcherRule
import com.yuhangdo.rustagent.StaticChatProvider
import com.yuhangdo.rustagent.StaticChatProviderResolver
import com.yuhangdo.rustagent.data.repository.ChatRepository
import com.yuhangdo.rustagent.data.repository.SelectedSessionRepository
import com.yuhangdo.rustagent.data.repository.SessionRepository
import com.yuhangdo.rustagent.data.repository.SettingsRepository
import kotlinx.coroutines.ExperimentalCoroutinesApi
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
    }
}

