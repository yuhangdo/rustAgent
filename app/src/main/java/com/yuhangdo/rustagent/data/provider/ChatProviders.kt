package com.yuhangdo.rustagent.data.provider

import com.yuhangdo.rustagent.model.ChatMessage
import com.yuhangdo.rustagent.model.FakeProviderScenario
import com.yuhangdo.rustagent.model.MessageRole
import com.yuhangdo.rustagent.model.ProviderChunk
import com.yuhangdo.rustagent.model.ProviderRequest
import com.yuhangdo.rustagent.model.ProviderSettings
import com.yuhangdo.rustagent.model.ProviderType
import com.yuhangdo.rustagent.model.apiValue
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

fun interface ChatProviderResolver {
    fun resolve(settings: ProviderSettings): ChatProvider
}

interface ChatProvider {
    fun streamReply(request: ProviderRequest): Flow<ProviderChunk>
}

class ChatProviderFactory(
    private val okHttpClient: OkHttpClient,
) : ChatProviderResolver {
    private val fakeProvider = FakeChatProvider()
    private val openAiCompatibleProvider = OpenAiCompatibleChatProvider(okHttpClient)

    override fun resolve(settings: ProviderSettings): ChatProvider = when (settings.providerType) {
        ProviderType.FAKE -> fakeProvider
        ProviderType.OPENAI_COMPATIBLE -> openAiCompatibleProvider
        // Legacy provider wiring has no embedded Rust implementation.
        // Callers that still use this factory fall back to the HTTP provider.
        ProviderType.EMBEDDED_RUST_AGENT -> openAiCompatibleProvider
    }
}

class FakeChatProvider : ChatProvider {
    override fun streamReply(request: ProviderRequest): Flow<ProviderChunk> = flow {
        val latestPrompt = request.history.lastOrNull { it.role == MessageRole.USER }?.answerContent.orEmpty()
        when (request.settings.fakeScenario) {
            FakeProviderScenario.SUCCESS_WITH_REASONING -> {
                val reasoning = buildString {
                    append("Routing request through the fake provider. ")
                    append("This run includes both reasoning and final answer output for console testing. ")
                    if (latestPrompt.isNotBlank()) {
                        append("Latest prompt: ")
                        append(latestPrompt.take(64))
                        append(".")
                    }
                }
                val answer = buildString {
                    append("Fake provider completed successfully. ")
                    append("Use this mode to verify the run timeline, reasoning summary, and final answer rendering. ")
                    if (latestPrompt.isNotBlank()) {
                        append("Prompt summary: ")
                        append(latestPrompt.take(96))
                    }
                }
                emit(ProviderChunk(reasoningDelta = reasoning))
                delay(180)
                emit(ProviderChunk(answerDelta = answer))
            }

            FakeProviderScenario.SUCCESS_ANSWER_ONLY -> {
                val answer = buildString {
                    append("Fake provider returned a final answer without reasoning output. ")
                    if (latestPrompt.isNotBlank()) {
                        append("Prompt summary: ")
                        append(latestPrompt.take(96))
                    }
                }
                emit(ProviderChunk(answerDelta = answer))
            }

            FakeProviderScenario.EMPTY_RESPONSE -> {
                delay(120)
            }

            FakeProviderScenario.DELAYED_SUCCESS -> {
                emit(
                    ProviderChunk(
                        reasoningDelta = "Fake provider is intentionally delayed. This helps validate long-running run states.",
                    ),
                )
                delay(1_300)
                emit(
                    ProviderChunk(
                        answerDelta = "Delayed fake provider completed successfully after a synthetic wait.",
                    ),
                )
            }

            FakeProviderScenario.PROVIDER_ERROR -> {
                delay(100)
                throw IOException("Synthetic fake-provider failure for console diagnostics.")
            }
        }
    }
}

class OpenAiCompatibleChatProvider(
    private val okHttpClient: OkHttpClient,
) : ChatProvider {

    override fun streamReply(request: ProviderRequest): Flow<ProviderChunk> = flow {
        val settings = request.settings
        if (settings.baseUrl.isBlank() || settings.apiKey.isBlank()) {
            emit(ProviderChunk(answerDelta = "OpenAI-compatible provider needs both base URL and API key."))
            return@flow
        }

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
                throw IOException("HTTP ${response.code}: ${body.take(300)}")
            }

            val message = JSONObject(body)
                .optJSONArray("choices")
                ?.optJSONObject(0)
                ?.optJSONObject("message")
                ?: throw IOException("Response missing choices[0].message")

            val reasoning = message.firstNonBlank("reasoning_content", "reasoningContent", "reasoning")
            val answer = message.extractAnswer()

            emit(
                ProviderChunk(
                    reasoningDelta = reasoning,
                    answerDelta = answer.ifBlank { "Provider returned an empty answer body." },
                ),
            )
        }
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
}

