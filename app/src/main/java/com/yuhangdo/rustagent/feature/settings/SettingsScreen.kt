package com.yuhangdo.rustagent.feature.settings

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.yuhangdo.rustagent.feature.settings.components.ConnectionSection
import com.yuhangdo.rustagent.feature.settings.components.ProviderSection
import com.yuhangdo.rustagent.feature.settings.components.RuntimeSection
import com.yuhangdo.rustagent.model.ProviderType

@Composable
fun SettingsScreen(
    state: SettingsContract.State,
    onIntent: (SettingsContract.Intent) -> Unit,
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
                text = "Settings Studio",
                style = MaterialTheme.typography.titleLarge,
                fontWeight = FontWeight.SemiBold,
            )
            Text(
                text = "Configure model connectivity, runtime behavior, and embedded agent options from one workspace.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }

        ProviderSection(
            settings = state.settings,
            onIntent = onIntent,
        )

        ConnectionSection(
            settings = state.settings,
            onIntent = onIntent,
        )

        RuntimeSection(
            settings = state.settings,
            showWorkspaceRoot = state.settings.providerType == ProviderType.EMBEDDED_RUST_AGENT,
            onIntent = onIntent,
        )

        Button(
            onClick = { onIntent(SettingsContract.Intent.SaveClicked) },
            enabled = !state.isSaving,
        ) {
            Text(if (state.isSaving) "Saving..." else "Save Settings")
        }
    }
}
