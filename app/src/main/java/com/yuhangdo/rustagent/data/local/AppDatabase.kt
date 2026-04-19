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
import androidx.room.ColumnInfo
import com.yuhangdo.rustagent.model.ChatMessage
import com.yuhangdo.rustagent.model.ChatSession
import com.yuhangdo.rustagent.model.MessageRole
import com.yuhangdo.rustagent.model.ProviderSettings
import com.yuhangdo.rustagent.model.ProviderType
import com.yuhangdo.rustagent.model.AgentRun
import com.yuhangdo.rustagent.model.AgentRunStatus
import com.yuhangdo.rustagent.model.FakeProviderScenario
import com.yuhangdo.rustagent.model.RunEvent
import com.yuhangdo.rustagent.model.RunEventType
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
    @ColumnInfo(defaultValue = "")
    val workspaceRoot: String,
    @ColumnInfo(defaultValue = "SUCCESS_WITH_REASONING")
    val fakeScenario: String,
)

@Entity(
    tableName = "agent_runs",
    foreignKeys = [
        ForeignKey(
            entity = ConversationSessionEntity::class,
            parentColumns = ["id"],
            childColumns = ["sessionId"],
            onDelete = ForeignKey.CASCADE,
        ),
    ],
    indices = [
        Index(value = ["sessionId"]),
        Index(value = ["userMessageId"]),
        Index(value = ["assistantMessageId"]),
    ],
)
data class AgentRunEntity(
    @PrimaryKey val id: String,
    val sessionId: String,
    val userMessageId: String,
    val assistantMessageId: String,
    val status: String,
    val providerType: String,
    val model: String,
    val baseUrlSnapshot: String,
    val startedAt: Long,
    val completedAt: Long?,
    val durationMs: Long?,
    val errorSummary: String?,
)

@Entity(
    tableName = "run_events",
    foreignKeys = [
        ForeignKey(
            entity = AgentRunEntity::class,
            parentColumns = ["id"],
            childColumns = ["runId"],
            onDelete = ForeignKey.CASCADE,
        ),
    ],
    indices = [Index(value = ["runId"])],
)
data class RunEventEntity(
    @PrimaryKey val id: String,
    val runId: String,
    val type: String,
    val title: String,
    val details: String,
    val createdAt: Long,
    val orderIndex: Int,
)

@Dao
interface SessionDao {
    @Query("SELECT * FROM sessions ORDER BY updatedAt DESC, createdAt DESC")
    fun observeSessions(): Flow<List<ConversationSessionEntity>>

    @Query("SELECT * FROM sessions WHERE id = :sessionId LIMIT 1")
    suspend fun getById(sessionId: String): ConversationSessionEntity?

    @Insert(onConflict = OnConflictStrategy.REPLACE)
    suspend fun upsert(session: ConversationSessionEntity)

    @Query(
        """
        UPDATE sessions
        SET updatedAt = :updatedAt,
            lastPreview = :lastPreview,
            messageCount = :messageCount
        WHERE id = :sessionId
        """,
    )
    suspend fun updateMetadata(
        sessionId: String,
        updatedAt: Long,
        lastPreview: String,
        messageCount: Int,
    )

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

@Dao
interface AgentRunDao {
    @Query("SELECT * FROM agent_runs ORDER BY startedAt DESC")
    fun observeAllRuns(): Flow<List<AgentRunEntity>>

    @Query("SELECT * FROM agent_runs WHERE sessionId = :sessionId ORDER BY startedAt DESC")
    fun observeRunsForSession(sessionId: String): Flow<List<AgentRunEntity>>

    @Query("SELECT * FROM agent_runs WHERE id = :runId LIMIT 1")
    fun observeRun(runId: String): Flow<AgentRunEntity?>

    @Query("SELECT * FROM agent_runs WHERE id = :runId LIMIT 1")
    suspend fun getById(runId: String): AgentRunEntity?

    @Insert(onConflict = OnConflictStrategy.REPLACE)
    suspend fun upsert(run: AgentRunEntity)
}

@Dao
interface RunEventDao {
    @Query("SELECT * FROM run_events WHERE runId = :runId ORDER BY orderIndex ASC, createdAt ASC")
    fun observeEventsForRun(runId: String): Flow<List<RunEventEntity>>

    @Query("SELECT * FROM run_events WHERE runId = :runId ORDER BY orderIndex ASC, createdAt ASC")
    suspend fun getEventsForRun(runId: String): List<RunEventEntity>

    @Query("SELECT COUNT(*) FROM run_events WHERE runId = :runId")
    suspend fun countForRun(runId: String): Int

    @Insert(onConflict = OnConflictStrategy.REPLACE)
    suspend fun upsert(event: RunEventEntity)
}

@Database(
    entities = [
        ConversationSessionEntity::class,
        ChatMessageEntity::class,
        AppSettingsEntity::class,
        AgentRunEntity::class,
        RunEventEntity::class,
    ],
    version = 3,
    exportSchema = false,
)
abstract class AppDatabase : RoomDatabase() {
    abstract fun sessionDao(): SessionDao
    abstract fun chatMessageDao(): ChatMessageDao
    abstract fun appSettingsDao(): AppSettingsDao
    abstract fun agentRunDao(): AgentRunDao
    abstract fun runEventDao(): RunEventDao
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
    workspaceRoot = workspaceRoot,
    fakeScenario = FakeProviderScenario.valueOf(fakeScenario),
)

fun ProviderSettings.asEntity(): AppSettingsEntity = AppSettingsEntity(
    providerType = providerType.name,
    baseUrl = baseUrl,
    apiKey = apiKey,
    model = model,
    systemPrompt = systemPrompt,
    workspaceRoot = workspaceRoot,
    fakeScenario = fakeScenario.name,
)

fun AgentRunEntity.asDomain(): AgentRun = AgentRun(
    id = id,
    sessionId = sessionId,
    userMessageId = userMessageId,
    assistantMessageId = assistantMessageId,
    status = AgentRunStatus.valueOf(status),
    providerType = ProviderType.valueOf(providerType),
    model = model,
    baseUrlSnapshot = baseUrlSnapshot,
    startedAt = startedAt,
    completedAt = completedAt,
    durationMs = durationMs,
    errorSummary = errorSummary,
)

fun AgentRun.asEntity(): AgentRunEntity = AgentRunEntity(
    id = id,
    sessionId = sessionId,
    userMessageId = userMessageId,
    assistantMessageId = assistantMessageId,
    status = status.name,
    providerType = providerType.name,
    model = model,
    baseUrlSnapshot = baseUrlSnapshot,
    startedAt = startedAt,
    completedAt = completedAt,
    durationMs = durationMs,
    errorSummary = errorSummary,
)

fun RunEventEntity.asDomain(): RunEvent = RunEvent(
    id = id,
    runId = runId,
    type = RunEventType.valueOf(type),
    title = title,
    details = details,
    createdAt = createdAt,
    orderIndex = orderIndex,
)

fun RunEvent.asEntity(): RunEventEntity = RunEventEntity(
    id = id,
    runId = runId,
    type = type.name,
    title = title,
    details = details,
    createdAt = createdAt,
    orderIndex = orderIndex,
)
