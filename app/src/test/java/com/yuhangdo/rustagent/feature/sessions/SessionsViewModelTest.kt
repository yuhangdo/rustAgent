package com.yuhangdo.rustagent.feature.sessions

import app.cash.turbine.test
import com.google.common.truth.Truth.assertThat
import com.yuhangdo.rustagent.FakeAgentRunDao
import com.yuhangdo.rustagent.FakeRunEventDao
import com.yuhangdo.rustagent.FakeSessionDao
import com.yuhangdo.rustagent.MainDispatcherRule
import com.yuhangdo.rustagent.data.repository.RunRepository
import com.yuhangdo.rustagent.data.repository.SelectedSessionRepository
import com.yuhangdo.rustagent.data.repository.SessionRepository
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.Rule
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class SessionsViewModelTest {
    @get:Rule
    val mainDispatcherRule = MainDispatcherRule()

    @Test
    fun createSession_selectsNewSession() = runTest {
        val selectedSessionRepository = SelectedSessionRepository()
        val viewModel = SessionsViewModel(
            sessionRepository = SessionRepository(FakeSessionDao()),
            runRepository = RunRepository(FakeAgentRunDao(), FakeRunEventDao()),
            selectedSessionRepository = selectedSessionRepository,
        )

        viewModel.onIntent(SessionsContract.Intent.CreateSession)
        advanceUntilIdle()

        val state = viewModel.uiState.value
        assertThat(state.sessions).hasSize(1)
        assertThat(state.selectedSessionId).isEqualTo(state.sessions.first().id)
    }

    @Test
    fun selectSession_emitsOpenChatEffect() = runTest {
        val sessionRepository = SessionRepository(FakeSessionDao())
        val selectedSessionRepository = SelectedSessionRepository()
        val viewModel = SessionsViewModel(
            sessionRepository = sessionRepository,
            runRepository = RunRepository(FakeAgentRunDao(), FakeRunEventDao()),
            selectedSessionRepository = selectedSessionRepository,
        )
        val sessionId = sessionRepository.createSession("Debug session")
        advanceUntilIdle()

        viewModel.effects.test {
            viewModel.onIntent(SessionsContract.Intent.SelectSession(sessionId))
            advanceUntilIdle()

            assertThat(awaitItem()).isEqualTo(SessionsContract.Effect.OpenChat(sessionId))
            cancelAndIgnoreRemainingEvents()
        }
    }
}
