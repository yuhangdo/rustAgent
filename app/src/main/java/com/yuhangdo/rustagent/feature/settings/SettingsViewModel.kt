package com.yuhangdo.rustagent.feature.settings

import androidx.lifecycle.viewModelScope
import com.yuhangdo.rustagent.core.ui.mvi.MviViewModel
import com.yuhangdo.rustagent.data.repository.SettingsRepository
import kotlinx.coroutines.launch

class SettingsViewModel(
    private val settingsRepository: SettingsRepository,
) : MviViewModel<
    SettingsContract.Intent,
    SettingsContract.State,
    SettingsContract.Effect,
    SettingsContract.Mutation
>(
    initialState = SettingsContract.State(),
    reducer = SettingsReducer(),
) {
    private var hasUnsavedChanges = false

    init {
        viewModelScope.launch {
            val storedSettings = settingsRepository.getSettings()
            if (!hasUnsavedChanges) {
                mutate(SettingsContract.Mutation.SettingsLoaded(storedSettings))
            }
        }
    }

    override fun handleIntent(intent: SettingsContract.Intent) {
        val current = uiState.value.settings
        when (intent) {
            is SettingsContract.Intent.ProviderTypeChanged -> {
                hasUnsavedChanges = true
                mutate(SettingsContract.Mutation.SettingsLoaded(current.copy(providerType = intent.providerType)))
            }

            is SettingsContract.Intent.FakeScenarioChanged -> {
                hasUnsavedChanges = true
                mutate(SettingsContract.Mutation.SettingsLoaded(current.copy(fakeScenario = intent.scenario)))
            }

            is SettingsContract.Intent.BaseUrlChanged -> {
                hasUnsavedChanges = true
                mutate(SettingsContract.Mutation.SettingsLoaded(current.copy(baseUrl = intent.value)))
            }

            is SettingsContract.Intent.ApiKeyChanged -> {
                hasUnsavedChanges = true
                mutate(SettingsContract.Mutation.SettingsLoaded(current.copy(apiKey = intent.value)))
            }

            is SettingsContract.Intent.ModelChanged -> {
                hasUnsavedChanges = true
                mutate(SettingsContract.Mutation.SettingsLoaded(current.copy(model = intent.value)))
            }

            is SettingsContract.Intent.SystemPromptChanged -> {
                hasUnsavedChanges = true
                mutate(SettingsContract.Mutation.SettingsLoaded(current.copy(systemPrompt = intent.value)))
            }

            is SettingsContract.Intent.WorkspaceRootChanged -> {
                hasUnsavedChanges = true
                mutate(SettingsContract.Mutation.SettingsLoaded(current.copy(workspaceRoot = intent.value)))
            }

            SettingsContract.Intent.SaveClicked -> save()
        }
    }

    private fun save() {
        viewModelScope.launch {
            mutate(SettingsContract.Mutation.SavingChanged(true))
            settingsRepository.updateSettings(uiState.value.settings)
            hasUnsavedChanges = false
            launchEffect(SettingsContract.Effect.ShowSnackbar("Provider profile saved."))
            mutate(SettingsContract.Mutation.SavingChanged(false))
        }
    }
}
