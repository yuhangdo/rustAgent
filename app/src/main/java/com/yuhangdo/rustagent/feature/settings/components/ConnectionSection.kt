package com.yuhangdo.rustagent.feature.settings.components

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import com.yuhangdo.rustagent.feature.settings.SettingsContract
import com.yuhangdo.rustagent.model.ProviderSettings

@Composable
fun ConnectionSection(
    settings: ProviderSettings,
    onIntent: (SettingsContract.Intent) -> Unit,
) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Text(
                text = "Model Connection",
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.SemiBold,
            )
            OutlinedTextField(
                value = settings.baseUrl,
                onValueChange = { onIntent(SettingsContract.Intent.BaseUrlChanged(it)) },
                modifier = Modifier.fillMaxWidth(),
                label = { Text("Base URL") },
                placeholder = { Text("https://api.openai.com/v1") },
            )
            OutlinedTextField(
                value = settings.apiKey,
                onValueChange = { onIntent(SettingsContract.Intent.ApiKeyChanged(it)) },
                modifier = Modifier.fillMaxWidth(),
                label = { Text("API Key") },
                visualTransformation = PasswordVisualTransformation(),
            )
            OutlinedTextField(
                value = settings.model,
                onValueChange = { onIntent(SettingsContract.Intent.ModelChanged(it)) },
                modifier = Modifier.fillMaxWidth(),
                label = { Text("Model") },
            )
        }
    }
}
