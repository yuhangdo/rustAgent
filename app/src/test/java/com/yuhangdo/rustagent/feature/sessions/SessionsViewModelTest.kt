package com.yuhangdo.rustagent.feature.sessions

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
    fun createSession_selects_new_session() = runTest {
        val selectedSessionRepository = SelectedSessionRepository()
        val viewModel = SessionsViewModel(
            sessionRepository = SessionRepository(FakeSessionDao()),
            runRepository = RunRepository(FakeAgentRunDao(), FakeRunEventDao()),
            selectedSessionRepository = selectedSessionRepository,
        )

        viewModel.onAction(SessionsAction.CreateClicked)
        advanceUntilIdle()

        val state = viewModel.uiState.value
        assertThat(state.sessions).hasSize(1)
        assertThat(state.selectedSessionId).isEqualTo(state.sessions.first().id)
    }
}

