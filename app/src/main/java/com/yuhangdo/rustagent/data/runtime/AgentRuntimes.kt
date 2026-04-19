package com.yuhangdo.rustagent.data.runtime

import android.app.Application
import com.yuhangdo.rustagent.model.ChatMessage
import com.yuhangdo.rustagent.model.FakeProviderScenario
import com.yuhangdo.rustagent.model.MessageRole
import com.yuhangdo.rustagent.model.ProviderSettings
import com.yuhangdo.rustagent.model.ProviderType
import com.yuhangdo.rustagent.model.RunEventType
import com.yuhangdo.rustagent.model.summarizeReasoning
import com.yuhangdo.rustagent.runtime.NativeRustRuntimeBridge
import com.yuhangdo.rustagent.runtime.RustEmbeddedRuntimeBridge
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flow
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import org.json.JSONArray
import org.json.JSONObject
import java.io.IOException

class AgentRuntimeFactory(
    application: Application,
    private val okHttpClient: OkHttpClient,
    private val nativeRustRuntimeBridge: NativeRustRuntimeBridge = RustEmbeddedRuntimeBridge,
) : AgentRuntimeResolver {
    private val fakeRuntime = FakeAgentRuntime()
    private val openAiCompatibleRuntime = OpenAiCompatibleAgentRuntime(okHttpClient)
    private val embeddedRustAgentRuntime = EmbeddedRustAgentRuntime(
        okHttpClient = okHttpClient,
        nativeRustRuntimeBridge = nativeRustRuntimeBridge,
        appStorageDir = application.applicationContext.filesDir.absolutePath,
    )

    override fun resolve(settings: ProviderSettings): AgentRuntime = when (settings.providerType) {
        ProviderType.FAKE -> fakeRuntime
        ProviderType.OPENAI_COMPATIBLE -> openAiCompatibleRuntime
        ProviderType.EMBEDDED_RUST_AGENT -> embeddedRustAgentRuntime
    }
}

class FakeAgentRuntime : AgentRuntime {
    override fun execute(request: AgentRuntimeRequest): Flow<AgentRuntimeEvent> = flow {
        emit(AgentRuntimeEvent.RunUpdate(RunEventType.STARTED, request.triggerLabel))
        emit(
            AgentRuntimeEvent.RunUpdate(
                RunEventType.REQUEST_BUILT,
                "Built prompt context from ${request.history.size} transcript messages.",
            ),
        )
        emit(
            AgentRuntimeEvent.RunUpdate(
                RunEventType.PROVIDER_SELECTED,
                "${request.settings.providerType.displayName} | ${request.settings.model}",
            ),
        )

        val latestPrompt = request.history.lastOrNull { it.role == MessageRole.USER }?.answerContent.orEmpty()
        when (request.settings.fakeScenario) {
            FakeProviderScenario.SUCCESS_WITH_REASONING -> {
                val reasoning = buildString {
                    append("Routing request through the fake runtime. ")
                    append("This run includes reasoning and answer output for embedded-console validation. ")
                    if (latestPrompt.isNotBlank()) {
                        append("Latest prompt: ")
                        append(latestPrompt.take(64))
                        append(".")
                    }
                }
                val answer = buildString {
                    append("Fake runtime completed successfully. ")
                    append("Use this mode to verify the run timeline and final answer rendering. ")
                    if (latestPrompt.isNotBlank()) {
                        append("Prompt summary: ")
                        append(latestPrompt.take(96))
                    }
                }
                emit(AgentRuntimeEvent.OutputUpdate(reasoningContent = reasoning, answerContent = ""))
                emit(AgentRuntimeEvent.RunUpdate(RunEventType.REASONING_SUMMARY, summarizeReasoning(reasoning, 400)))
                delay(180)
                emit(AgentRuntimeEvent.OutputUpdate(reasoningContent = reasoning, answerContent = answer))
                emit(AgentRuntimeEvent.RunUpdate(RunEventType.ANSWER_RECEIVED, answer.take(400)))
                emit(AgentRuntimeEvent.RunUpdate(RunEventType.COMPLETED, "Completed in 180ms."))
            }

            FakeProviderScenario.SUCCESS_ANSWER_ONLY -> {
                val answer = buildString {
                    append("Fake runtime returned a final answer without reasoning output. ")
                    if (latestPrompt.isNotBlank()) {
                        append("Prompt summary: ")
                        append(latestPrompt.take(96))
                    }
                }
                emit(AgentRuntimeEvent.OutputUpdate(reasoningContent = "", answerContent = answer))
                emit(AgentRuntimeEvent.RunUpdate(RunEventType.ANSWER_RECEIVED, answer.take(400)))
                emit(AgentRuntimeEvent.RunUpdate(RunEventType.COMPLETED, "Completed immediately."))
            }

            FakeProviderScenario.EMPTY_RESPONSE -> {
                val answer = "Provider returned an empty answer body."
                delay(120)
                emit(AgentRuntimeEvent.OutputUpdate(reasoningContent = "", answerContent = answer))
                emit(AgentRuntimeEvent.RunUpdate(RunEventType.ANSWER_RECEIVED, answer))
                emit(AgentRuntimeEvent.RunUpdate(RunEventType.COMPLETED, "Completed with an empty-response placeholder."))
            }

            FakeProviderScenario.DELAYED_SUCCESS -> {
                val reasoning = "Fake runtime is intentionally delayed. This helps validate long-running run states."
                emit(AgentRuntimeEvent.OutputUpdate(reasoningContent = reasoning, answerContent = ""))
                emit(AgentRuntimeEvent.RunUpdate(RunEventType.REASONING_SUMMARY, reasoning))
                delay(1_300)
                val answer = "Delayed fake runtime completed successfully after a synthetic wait."
                emit(AgentRuntimeEvent.OutputUpdate(reasoningContent = reasoning, answerContent = answer))
                emit(AgentRuntimeEvent.RunUpdate(RunEventType.ANSWER_RECEIVED, answer))
                emit(AgentRuntimeEvent.RunUpdate(RunEventType.COMPLETED, "Completed after an artificial delay."))
            }

            FakeProviderScenario.PROVIDER_ERROR -> {
                delay(100)
                emit(AgentRuntimeEvent.RunUpdate(RunEventType.FAILED, "Synthetic fake-runtime failure for console diagnostics."))
            }
        }
    }
}

class OpenAiCompatibleAgentRuntime(
    private val okHttpClient: OkHttpClient,
) : AgentRuntime {
    override fun execute(request: AgentRuntimeRequest): Flow<AgentRuntimeEvent> = flow {
        emit(AgentRuntimeEvent.RunUpdate(RunEventType.STARTED, request.triggerLabel))
        emit(
            AgentRuntimeEvent.RunUpdate(
                RunEventType.REQUEST_BUILT,
                "Built prompt context from ${request.history.size} transcript messages.",
            ),
        )
        emit(
            AgentRuntimeEvent.RunUpdate(
                RunEventType.PROVIDER_SELECTED,
                "${request.settings.providerType.displayName} | ${request.settings.model}",
            ),
        )

        val settings = request.settings
        if (settings.baseUrl.isBlank() || settings.apiKey.isBlank()) {
            emit(AgentRuntimeEvent.RunUpdate(RunEventType.FAILED, "OpenAI-compatible runtime needs both base URL and API key."))
            return@flow
        }

        try {
            val payload = JSONObject().apply {
                put("model", settings.model.ifBlank { "gpt-4o-mini" })
                put("stream", false)
                put("messages", JSONArray().apply {
                    put(
                        JSONObject().apply {
                            put("role", MessageRole.SYSTEM.apiValue)
                            put("content", settings.systemPrompt)
                        },
                    )
                    request.history.forEach { message ->
                        put(
                            JSONObject().apply {
                                put("role", message.role.apiValue)
                                put("content", message.asProviderContent())
                            },
                        )
                    }
                })
            }

            val requestUrl = buildChatCompletionsUrl(settings.baseUrl)
            val httpRequest = Request.Builder()
                .url(requestUrl)
                .header("Authorization", "Bearer ${settings.apiKey}")
                .header("Content-Type", "application/json")
                .post(payload.toString().toRequestBody("application/json; charset=utf-8".toMediaType()))
                .build()

            okHttpClient.newCall(httpRequest).execute().use { response ->
                val body = response.body?.string().orEmpty()
                if (!response.isSuccessful) {
                    emit(AgentRuntimeEvent.RunUpdate(RunEventType.FAILED, "HTTP ${response.code}: ${body.take(300)}"))
                    return@use
                }

                val message = JSONObject(body)
                    .optJSONArray("choices")
                    ?.optJSONObject(0)
                    ?.optJSONObject("message")
                    ?: throw IOException("Response missing choices[0].message")

                val reasoning = message.firstNonBlank("reasoning_content", "reasoningContent", "reasoning")
                var answer = message.extractAnswer()
                if (answer.isBlank()) {
                    answer = "Provider returned an empty answer body."
                }

                if (reasoning.isNotBlank()) {
                    emit(AgentRuntimeEvent.OutputUpdate(reasoningContent = reasoning, answerContent = ""))
                    emit(AgentRuntimeEvent.RunUpdate(RunEventType.REASONING_SUMMARY, summarizeReasoning(reasoning, 400)))
                }

                emit(AgentRuntimeEvent.OutputUpdate(reasoningContent = reasoning, answerContent = answer))
                emit(AgentRuntimeEvent.RunUpdate(RunEventType.ANSWER_RECEIVED, answer.take(400)))
                emit(AgentRuntimeEvent.RunUpdate(RunEventType.COMPLETED, "Completed in the OpenAI-compatible runtime."))
            }
        } catch (throwable: Throwable) {
            emit(AgentRuntimeEvent.RunUpdate(RunEventType.FAILED, throwable.message ?: "Unknown OpenAI-compatible runtime error."))
        }
    }
}

class EmbeddedRustAgentRuntime(
    private val okHttpClient: OkHttpClient,
    private val nativeRustRuntimeBridge: NativeRustRuntimeBridge,
    private val appStorageDir: String,
) : AgentRuntime {
    override fun execute(request: AgentRuntimeRequest): Flow<AgentRuntimeEvent> = flow {
        emit(AgentRuntimeEvent.RunUpdate(RunEventType.STARTED, request.triggerLabel))
        emit(
            AgentRuntimeEvent.RunUpdate(
                RunEventType.REQUEST_BUILT,
                "Built prompt context from ${request.history.size} transcript messages.",
            ),
        )
        emit(
            AgentRuntimeEvent.RunUpdate(
                RunEventType.PROVIDER_SELECTED,
                "${request.settings.providerType.displayName} | ${request.settings.model}",
            ),
        )

        try {
            val port = nativeRustRuntimeBridge.ensureServerStarted(appStorageDir)
            val baseUrl = "http://127.0.0.1:$port/api"
            val startResponse = postJson(
                url = "$baseUrl/runs",
                payload = request.toJson(),
            )
            startResponse.use { response ->
                if (!response.isSuccessful) {
                    throw IOException("Embedded runtime HTTP ${response.code}: ${response.body?.string().orEmpty().take(300)}")
                }
            }

            var deliveredEventCount = 0
            var lastReasoning = ""
            var lastAnswer = ""

            while (true) {
                delay(250)
                val snapshotResponse = getJson("$baseUrl/runs/${request.runId}")
                val snapshotBody = snapshotResponse.use { response ->
                    if (!response.isSuccessful) {
                        throw IOException("Snapshot HTTP ${response.code}: ${response.body?.string().orEmpty().take(300)}")
                    }
                    response.body?.string().orEmpty()
                }
                val snapshot = RuntimeRunSnapshot.fromJson(snapshotBody)

                snapshot.events.drop(deliveredEventCount).forEach { event ->
                    emit(
                        AgentRuntimeEvent.RunUpdate(
                            type = event.type,
                            title = event.title,
                            details = event.details,
                        ),
                    )
                }
                deliveredEventCount = snapshot.events.size

                if (snapshot.reasoningContent != lastReasoning || snapshot.answerContent != lastAnswer) {
                    lastReasoning = snapshot.reasoningContent
                    lastAnswer = snapshot.answerContent
                    emit(
                        AgentRuntimeEvent.OutputUpdate(
                            reasoningContent = snapshot.reasoningContent,
                            answerContent = snapshot.answerContent,
                        ),
                    )
                }

                when (snapshot.status) {
                    "COMPLETED", "FAILED", "CANCELLED" -> break
                }
            }
        } catch (throwable: Throwable) {
            emit(AgentRuntimeEvent.RunUpdate(RunEventType.FAILED, throwable.message ?: "Unknown embedded runtime error."))
        }
    }

    override suspend fun cancel(runId: String) {
        val port = nativeRustRuntimeBridge.ensureServerStarted(appStorageDir)
        val baseUrl = "http://127.0.0.1:$port/api"
        okHttpClient.newCall(
            Request.Builder()
                .url("$baseUrl/runs/$runId/cancel")
                .post("{}".toRequestBody("application/json; charset=utf-8".toMediaType()))
                .build(),
        ).execute().use { response ->
            if (!response.isSuccessful) {
                throw IOException("Cancel HTTP ${response.code}: ${response.body?.string().orEmpty().take(300)}")
            }
        }
    }

    private fun AgentRuntimeRequest.toJson(): JSONObject = JSONObject().apply {
        put("runId", runId)
        put("triggerLabel", triggerLabel)
        put("history", JSONArray().apply {
            history.forEach { message ->
                put(
                    JSONObject().apply {
                        put("role", message.role.apiValue)
                        put("content", message.asProviderContent())
                    },
                )
            }
        })
        put(
            "settings",
            JSONObject().apply {
                put("baseUrl", settings.baseUrl)
                put("apiKey", settings.apiKey)
                put("model", settings.model)
                put("systemPrompt", settings.systemPrompt)
            },
        )
        put(
            "workspaceRoot",
            settings.workspaceRoot.ifBlank { appStorageDir },
        )
    }

    private fun postJson(url: String, payload: JSONObject) = okHttpClient.newCall(
        Request.Builder()
            .url(url)
            .header("Content-Type", "application/json")
            .post(payload.toString().toRequestBody("application/json; charset=utf-8".toMediaType()))
            .build(),
    ).execute()

    private fun getJson(url: String) = okHttpClient.newCall(
        Request.Builder()
            .url(url)
            .get()
            .build(),
    ).execute()
}

private data class RuntimeRunSnapshot(
    val status: String,
    val reasoningContent: String,
    val answerContent: String,
    val events: List<RuntimeRunEvent>,
) {
    companion object {
        fun fromJson(raw: String): RuntimeRunSnapshot {
            val json = JSONObject(raw)
            val eventsArray = json.optJSONArray("events") ?: JSONArray()
            val events = buildList {
                for (index in 0 until eventsArray.length()) {
                    val eventJson = eventsArray.optJSONObject(index) ?: continue
                    val eventType = eventJson.optString("eventType").toRunEventType()
                    add(
                        RuntimeRunEvent(
                            type = eventType,
                            title = eventJson.optString("title").ifBlank { eventType.displayName },
                            details = eventJson.optString("details"),
                        ),
                    )
                }
            }
            return RuntimeRunSnapshot(
                status = json.optString("status"),
                reasoningContent = json.optString("reasoningContent"),
                answerContent = json.optString("answerContent"),
                events = events,
            )
        }
    }
}

private data class RuntimeRunEvent(
    val type: RunEventType,
    val title: String,
    val details: String,
)

private fun String.toRunEventType(): RunEventType = RunEventType.entries.firstOrNull { it.name == this }
    ?: RunEventType.FAILED

private val MessageRole.apiValue: String
    get() = when (this) {
        MessageRole.USER -> "user"
        MessageRole.ASSISTANT -> "assistant"
        MessageRole.SYSTEM -> "system"
    }

private fun ChatMessage.asProviderContent(): String = answerContent.trim()

private fun JSONObject.firstNonBlank(vararg keys: String): String {
    keys.forEach { key ->
        val value = optString(key)
        if (value.isNotBlank()) {
            return value
        }
    }
    return ""
}

private fun JSONObject.extractAnswer(): String {
    val directContent = optString("content")
    if (directContent.isNotBlank()) {
        return directContent
    }

    val contentArray = optJSONArray("content") ?: return ""
    return buildString {
        for (index in 0 until contentArray.length()) {
            val part = contentArray.optJSONObject(index) ?: continue
            append(part.optString("text"))
        }
        }.trim()
}

private fun buildChatCompletionsUrl(baseUrl: String): String {
    val trimmed = baseUrl.trimEnd('/')
    return when {
        trimmed.endsWith("/chat/completions") -> trimmed
        trimmed.endsWith("/v1") -> "$trimmed/chat/completions"
        else -> "$trimmed/v1/chat/completions"
    }
}
