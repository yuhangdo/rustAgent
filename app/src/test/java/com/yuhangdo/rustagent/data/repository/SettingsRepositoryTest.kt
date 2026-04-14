package com.yuhangdo.rustagent.data.repository

import com.google.common.truth.Truth.assertThat
import com.yuhangdo.rustagent.FakeAppSettingsDao
import com.yuhangdo.rustagent.model.FakeProviderScenario
import com.yuhangdo.rustagent.model.ProviderType
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.runTest
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class SettingsRepositoryTest {
    @Test
    fun updateSettings_updates_observed_value() = runTest {
        val repository = SettingsRepository(FakeAppSettingsDao())

        repository.updateSettings(
            repository.getSettings().copy(
                providerType = ProviderType.OPENAI_COMPATIBLE,
                baseUrl = "https://api.openai.com/v1",
                model = "gpt-4o-mini",
                fakeScenario = FakeProviderScenario.PROVIDER_ERROR,
            ),
        )

        val updated = repository.observeSettings().first()
        assertThat(updated.providerType).isEqualTo(ProviderType.OPENAI_COMPATIBLE)
        assertThat(updated.baseUrl).isEqualTo("https://api.openai.com/v1")
        assertThat(updated.fakeScenario).isEqualTo(FakeProviderScenario.PROVIDER_ERROR)
    }
}
