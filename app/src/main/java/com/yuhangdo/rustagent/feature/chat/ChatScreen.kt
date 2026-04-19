package com.yuhangdo.rustagent.feature.chat

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material3.AssistChip
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.yuhangdo.rustagent.feature.chat.components.ChatComposerBar
import com.yuhangdo.rustagent.feature.chat.components.ChatMessageCard
import com.yuhangdo.rustagent.feature.chat.components.DeepThinkingPanel

@Composable
fun ChatScreen(
    state: ChatContract.State,
    onIntent: (ChatContract.Intent) -> Unit,
    modifier: Modifier = Modifier,
) {
    val listState = rememberLazyListState()

    LaunchedEffect(state.messages.size) {
        if (state.messages.isNotEmpty()) {
            listState.animateScrollToItem(state.messages.lastIndex)
        }
    }

    Surface(
        modifier = modifier
            .fillMaxSize()
            .background(
                Brush.verticalGradient(
                    colors = listOf(
                        MaterialTheme.colorScheme.surface,
                        MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.35f),
                    ),
                ),
            ),
    ) {
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.SpaceBetween,
            ) {
                Column(
                    modifier = Modifier.weight(1f),
                    verticalArrangement = Arrangement.spacedBy(4.dp),
                ) {
                    Text(
                        text = state.sessionTitle,
                        style = MaterialTheme.typography.titleLarge,
                        fontWeight = FontWeight.SemiBold,
                    )
                    Text(
                        text = if (state.activeRunCount > 0) {
                            "${state.activeRunCount} run(s) active. Deep thinking stays attached to each reply."
                        } else {
                            "Chat first. Deep thinking and run trace stay attached to each reply."
                        },
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                Column(
                    horizontalAlignment = Alignment.End,
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    AssistChip(
                        onClick = { },
                        label = { Text(state.providerTypeLabel) },
                    )
                    if (state.activeRunCount > 0) {
                        AssistChip(
                            onClick = { },
                            label = { Text("Streaming") },
                        )
                    }
                }
            }

            if (state.errorMessage != null) {
                Card(modifier = Modifier.fillMaxWidth()) {
                    Text(
                        text = state.errorMessage,
                        modifier = Modifier.padding(14.dp),
                        color = MaterialTheme.colorScheme.error,
                        style = MaterialTheme.typography.bodyMedium,
                    )
                }
            }

            if (state.messages.isEmpty()) {
                Card(modifier = Modifier.weight(1f)) {
                    Column(
                        modifier = Modifier.padding(20.dp),
                        verticalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        Text(
                            text = "Talk to the agent, inspect the reasoning trail",
                            style = MaterialTheme.typography.titleMedium,
                            fontWeight = FontWeight.SemiBold,
                        )
                        Text(
                            text = "Replies stream into the transcript, while deep thinking keeps the run trace, provider steps, and diagnostics one tap away.",
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            } else {
                LazyColumn(
                    state = listState,
                    modifier = Modifier.weight(1f),
                    verticalArrangement = Arrangement.spacedBy(10.dp),
                ) {
                    items(
                        items = state.messages,
                        key = { message -> message.id },
                    ) { message ->
                        ChatMessageCard(
                            message = message,
                            onOpenDeepThinking = { runId ->
                                onIntent(ChatContract.Intent.OpenDeepThinking(runId))
                            },
                        )
                    }
                }
            }

            if (state.isSending) {
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.End,
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    CircularProgressIndicator(
                        modifier = Modifier.size(22.dp),
                        strokeWidth = 2.dp,
                    )
                    Spacer(modifier = Modifier.size(12.dp))
                    Text(
                        text = "Agent is working...",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }

            ChatComposerBar(
                draftMessage = state.draftMessage,
                isSending = state.isSending,
                onDraftChanged = { onIntent(ChatContract.Intent.DraftChanged(it)) },
                onSendClicked = { onIntent(ChatContract.Intent.SendClicked) },
            )
        }
    }

    state.deepThinkingPanel?.let { panel ->
        DeepThinkingPanel(
            panel = panel,
            onDismiss = { onIntent(ChatContract.Intent.CloseDeepThinking) },
            onRetry = { onIntent(ChatContract.Intent.RetryRun(panel.runId)) },
            onCancel = { onIntent(ChatContract.Intent.CancelRun(panel.runId)) },
        )
    }
}
