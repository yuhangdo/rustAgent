package com.yuhangdo.rustagent.feature.chat

import com.google.common.truth.Truth.assertThat
import com.yuhangdo.rustagent.model.MessageRole
import org.junit.Test

class ChatReducerTest {
    private val reducer = ChatReducer()

    @Test
    fun reduce_snapshotLoaded_updatesMessagesAndSessionMetadata() {
        val initial = ChatContract.State()

        val reduced = reducer.reduce(
            previous = initial,
            mutation = ChatContract.Mutation.SnapshotLoaded(
                sessionId = "session-1",
                sessionTitle = "Build android-v1",
                messages = listOf(
                    ChatMessagePresentation(
                        id = "msg-1",
                        role = MessageRole.USER,
                        answerContent = "Build android-v1",
                        reasoningPreview = "",
                    ),
                ),
                providerTypeLabel = "Fake Provider",
                activeRunCount = 1,
            ),
        )

        assertThat(reduced.sessionId).isEqualTo("session-1")
        assertThat(reduced.sessionTitle).isEqualTo("Build android-v1")
        assertThat(reduced.messages).hasSize(1)
        assertThat(reduced.activeRunCount).isEqualTo(1)
    }
}
