package com.yuhangdo.rustagent.feature.chat.components

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.Button
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier

@Composable
fun ChatComposerBar(
    draftMessage: String,
    isSending: Boolean,
    onDraftChanged: (String) -> Unit,
    onSendClicked: () -> Unit,
) {
    OutlinedTextField(
        value = draftMessage,
        onValueChange = onDraftChanged,
        modifier = Modifier.fillMaxWidth(),
        minLines = 3,
        maxLines = 5,
        label = { Text("Ask the agent") },
        placeholder = { Text("Describe the bug, provider issue, or debugging task...") },
    )

    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.End,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Button(
            onClick = onSendClicked,
            enabled = draftMessage.isNotBlank() && !isSending,
        ) {
            Text("Send")
        }
    }
}
