package com.yuhangdo.rustagent

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import com.yuhangdo.rustagent.feature.chat.ChatRoute
import com.yuhangdo.rustagent.feature.sessions.SessionsRoute
import com.yuhangdo.rustagent.feature.settings.SettingsRoute
import com.yuhangdo.rustagent.ui.appshell.AppRoute
import com.yuhangdo.rustagent.ui.appshell.AppShellRoute
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

@Composable
private fun RustAgentAppContent() {
    val context = LocalContext.current
    val app = context.applicationContext as RustAgentApp
    val factory = remember(context) {
        app.container.viewModelFactory
    }

    AppShellRoute(app = app) { route, innerPadding, showSnackbar, navigateTo ->
        when (route) {
            AppRoute.Chat -> ChatRoute(
                factory = factory,
                modifier = Modifier.padding(innerPadding),
                onGlobalMessage = showSnackbar,
            )

            AppRoute.Sessions -> SessionsRoute(
                factory = factory,
                modifier = Modifier.padding(innerPadding),
                onOpenChat = { navigateTo(AppRoute.Chat) },
                onGlobalMessage = showSnackbar,
            )

            AppRoute.Settings -> SettingsRoute(
                factory = factory,
                modifier = Modifier.padding(innerPadding),
                onGlobalMessage = showSnackbar,
            )
        }
    }
}

