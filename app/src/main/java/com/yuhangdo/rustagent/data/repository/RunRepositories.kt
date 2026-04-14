package com.yuhangdo.rustagent.data.repository

import com.yuhangdo.rustagent.data.local.AgentRunDao
import com.yuhangdo.rustagent.data.local.RunEventDao
import com.yuhangdo.rustagent.data.local.asDomain
import com.yuhangdo.rustagent.data.local.asEntity
import com.yuhangdo.rustagent.model.AgentRun
import com.yuhangdo.rustagent.model.AgentRunStatus
import com.yuhangdo.rustagent.model.ProviderSettings
import com.yuhangdo.rustagent.model.RunEvent
import com.yuhangdo.rustagent.model.RunEventType
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map
import java.util.UUID

class RunRepository(
    private val agentRunDao: AgentRunDao,
    private val runEventDao: RunEventDao,
) {
    fun observeAllRuns(): Flow<List<AgentRun>> = agentRunDao.observeAllRuns().map { runs ->
        runs.map { it.asDomain() }
    }

    fun observeRunsForSession(sessionId: String): Flow<List<AgentRun>> =
        agentRunDao.observeRunsForSession(sessionId).map { runs ->
            runs.map { it.asDomain() }
        }

    fun observeRun(runId: String): Flow<AgentRun?> = agentRunDao.observeRun(runId).map { it?.asDomain() }

    fun observeEventsForRun(runId: String): Flow<List<RunEvent>> =
        runEventDao.observeEventsForRun(runId).map { events ->
            events.map { it.asDomain() }
        }

    suspend fun getRun(runId: String): AgentRun? = agentRunDao.getById(runId)?.asDomain()

    suspend fun getEventsForRun(runId: String): List<RunEvent> =
        runEventDao.getEventsForRun(runId).map { it.asDomain() }

    suspend fun createRun(
        sessionId: String,
        userMessageId: String,
        assistantMessageId: String,
        settings: ProviderSettings,
    ): AgentRun {
        val run = AgentRun(
            id = UUID.randomUUID().toString(),
            sessionId = sessionId,
            userMessageId = userMessageId,
            assistantMessageId = assistantMessageId,
            status = AgentRunStatus.RUNNING,
            providerType = settings.providerType,
            model = settings.model,
            baseUrlSnapshot = settings.baseUrl,
            startedAt = System.currentTimeMillis(),
            completedAt = null,
            durationMs = null,
            errorSummary = null,
        )
        agentRunDao.upsert(run.asEntity())
        return run
    }

    suspend fun appendEvent(
        runId: String,
        type: RunEventType,
        details: String,
        title: String = type.displayName,
    ): RunEvent {
        val event = RunEvent(
            id = UUID.randomUUID().toString(),
            runId = runId,
            type = type,
            title = title,
            details = details,
            createdAt = System.currentTimeMillis(),
            orderIndex = runEventDao.countForRun(runId),
        )
        runEventDao.upsert(event.asEntity())
        return event
    }

    suspend fun markCompleted(runId: String): AgentRun? {
        val current = agentRunDao.getById(runId)?.asDomain() ?: return null
        val completedAt = System.currentTimeMillis()
        val updated = current.copy(
            status = AgentRunStatus.COMPLETED,
            completedAt = completedAt,
            durationMs = completedAt - current.startedAt,
            errorSummary = null,
        )
        agentRunDao.upsert(updated.asEntity())
        return updated
    }

    suspend fun markFailed(
        runId: String,
        errorSummary: String,
    ): AgentRun? {
        val current = agentRunDao.getById(runId)?.asDomain() ?: return null
        val completedAt = System.currentTimeMillis()
        val updated = current.copy(
            status = AgentRunStatus.FAILED,
            completedAt = completedAt,
            durationMs = completedAt - current.startedAt,
            errorSummary = errorSummary,
        )
        agentRunDao.upsert(updated.asEntity())
        return updated
    }
}
