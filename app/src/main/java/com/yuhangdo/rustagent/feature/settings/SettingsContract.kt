package com.yuhangdo.rustagent.feature.settings

import com.yuhangdo.rustagent.core.ui.mvi.UiEffect
import com.yuhangdo.rustagent.core.ui.mvi.UiIntent
import com.yuhangdo.rustagent.core.ui.mvi.UiState
import com.yuhangdo.rustagent.model.FakeProviderScenario
import com.yuhangdo.rustagent.model.ProviderSettings
import com.yuhangdo.rustagent.model.ProviderType

object SettingsContract {
    data class State(
        val settings: ProviderSettings = ProviderSettings(),
        val isSaving: Boolean = false,
    ) : UiState

    sealed interface Intent : UiIntent {
        data class ProviderTypeChanged(val providerType: ProviderType) : Intent
        data class FakeScenarioChanged(val scenario: FakeProviderScenario) : Intent
        data class BaseUrlChanged(val value: String) : Intent
        data class ApiKeyChanged(val value: String) : Intent
        data class ModelChanged(val value: String) : Intent
        data class SystemPromptChanged(val value: String) : Intent
        data class WorkspaceRootChanged(val value: String) : Intent
        data object SaveClicked : Intent
    }

    sealed interface Effect : UiEffect {
        data class ShowSnackbar(val message: String) : Effect
    }

    sealed interface Mutation {
        data class SettingsLoaded(val settings: ProviderSettings) : Mutation
        data class SavingChanged(val value: Boolean) : Mutation
    }
}
