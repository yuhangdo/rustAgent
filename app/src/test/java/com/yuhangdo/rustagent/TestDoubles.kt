package com.yuhangdo.rustagent

import com.yuhangdo.rustagent.data.local.AppSettingsDao
import com.yuhangdo.rustagent.data.local.AppSettingsEntity
import com.yuhangdo.rustagent.data.local.ChatMessageDao
import com.yuhangdo.rustagent.data.local.ChatMessageEntity
import com.yuhangdo.rustagent.data.local.ConversationSessionEntity
import com.yuhangdo.rustagent.data.local.AgentRunDao
import com.yuhangdo.rustagent.data.local.AgentRunEntity
import com.yuhangdo.rustagent.data.local.RunEventDao
import com.yuhangdo.rustagent.data.local.RunEventEntity
import com.yuhangdo.rustagent.data.local.SessionDao
import com.yuhangdo.rustagent.data.provider.ChatProvider
import com.yuhangdo.rustagent.data.provider.ChatProviderResolver
import com.yuhangdo.rustagent.model.ProviderChunk
import com.yuhangdo.rustagent.model.ProviderRequest
import com.yuhangdo.rustagent.model.ProviderSettings
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.map

class FakeSessionDao : SessionDao {
    private val sessions = MutableStateFlow<List<ConversationSessionEntity>>(emptyList())

    override fun observeSessions(): Flow<List<ConversationSessionEntity>> = sessions

    override suspend fun getById(sessionId: String): ConversationSessionEntity? =
        sessions.value.firstOrNull { it.id == sessionId }

    override suspend fun upsert(session: ConversationSessionEntity) {
        sessions.value = sessions.value
            .filterNot { it.id == session.id }
            .plus(session)
            .sortedByDescending { it.updatedAt }
    }

    override suspend fun deleteById(sessionId: String) {
        sessions.value = sessions.value.filterNot { it.id == sessionId }
    }
}

class FakeChatMessageDao : ChatMessageDao {
    private val messages = MutableStateFlow<List<ChatMessageEntity>>(emptyList())

    override fun observeMessages(sessionId: String): Flow<List<ChatMessageEntity>> =
        messages.map { all -> all.filter { it.sessionId == sessionId }.sortedBy { it.createdAt } }

    override suspend fun getMessages(sessionId: String): List<ChatMessageEntity> =
        messages.value.filter { it.sessionId == sessionId }.sortedBy { it.createdAt }

    override suspend fun getMessageById(messageId: String): ChatMessageEntity? =
        messages.value.firstOrNull { it.id == messageId }

    override suspend fun upsert(message: ChatMessageEntity) {
        messages.value = messages.value
            .filterNot { it.id == message.id }
            .plus(message)
            .sortedBy { it.createdAt }
    }

    override suspend fun countForSession(sessionId: String): Int =
        messages.value.count { it.sessionId == sessionId }
}

class FakeAppSettingsDao(
    initialValue: AppSettingsEntity? = null,
) : AppSettingsDao {
    private val settings = MutableStateFlow(initialValue)

    override fun observe(): Flow<AppSettingsEntity?> = settings

    override suspend fun get(): AppSettingsEntity? = settings.value

    override suspend fun upsert(settings: AppSettingsEntity) {
        this.settings.value = settings
    }
}

class FakeAgentRunDao : AgentRunDao {
    private val runs = MutableStateFlow<List<AgentRunEntity>>(emptyList())

    override fun observeAllRuns(): Flow<List<AgentRunEntity>> = runs.map { all ->
        all.sortedByDescending { it.startedAt }
    }

    override fun observeRunsForSession(sessionId: String): Flow<List<AgentRunEntity>> = runs.map { all ->
        all.filter { it.sessionId == sessionId }.sortedByDescending { it.startedAt }
    }

    override fun observeRun(runId: String): Flow<AgentRunEntity?> = runs.map { all ->
        all.firstOrNull { it.id == runId }
    }

    override suspend fun getById(runId: String): AgentRunEntity? = runs.value.firstOrNull { it.id == runId }

    override suspend fun upsert(run: AgentRunEntity) {
        runs.value = runs.value
            .filterNot { it.id == run.id }
            .plus(run)
            .sortedByDescending { it.startedAt }
    }
}

class FakeRunEventDao : RunEventDao {
    private val events = MutableStateFlow<List<RunEventEntity>>(emptyList())

    override fun observeEventsForRun(runId: String): Flow<List<RunEventEntity>> = events.map { all ->
        all.filter { it.runId == runId }.sortedWith(compareBy<RunEventEntity> { it.orderIndex }.thenBy { it.createdAt })
    }

    override suspend fun getEventsForRun(runId: String): List<RunEventEntity> =
        events.value.filter { it.runId == runId }.sortedWith(compareBy<RunEventEntity> { it.orderIndex }.thenBy { it.createdAt })

    override suspend fun countForRun(runId: String): Int = events.value.count { it.runId == runId }

    override suspend fun upsert(event: RunEventEntity) {
        events.value = events.value
            .filterNot { it.id == event.id }
            .plus(event)
            .sortedWith(compareBy<RunEventEntity> { it.runId }.thenBy { it.orderIndex }.thenBy { it.createdAt })
    }
}

class StaticChatProvider(
    private val reasoning: String,
    private val answer: String,
) : ChatProvider {
    override fun streamReply(request: ProviderRequest): Flow<ProviderChunk> = flow {
        emit(ProviderChunk(reasoningDelta = reasoning))
        emit(ProviderChunk(answerDelta = answer))
    }
}

class StaticChatProviderResolver(
    private val provider: ChatProvider,
) : ChatProviderResolver {
    override fun resolve(settings: ProviderSettings): ChatProvider = provider
}

