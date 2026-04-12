package com.yuhangdo.rustagent

import com.yuhangdo.rustagent.data.local.AppSettingsDao
import com.yuhangdo.rustagent.data.local.AppSettingsEntity
import com.yuhangdo.rustagent.data.local.ChatMessageDao
import com.yuhangdo.rustagent.data.local.ChatMessageEntity
import com.yuhangdo.rustagent.data.local.ConversationSessionEntity
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

