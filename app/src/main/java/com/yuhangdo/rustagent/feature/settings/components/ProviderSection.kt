package com.yuhangdo.rustagent.feature.settings.components

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.yuhangdo.rustagent.feature.settings.SettingsContract
import com.yuhangdo.rustagent.model.ProviderSettings
import com.yuhangdo.rustagent.model.ProviderType

@Composable
fun ProviderSection(
    settings: ProviderSettings,
    onIntent: (SettingsContract.Intent) -> Unit,
) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Text(
                text = "Provider",
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.SemiBold,
            )
            ProviderType.entries.forEach { providerType ->
                val selected = providerType == settings.providerType
                if (selected) {
                    Button(
                        onClick = { onIntent(SettingsContract.Intent.ProviderTypeChanged(providerType)) },
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Text(providerType.displayName)
                    }
                } else {
                    OutlinedButton(
                        onClick = { onIntent(SettingsContract.Intent.ProviderTypeChanged(providerType)) },
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Text(providerType.displayName)
                    }
                }
            }
        }
    }
}
