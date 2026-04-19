package com.yuhangdo.rustagent.ui.appshell

import com.yuhangdo.rustagent.core.ui.mvi.MviViewModel

class AppShellViewModel : MviViewModel<
    AppShellContract.Intent,
    AppShellContract.State,
    AppShellContract.Effect,
    AppShellContract.Mutation
>(
    initialState = AppShellContract.State(),
    reducer = AppShellReducer(),
) {
    override fun handleIntent(intent: AppShellContract.Intent) {
        when (intent) {
            is AppShellContract.Intent.NavigateTo -> mutate(
                AppShellContract.Mutation.RouteChanged(intent.route),
            )

            is AppShellContract.Intent.ShowSnackbar -> launchEffect(
                AppShellContract.Effect.ShowSnackbar(intent.message),
            )
        }
    }
}
