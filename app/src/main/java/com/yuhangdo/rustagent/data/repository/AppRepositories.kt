package com.yuhangdo.rustagent.data.repository

import com.yuhangdo.rustagent.data.local.AppSettingsDao
import com.yuhangdo.rustagent.data.local.ChatMessageDao
import com.yuhangdo.rustagent.data.local.ChatMessageEntity
import com.yuhangdo.rustagent.data.local.SessionDao
import com.yuhangdo.rustagent.data.local.asDomain
import com.yuhangdo.rustagent.data.local.asEntity
import com.yuhangdo.rustagent.model.ChatMessage
import com.yuhangdo.rustagent.model.ChatSession
import com.yuhangdo.rustagent.model.MessageRole
import com.yuhangdo.rustagent.model.ProviderSettings
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.map
import java.util.UUID

class SelectedSessionRepository {
    private val selectedSessionId = MutableStateFlow<String?>(null)

    fun observeSelectedSessionId(): StateFlow<String?> = selectedSessionId.asStateFlow()

    suspend fun selectSession(sessionId: String?) {
        selectedSessionId.emit(sessionId)
    }

    fun currentSelectedSessionId(): String? = selectedSessionId.value
}

class SessionRepository(
    private val sessionDao: SessionDao,
) {
    fun observeSessions(): Flow<List<ChatSession>> = sessionDao.observeSessions().map { sessions ->
        sessions.map { it.asDomain() }
    }

    suspend fun createSession(title: String = "New Chat"): String {
        val now = System.currentTimeMillis()
        val sessionId = UUID.randomUUID().toString()
        sessionDao.upsert(
            com.yuhangdo.rustagent.data.local.ConversationSessionEntity(
                id = sessionId,
                title = title.ifBlank { "New Chat" },
                createdAt = now,
                updatedAt = now,
                lastPreview = "",
                messageCount = 0,
            ),
        )
        return sessionId
    }

    suspend fun getSession(sessionId: String): ChatSession? = sessionDao.getById(sessionId)?.asDomain()

    suspend fun deleteSession(sessionId: String) {
        sessionDao.deleteById(sessionId)
    }

    suspend fun touchSession(
        sessionId: String,
        preview: String,
        messageCount: Int,
    ) {
        val current = sessionDao.getById(sessionId) ?: return
        sessionDao.upsert(
            current.copy(
                updatedAt = System.currentTimeMillis(),
                lastPreview = preview,
                messageCount = messageCount,
            ),
        )
    }
}

class ChatRepository(
    private val chatMessageDao: ChatMessageDao,
    private val sessionRepository: SessionRepository,
) {
    fun observeMessages(sessionId: String): Flow<List<ChatMessage>> = chatMessageDao.observeMessages(sessionId).map {
        it.map { entity -> entity.asDomain() }
    }

    suspend fun getMessages(sessionId: String): List<ChatMessage> = chatMessageDao.getMessages(sessionId).map {
        it.asDomain()
    }

    suspend fun getMessageById(messageId: String): ChatMessage? = chatMessageDao.getMessageById(messageId)?.asDomain()

    suspend fun getHistoryThroughMessage(
        sessionId: String,
        messageId: String,
    ): List<ChatMessage> {
        val messages = getMessages(sessionId)
        val history = mutableListOf<ChatMessage>()
        for (message in messages) {
            history += message
            if (message.id == messageId) {
                break
            }
        }
        return history
    }

    suspend fun addUserMessage(
        sessionId: String,
        content: String,
    ): ChatMessage {
        val entity = ChatMessageEntity(
            id = UUID.randomUUID().toString(),
            sessionId = sessionId,
            role = MessageRole.USER.name,
            reasoningContent = "",
            answerContent = content,
            createdAt = System.currentTimeMillis(),
        )
        chatMessageDao.upsert(entity)
        sessionRepository.touchSession(
            sessionId = sessionId,
            preview = content,
            messageCount = chatMessageDao.countForSession(sessionId),
        )
        return entity.asDomain()
    }

    suspend fun createAssistantPlaceholder(sessionId: String): String {
        val entity = ChatMessageEntity(
            id = UUID.randomUUID().toString(),
            sessionId = sessionId,
            role = MessageRole.ASSISTANT.name,
            reasoningContent = "",
            answerContent = "",
            createdAt = System.currentTimeMillis(),
        )
        chatMessageDao.upsert(entity)
        sessionRepository.touchSession(
            sessionId = sessionId,
            preview = "Thinking...",
            messageCount = chatMessageDao.countForSession(sessionId),
        )
        return entity.id
    }

    suspend fun updateAssistantMessage(
        messageId: String,
        reasoningContent: String,
        answerContent: String,
    ) {
        val current = chatMessageDao.getMessageById(messageId) ?: return
        val updated = current.copy(
            reasoningContent = reasoningContent,
            answerContent = answerContent,
        )
        chatMessageDao.upsert(updated)
        sessionRepository.touchSession(
            sessionId = updated.sessionId,
            preview = answerContent.ifBlank { reasoningContent.ifBlank { "Waiting for reply..." } },
            messageCount = chatMessageDao.countForSession(updated.sessionId),
        )
    }
}

class SettingsRepository(
    private val appSettingsDao: AppSettingsDao,
) {
    fun observeSettings(): Flow<ProviderSettings> = appSettingsDao.observe().map { entity ->
        entity?.asDomain() ?: ProviderSettings()
    }

    suspend fun getSettings(): ProviderSettings = appSettingsDao.get()?.asDomain() ?: ProviderSettings()

    suspend fun updateSettings(settings: ProviderSettings) {
        appSettingsDao.upsert(settings.asEntity())
    }
}
