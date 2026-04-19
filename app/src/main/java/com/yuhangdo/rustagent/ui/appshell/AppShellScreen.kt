package com.yuhangdo.rustagent.ui.appshell

import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.material3.BottomAppBar
import androidx.compose.material3.CenterAlignedTopAppBar
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier

@Composable
@OptIn(ExperimentalMaterial3Api::class)
fun AppShellScreen(
    state: AppShellContract.State,
    snackbarHostState: SnackbarHostState,
    onIntent: (AppShellContract.Intent) -> Unit,
    modifier: Modifier = Modifier,
    content: @Composable (PaddingValues) -> Unit,
) {
    Scaffold(
        modifier = modifier,
        topBar = {
            CenterAlignedTopAppBar(
                title = { Text(state.currentRoute.label) },
            )
        },
        bottomBar = {
            BottomAppBar {
                AppRoute.entries.forEach { route ->
                    TextButton(
                        onClick = { onIntent(AppShellContract.Intent.NavigateTo(route)) },
                    ) {
                        Text(route.label)
                    }
                }
            }
        },
        snackbarHost = {
            SnackbarHost(hostState = snackbarHostState)
        },
        content = content,
    )
}
