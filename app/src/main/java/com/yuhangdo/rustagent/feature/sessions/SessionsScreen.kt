package com.yuhangdo.rustagent.feature.sessions

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.yuhangdo.rustagent.feature.sessions.components.SessionCard

@Composable
fun SessionsScreen(
    state: SessionsContract.State,
    onIntent: (SessionsContract.Intent) -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier
            .fillMaxSize()
            .padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(
                text = "Sessions",
                style = MaterialTheme.typography.titleLarge,
                fontWeight = FontWeight.SemiBold,
            )
            Text(
                text = "Switch between threads and inspect the latest run health before jumping back into chat.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }

        Button(
            onClick = { onIntent(SessionsContract.Intent.CreateSession) },
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text("New Session")
        }

        if (state.sessions.isEmpty()) {
            Card(modifier = Modifier.fillMaxWidth()) {
                Text(
                    text = "No sessions yet. Create one here or send your first message on the Chat tab.",
                    modifier = Modifier.padding(20.dp),
                    style = MaterialTheme.typography.bodyMedium,
                )
            }
        } else {
            LazyColumn(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                items(
                    items = state.sessions,
                    key = { session -> session.id },
                ) { session ->
                    SessionCard(
                        session = session,
                        onSelect = { onIntent(SessionsContract.Intent.SelectSession(session.id)) },
                        onDelete = { onIntent(SessionsContract.Intent.DeleteSession(session.id)) },
                    )
                }
            }
        }
    }
}
