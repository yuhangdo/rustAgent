package com.yuhangdo.rustagent.data.provider

import com.yuhangdo.rustagent.model.ChatMessage
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
    }
}

class FakeChatProvider : ChatProvider {
    override fun streamReply(request: ProviderRequest): Flow<ProviderChunk> = flow {
        val latestPrompt = request.history.lastOrNull { it.role == MessageRole.USER }?.answerContent.orEmpty()
        val reasoning = buildString {
            append("Routing request through the fake provider. ")
            append("It keeps reasoningContent and answerContent separate for UI and persistence tests. ")
            if (latestPrompt.isNotBlank()) {
                append("Latest prompt: ")
                append(latestPrompt.take(64))
                append(".")
            }
        }
        val answer = buildString {
            append("This is a fake answer stream. ")
            append("Swap the provider to OpenAI-compatible in Settings when real credentials are ready. ")
            if (latestPrompt.isNotBlank()) {
                append("Prompt summary: ")
                append(latestPrompt.take(96))
            }
        }

        emit(ProviderChunk(reasoningDelta = reasoning))
        delay(180)
        emit(ProviderChunk(answerDelta = answer))
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

        val requestUrl = settings.baseUrl.trimEnd('/') + "/chat/completions"
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

    private fun ChatMessage.asProviderContent(): String = buildString {
        if (reasoningContent.isNotBlank()) {
            append("Reasoning:\n")
            append(reasoningContent.trim())
            append("\n\n")
        }
        append(answerContent.trim())
    }.trim()

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
}

