package com.yuhangdo.rustagent.feature.chat.components

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.AssistChip
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import com.yuhangdo.rustagent.feature.chat.ChatMessagePresentation
import com.yuhangdo.rustagent.model.MessageRole
import com.yuhangdo.rustagent.model.summarizeReasoning
import androidx.compose.ui.unit.dp

@Composable
fun ChatMessageCard(
    message: ChatMessagePresentation,
    onOpenDeepThinking: (String) -> Unit,
) {
    val roleLabel = when (message.role) {
        MessageRole.USER -> "You"
        MessageRole.ASSISTANT -> "Agent"
        MessageRole.SYSTEM -> "System"
    }
    var showReasoning by rememberSaveable(message.id) { mutableStateOf(false) }

    Card(modifier = Modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    text = roleLabel,
                    style = MaterialTheme.typography.labelLarge,
                    color = MaterialTheme.colorScheme.primary,
                )
                if (message.runId != null && message.runStatus != null) {
                    AssistChip(
                        onClick = { onOpenDeepThinking(message.runId) },
                        label = {
                            Text(
                                text = message.durationMs?.let {
                                    "${message.runStatus.displayName} | ${it}ms"
                                } ?: message.runStatus.displayName,
                            )
                        },
                    )
                }
            }

            if (message.providerLabel != null) {
                Text(
                    text = message.providerLabel,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }

            if (message.reasoningPreview.isNotBlank()) {
                Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.SpaceBetween,
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Text(
                            text = "Deep Thinking Preview",
                            style = MaterialTheme.typography.labelMedium,
                            fontWeight = FontWeight.Bold,
                        )
                        TextButton(onClick = { showReasoning = !showReasoning }) {
                            Text(if (showReasoning) "Collapse" else "Expand")
                        }
                    }
                    Text(
                        text = if (showReasoning) {
                            message.reasoningPreview
                        } else {
                            summarizeReasoning(message.reasoningPreview, 120)
                        },
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }

            if (message.answerContent.isNotBlank()) {
                Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    Text(
                        text = "Answer",
                        style = MaterialTheme.typography.labelMedium,
                        fontWeight = FontWeight.Bold,
                    )
                    Text(
                        text = message.answerContent,
                        style = MaterialTheme.typography.bodyLarge,
                    )
                }
            }

            if (message.answerContent.isBlank() && message.reasoningPreview.isBlank()) {
                Text(
                    text = if (message.isStreaming) "Streaming response..." else "Waiting for provider output...",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }

            if (message.errorSummary != null) {
                Text(
                    text = message.errorSummary,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                )
            }

            if (message.runId != null) {
                TextButton(onClick = { onOpenDeepThinking(message.runId) }) {
                    Text("Deep Thinking")
                }
            }
        }
    }
}
