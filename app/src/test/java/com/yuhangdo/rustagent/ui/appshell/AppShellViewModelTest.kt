package com.yuhangdo.rustagent.ui.appshell

import app.cash.turbine.test
import com.google.common.truth.Truth.assertThat
import com.yuhangdo.rustagent.MainDispatcherRule
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.runTest
import org.junit.Rule
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class AppShellViewModelTest {
    @get:Rule
    val mainDispatcherRule = MainDispatcherRule()

    @Test
    fun initialState_defaultsToChatRoute() {
        val viewModel = AppShellViewModel()

        assertThat(viewModel.uiState.value.currentRoute).isEqualTo(AppRoute.Chat)
    }

    @Test
    fun navigateTo_updatesState() {
        val viewModel = AppShellViewModel()

        viewModel.onIntent(AppShellContract.Intent.NavigateTo(AppRoute.Settings))

        assertThat(viewModel.uiState.value.currentRoute).isEqualTo(AppRoute.Settings)
    }

    @Test
    fun showSnackbar_emitsOneOffEffect() = runTest {
        val viewModel = AppShellViewModel()

        viewModel.effects.test {
            viewModel.onIntent(AppShellContract.Intent.ShowSnackbar("Provider profile saved."))

            assertThat(awaitItem()).isEqualTo(
                AppShellContract.Effect.ShowSnackbar("Provider profile saved."),
            )
            cancelAndIgnoreRemainingEvents()
        }
    }
}
