package com.yuhangdo.rustagent.ui.appshell

import com.yuhangdo.rustagent.core.ui.mvi.Reducer

class AppShellReducer : Reducer<AppShellContract.State, AppShellContract.Mutation> {
    override fun reduce(
        previous: AppShellContract.State,
        mutation: AppShellContract.Mutation,
    ): AppShellContract.State = when (mutation) {
        is AppShellContract.Mutation.RouteChanged -> previous.copy(currentRoute = mutation.route)
    }
}
