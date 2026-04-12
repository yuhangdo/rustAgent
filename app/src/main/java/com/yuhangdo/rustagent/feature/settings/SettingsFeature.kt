package com.yuhangdo.rustagent.feature.settings

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.yuhangdo.rustagent.data.repository.SettingsRepository
import com.yuhangdo.rustagent.model.ProviderSettings
import com.yuhangdo.rustagent.model.ProviderType
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.collect
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch

data class SettingsUiState(
    val settings: ProviderSettings = ProviderSettings(),
    val isSaving: Boolean = false,
    val bannerMessage: String? = null,
)

sealed interface SettingsAction {
    data class ProviderTypeChanged(val providerType: ProviderType) : SettingsAction
    data class BaseUrlChanged(val value: String) : SettingsAction
    data class ApiKeyChanged(val value: String) : SettingsAction
    data class ModelChanged(val value: String) : SettingsAction
    data class SystemPromptChanged(val value: String) : SettingsAction
    data object SaveClicked : SettingsAction
}

class SettingsViewModel(
    private val settingsRepository: SettingsRepository,
) : ViewModel() {
    private val draftSettings = MutableStateFlow(ProviderSettings())
    private val isSaving = MutableStateFlow(false)
    private val bannerMessage = MutableStateFlow<String?>(null)

    val uiState: StateFlow<SettingsUiState> = combine(
        draftSettings,
        isSaving,
        bannerMessage,
    ) { draft, saving, banner ->
        SettingsUiState(
            settings = draft,
            isSaving = saving,
            bannerMessage = banner,
        )
    }.stateIn(
        scope = viewModelScope,
        started = SharingStarted.WhileSubscribed(5_000),
        initialValue = SettingsUiState(),
    )

    init {
        viewModelScope.launch {
            settingsRepository.observeSettings().collect { settings ->
                draftSettings.value = settings
            }
        }
    }

    fun onAction(action: SettingsAction) {
        when (action) {
            is SettingsAction.ProviderTypeChanged -> {
                draftSettings.value = draftSettings.value.copy(providerType = action.providerType)
            }
            is SettingsAction.BaseUrlChanged -> {
                draftSettings.value = draftSettings.value.copy(baseUrl = action.value)
            }
            is SettingsAction.ApiKeyChanged -> {
                draftSettings.value = draftSettings.value.copy(apiKey = action.value)
            }
            is SettingsAction.ModelChanged -> {
                draftSettings.value = draftSettings.value.copy(model = action.value)
            }
            is SettingsAction.SystemPromptChanged -> {
                draftSettings.value = draftSettings.value.copy(systemPrompt = action.value)
            }
            SettingsAction.SaveClicked -> save()
        }
    }

    private fun save() {
        viewModelScope.launch {
            isSaving.value = true
            settingsRepository.updateSettings(draftSettings.value)
            bannerMessage.value = "Provider settings saved."
            isSaving.value = false
        }
    }
}

@Composable
fun SettingsScreen(
    uiState: SettingsUiState,
    onAction: (SettingsAction) -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier
            .fillMaxSize()
            .padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(
                text = "Settings",
                style = MaterialTheme.typography.titleLarge,
                fontWeight = FontWeight.SemiBold,
            )
            Text(
                text = "Switch fake and real providers without changing the MVI or UI layers.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }

        if (uiState.bannerMessage != null) {
            Card(modifier = Modifier.fillMaxWidth()) {
                Text(
                    text = uiState.bannerMessage,
                    modifier = Modifier.padding(16.dp),
                    style = MaterialTheme.typography.bodyMedium,
                )
            }
        }

        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            ProviderType.entries.forEach { providerType ->
                val isSelected = providerType == uiState.settings.providerType
                if (isSelected) {
                    Button(onClick = { onAction(SettingsAction.ProviderTypeChanged(providerType)) }) {
                        Text(providerType.displayName)
                    }
                } else {
                    OutlinedButton(onClick = { onAction(SettingsAction.ProviderTypeChanged(providerType)) }) {
                        Text(providerType.displayName)
                    }
                }
            }
        }

        OutlinedTextField(
            value = uiState.settings.baseUrl,
            onValueChange = { onAction(SettingsAction.BaseUrlChanged(it)) },
            modifier = Modifier.fillMaxWidth(),
            label = { Text("Base URL") },
            placeholder = { Text("https://api.openai.com/v1") },
        )

        OutlinedTextField(
            value = uiState.settings.apiKey,
            onValueChange = { onAction(SettingsAction.ApiKeyChanged(it)) },
            modifier = Modifier.fillMaxWidth(),
            label = { Text("API Key") },
            visualTransformation = PasswordVisualTransformation(),
        )

        OutlinedTextField(
            value = uiState.settings.model,
            onValueChange = { onAction(SettingsAction.ModelChanged(it)) },
            modifier = Modifier.fillMaxWidth(),
            label = { Text("Model") },
        )

        OutlinedTextField(
            value = uiState.settings.systemPrompt,
            onValueChange = { onAction(SettingsAction.SystemPromptChanged(it)) },
            modifier = Modifier.fillMaxWidth(),
            minLines = 4,
            maxLines = 8,
            label = { Text("System Prompt") },
        )

        Button(
            onClick = { onAction(SettingsAction.SaveClicked) },
            enabled = !uiState.isSaving,
        ) {
            Text(if (uiState.isSaving) "Saving..." else "Save Settings")
        }
    }
}
