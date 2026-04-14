package com.yuhangdo.rustagent.data.repository

import com.google.common.truth.Truth.assertThat
import com.yuhangdo.rustagent.FakeAgentRunDao
import com.yuhangdo.rustagent.FakeRunEventDao
import com.yuhangdo.rustagent.model.AgentRunStatus
import com.yuhangdo.rustagent.model.ProviderSettings
import com.yuhangdo.rustagent.model.RunEventType
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.runTest
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class RunRepositoryTest {
    @Test
    fun createRun_appendEvents_and_complete_updates_status_and_timeline() = runTest {
        val repository = RunRepository(
            agentRunDao = FakeAgentRunDao(),
            runEventDao = FakeRunEventDao(),
        )

        val run = repository.createRun(
            sessionId = "session-1",
            userMessageId = "user-1",
            assistantMessageId = "assistant-1",
            settings = ProviderSettings(),
        )
        repository.appendEvent(run.id, RunEventType.STARTED, "Run started.")
        repository.appendEvent(run.id, RunEventType.REQUEST_BUILT, "Prompt built.")
        repository.markCompleted(run.id)

        val storedRun = repository.observeAllRuns().first().first()
        val events = repository.getEventsForRun(run.id)

        assertThat(storedRun.status).isEqualTo(AgentRunStatus.COMPLETED)
        assertThat(storedRun.durationMs).isNotNull()
        assertThat(events.map { it.type }).containsExactly(
            RunEventType.STARTED,
            RunEventType.REQUEST_BUILT,
        ).inOrder()
    }

    @Test
    fun markFailed_sets_error_summary() = runTest {
        val repository = RunRepository(
            agentRunDao = FakeAgentRunDao(),
            runEventDao = FakeRunEventDao(),
        )

        val run = repository.createRun(
            sessionId = "session-2",
            userMessageId = "user-2",
            assistantMessageId = "assistant-2",
            settings = ProviderSettings(),
        )
        repository.markFailed(run.id, "Boom")

        val storedRun = repository.observeAllRuns().first().first()
        assertThat(storedRun.status).isEqualTo(AgentRunStatus.FAILED)
        assertThat(storedRun.errorSummary).isEqualTo("Boom")
    }
}
