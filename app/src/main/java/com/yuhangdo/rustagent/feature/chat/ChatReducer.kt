package com.yuhangdo.rustagent.feature.chat

import com.yuhangdo.rustagent.core.ui.mvi.Reducer

class ChatReducer : Reducer<ChatContract.State, ChatContract.Mutation> {
    override fun reduce(
        previous: ChatContract.State,
        mutation: ChatContract.Mutation,
    ): ChatContract.State = when (mutation) {
        is ChatContract.Mutation.SnapshotLoaded -> previous.copy(
            sessionId = mutation.sessionId,
            sessionTitle = mutation.sessionTitle,
            messages = mutation.messages,
            providerTypeLabel = mutation.providerTypeLabel,
            activeRunCount = mutation.activeRunCount,
        )

        is ChatContract.Mutation.DraftChanged -> previous.copy(draftMessage = mutation.value)
        is ChatContract.Mutation.SendingChanged -> previous.copy(isSending = mutation.value)
        is ChatContract.Mutation.ErrorChanged -> previous.copy(errorMessage = mutation.value)
        is ChatContract.Mutation.DeepThinkingChanged -> previous.copy(deepThinkingPanel = mutation.panel)
    }
}
