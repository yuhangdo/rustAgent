package com.yuhangdo.rustagent.feature.settings

import app.cash.turbine.test
import com.google.common.truth.Truth.assertThat
import com.yuhangdo.rustagent.FakeAppSettingsDao
import com.yuhangdo.rustagent.MainDispatcherRule
import com.yuhangdo.rustagent.data.repository.SettingsRepository
import com.yuhangdo.rustagent.model.ProviderType
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.Rule
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class SettingsViewModelTest {
    @get:Rule
    val mainDispatcherRule = MainDispatcherRule()

    @Test
    fun providerChanged_updatesDraftState() = runTest {
        val viewModel = SettingsViewModel(
            settingsRepository = SettingsRepository(FakeAppSettingsDao()),
        )

        viewModel.onIntent(SettingsContract.Intent.ProviderTypeChanged(ProviderType.EMBEDDED_RUST_AGENT))
        advanceUntilIdle()

        assertThat(viewModel.uiState.value.settings.providerType).isEqualTo(ProviderType.EMBEDDED_RUST_AGENT)
    }

    @Test
    fun save_emitsSnackbarEffect() = runTest {
        val viewModel = SettingsViewModel(
            settingsRepository = SettingsRepository(FakeAppSettingsDao()),
        )

        viewModel.effects.test {
            viewModel.onIntent(SettingsContract.Intent.SaveClicked)
            advanceUntilIdle()

            assertThat(awaitItem()).isEqualTo(
                SettingsContract.Effect.ShowSnackbar("Provider profile saved."),
            )
            cancelAndIgnoreRemainingEvents()
        }
    }
}
