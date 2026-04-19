package com.yuhangdo.rustagent.ui.appshell

import com.yuhangdo.rustagent.core.ui.mvi.UiEffect
import com.yuhangdo.rustagent.core.ui.mvi.UiIntent
import com.yuhangdo.rustagent.core.ui.mvi.UiState

object AppShellContract {
    data class State(
        val currentRoute: AppRoute = AppRoute.Chat,
    ) : UiState

    sealed interface Intent : UiIntent {
        data class NavigateTo(val route: AppRoute) : Intent
        data class ShowSnackbar(val message: String) : Intent
    }

    sealed interface Effect : UiEffect {
        data class ShowSnackbar(val message: String) : Effect
    }

    sealed interface Mutation {
        data class RouteChanged(val route: AppRoute) : Mutation
    }
}
