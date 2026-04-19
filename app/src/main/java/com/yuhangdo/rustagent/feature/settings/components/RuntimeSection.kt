package com.yuhangdo.rustagent.feature.settings.components

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.yuhangdo.rustagent.feature.settings.SettingsContract
import com.yuhangdo.rustagent.model.FakeProviderScenario
import com.yuhangdo.rustagent.model.ProviderSettings
import com.yuhangdo.rustagent.model.ProviderType

@Composable
fun RuntimeSection(
    settings: ProviderSettings,
    showWorkspaceRoot: Boolean,
    onIntent: (SettingsContract.Intent) -> Unit,
) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Text(
                text = "Runtime",
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.SemiBold,
            )
            OutlinedTextField(
                value = settings.systemPrompt,
                onValueChange = { onIntent(SettingsContract.Intent.SystemPromptChanged(it)) },
                modifier = Modifier.fillMaxWidth(),
                minLines = 4,
                maxLines = 8,
                label = { Text("System Prompt") },
            )
            if (settings.providerType == ProviderType.FAKE) {
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(
                        text = "Fake Scenario",
                        style = MaterialTheme.typography.labelLarge,
                    )
                    FakeProviderScenario.entries.forEach { scenario ->
                        OutlinedButton(
                            onClick = { onIntent(SettingsContract.Intent.FakeScenarioChanged(scenario)) },
                            modifier = Modifier.fillMaxWidth(),
                        ) {
                            Text(scenario.displayName)
                        }
                    }
                }
            }
            if (showWorkspaceRoot) {
                OutlinedTextField(
                    value = settings.workspaceRoot,
                    onValueChange = { onIntent(SettingsContract.Intent.WorkspaceRootChanged(it)) },
                    modifier = Modifier.fillMaxWidth(),
                    label = { Text("Workspace Root (Optional)") },
                    placeholder = { Text("/data/user/0/com.yuhangdo.rustagent/files") },
                )
            }
        }
    }
}
