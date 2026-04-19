package com.yuhangdo.rustagent.data.repository

import com.google.common.truth.Truth.assertThat
import com.yuhangdo.rustagent.data.local.ChatMessageDao
import com.yuhangdo.rustagent.data.local.ChatMessageEntity
import com.yuhangdo.rustagent.data.local.ConversationSessionEntity
import com.yuhangdo.rustagent.data.local.SessionDao
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.test.runTest
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class ChatRepositoryTest {
    @Test
    fun addUserMessage_preserves_inserted_message_after_session_metadata_updates() = runTest {
        val messageDao = CascadingFakeChatMessageDao()
        val sessionDao = CascadingFakeSessionDao(onReplace = messageDao::deleteForSession)
        val sessionRepository = SessionRepository(sessionDao)
        val repository = ChatRepository(
            chatMessageDao = messageDao,
            sessionRepository = sessionRepository,
        )

        val sessionId = sessionRepository.createSession("Debug session")
        repository.addUserMessage(sessionId, "hello emulator")

        val storedMessages = repository.getMessages(sessionId)
        assertThat(storedMessages).hasSize(1)
        assertThat(storedMessages.single().answerContent).isEqualTo("hello emulator")
    }
}

private class CascadingFakeSessionDao(
    private val onReplace: (String) -> Unit,
) : SessionDao {
    private val sessions = MutableStateFlow<List<ConversationSessionEntity>>(emptyList())

    override fun observeSessions(): Flow<List<ConversationSessionEntity>> = sessions

    override suspend fun getById(sessionId: String): ConversationSessionEntity? =
        sessions.value.firstOrNull { it.id == sessionId }

    override suspend fun upsert(session: ConversationSessionEntity) {
        if (sessions.value.any { it.id == session.id }) {
            onReplace(session.id)
        }
        sessions.value = sessions.value
            .filterNot { it.id == session.id }
            .plus(session)
            .sortedByDescending { it.updatedAt }
    }

    override suspend fun updateMetadata(
        sessionId: String,
        updatedAt: Long,
        lastPreview: String,
        messageCount: Int,
    ) {
        sessions.value = sessions.value.map { session ->
            if (session.id == sessionId) {
                session.copy(
                    updatedAt = updatedAt,
                    lastPreview = lastPreview,
                    messageCount = messageCount,
                )
            } else {
                session
            }
        }.sortedByDescending { it.updatedAt }
    }

    override suspend fun deleteById(sessionId: String) {
        sessions.value = sessions.value.filterNot { it.id == sessionId }
    }
}

private class CascadingFakeChatMessageDao : ChatMessageDao {
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

    fun deleteForSession(sessionId: String) {
        messages.value = messages.value.filterNot { it.sessionId == sessionId }
    }
}
