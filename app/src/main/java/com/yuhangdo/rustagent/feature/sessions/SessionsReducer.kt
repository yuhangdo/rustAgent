package com.yuhangdo.rustagent.feature.sessions

import com.yuhangdo.rustagent.core.ui.mvi.Reducer

class SessionsReducer : Reducer<SessionsContract.State, SessionsContract.Mutation> {
    override fun reduce(
        previous: SessionsContract.State,
        mutation: SessionsContract.Mutation,
    ): SessionsContract.State = when (mutation) {
        is SessionsContract.Mutation.SnapshotLoaded -> previous.copy(
            sessions = mutation.sessions,
            selectedSessionId = mutation.selectedSessionId,
        )
    }
}
