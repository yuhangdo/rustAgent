package com.yuhangdo.rustagent

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.BottomAppBar
import androidx.compose.material3.CenterAlignedTopAppBar
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.yuhangdo.rustagent.feature.chat.ChatScreen
import com.yuhangdo.rustagent.feature.chat.ChatViewModel
import com.yuhangdo.rustagent.feature.sessions.SessionsScreen
import com.yuhangdo.rustagent.feature.sessions.SessionsViewModel
import com.yuhangdo.rustagent.feature.settings.SettingsScreen
import com.yuhangdo.rustagent.feature.settings.SettingsViewModel
import com.yuhangdo.rustagent.ui.theme.RustAgentTheme

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            RustAgentTheme {
                Surface(
                    modifier = Modifier.fillMaxSize(),
                    color = MaterialTheme.colorScheme.background,
                ) {
                    RustAgentAppContent()
                }
            }
        }
    }
}

private enum class TopLevelDestination(
    val label: String,
) {
    Chat("Chat"),
    Sessions("Sessions"),
    Settings("Settings"),
}

@Composable
@OptIn(ExperimentalMaterial3Api::class)
private fun RustAgentAppContent() {
    val context = LocalContext.current
    val factory = remember(context) {
        (context.applicationContext as RustAgentApp).container.viewModelFactory
    }
    val chatViewModel: ChatViewModel = viewModel(factory = factory)
    val sessionsViewModel: SessionsViewModel = viewModel(factory = factory)
    val settingsViewModel: SettingsViewModel = viewModel(factory = factory)

    val chatState by chatViewModel.uiState.collectAsStateWithLifecycle()
    val sessionsState by sessionsViewModel.uiState.collectAsStateWithLifecycle()
    val settingsState by settingsViewModel.uiState.collectAsStateWithLifecycle()

    var destination by rememberSaveable { mutableStateOf(TopLevelDestination.Chat) }

    Scaffold(
        topBar = {
            CenterAlignedTopAppBar(
                title = { Text(destination.label) },
            )
        },
        bottomBar = {
            BottomAppBar {
                TopLevelDestination.entries.forEach { item ->
                    TextButton(onClick = { destination = item }) {
                        Text(item.label)
                    }
                }
            }
        },
    ) { innerPadding ->
        when (destination) {
            TopLevelDestination.Chat -> ChatScreen(
                uiState = chatState,
                onAction = chatViewModel::onAction,
                modifier = Modifier.padding(innerPadding),
            )

            TopLevelDestination.Sessions -> SessionsScreen(
                uiState = sessionsState,
                onAction = sessionsViewModel::onAction,
                onOpenChat = { destination = TopLevelDestination.Chat },
                modifier = Modifier.padding(innerPadding),
            )

            TopLevelDestination.Settings -> SettingsScreen(
                uiState = settingsState,
                onAction = settingsViewModel::onAction,
                modifier = Modifier.padding(innerPadding),
            )
        }
    }
}

