package com.yuhangdo.rustagent.data.local

import androidx.room.Dao
import androidx.room.Database
import androidx.room.Entity
import androidx.room.ForeignKey
import androidx.room.Index
import androidx.room.Insert
import androidx.room.OnConflictStrategy
import androidx.room.PrimaryKey
import androidx.room.Query
import androidx.room.RoomDatabase
import com.yuhangdo.rustagent.model.ChatMessage
import com.yuhangdo.rustagent.model.ChatSession
import com.yuhangdo.rustagent.model.MessageRole
import com.yuhangdo.rustagent.model.ProviderSettings
import com.yuhangdo.rustagent.model.ProviderType
import kotlinx.coroutines.flow.Flow

@Entity(tableName = "sessions")
data class ConversationSessionEntity(
    @PrimaryKey val id: String,
    val title: String,
    val createdAt: Long,
    val updatedAt: Long,
    val lastPreview: String,
    val messageCount: Int,
)

@Entity(
    tableName = "messages",
    foreignKeys = [
        ForeignKey(
            entity = ConversationSessionEntity::class,
            parentColumns = ["id"],
            childColumns = ["sessionId"],
            onDelete = ForeignKey.CASCADE,
        ),
    ],
    indices = [Index(value = ["sessionId"])],
)
data class ChatMessageEntity(
    @PrimaryKey val id: String,
    val sessionId: String,
    val role: String,
    val reasoningContent: String,
    val answerContent: String,
    val createdAt: Long,
)

@Entity(tableName = "app_settings")
data class AppSettingsEntity(
    @PrimaryKey val singletonId: Int = 0,
    val providerType: String,
    val baseUrl: String,
    val apiKey: String,
    val model: String,
    val systemPrompt: String,
)

@Dao
interface SessionDao {
    @Query("SELECT * FROM sessions ORDER BY updatedAt DESC, createdAt DESC")
    fun observeSessions(): Flow<List<ConversationSessionEntity>>

    @Query("SELECT * FROM sessions WHERE id = :sessionId LIMIT 1")
    suspend fun getById(sessionId: String): ConversationSessionEntity?

    @Insert(onConflict = OnConflictStrategy.REPLACE)
    suspend fun upsert(session: ConversationSessionEntity)

    @Query("DELETE FROM sessions WHERE id = :sessionId")
    suspend fun deleteById(sessionId: String)
}

@Dao
interface ChatMessageDao {
    @Query("SELECT * FROM messages WHERE sessionId = :sessionId ORDER BY createdAt ASC")
    fun observeMessages(sessionId: String): Flow<List<ChatMessageEntity>>

    @Query("SELECT * FROM messages WHERE sessionId = :sessionId ORDER BY createdAt ASC")
    suspend fun getMessages(sessionId: String): List<ChatMessageEntity>

    @Query("SELECT * FROM messages WHERE id = :messageId LIMIT 1")
    suspend fun getMessageById(messageId: String): ChatMessageEntity?

    @Insert(onConflict = OnConflictStrategy.REPLACE)
    suspend fun upsert(message: ChatMessageEntity)

    @Query("SELECT COUNT(*) FROM messages WHERE sessionId = :sessionId")
    suspend fun countForSession(sessionId: String): Int
}

@Dao
interface AppSettingsDao {
    @Query("SELECT * FROM app_settings WHERE singletonId = 0")
    fun observe(): Flow<AppSettingsEntity?>

    @Query("SELECT * FROM app_settings WHERE singletonId = 0")
    suspend fun get(): AppSettingsEntity?

    @Insert(onConflict = OnConflictStrategy.REPLACE)
    suspend fun upsert(settings: AppSettingsEntity)
}

@Database(
    entities = [
        ConversationSessionEntity::class,
        ChatMessageEntity::class,
        AppSettingsEntity::class,
    ],
    version = 1,
    exportSchema = false,
)
abstract class AppDatabase : RoomDatabase() {
    abstract fun sessionDao(): SessionDao
    abstract fun chatMessageDao(): ChatMessageDao
    abstract fun appSettingsDao(): AppSettingsDao
}

fun ConversationSessionEntity.asDomain(): ChatSession = ChatSession(
    id = id,
    title = title,
    createdAt = createdAt,
    updatedAt = updatedAt,
    lastPreview = lastPreview,
    messageCount = messageCount,
)

fun ChatMessageEntity.asDomain(): ChatMessage = ChatMessage(
    id = id,
    sessionId = sessionId,
    role = MessageRole.valueOf(role),
    reasoningContent = reasoningContent,
    answerContent = answerContent,
    createdAt = createdAt,
)

fun AppSettingsEntity.asDomain(): ProviderSettings = ProviderSettings(
    providerType = ProviderType.valueOf(providerType),
    baseUrl = baseUrl,
    apiKey = apiKey,
    model = model,
    systemPrompt = systemPrompt,
)

fun ProviderSettings.asEntity(): AppSettingsEntity = AppSettingsEntity(
    providerType = providerType.name,
    baseUrl = baseUrl,
    apiKey = apiKey,
    model = model,
    systemPrompt = systemPrompt,
)
