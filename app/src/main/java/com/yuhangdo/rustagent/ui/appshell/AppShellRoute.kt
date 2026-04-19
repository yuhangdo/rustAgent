package com.yuhangdo.rustagent.ui.appshell

import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.yuhangdo.rustagent.RustAgentApp
import kotlinx.coroutines.flow.collectLatest

@Composable
fun AppShellRoute(
    app: RustAgentApp,
    modifier: Modifier = Modifier,
    content: @Composable (AppRoute, PaddingValues, (String) -> Unit, (AppRoute) -> Unit) -> Unit,
) {
    val shellViewModel: AppShellViewModel = viewModel(factory = app.container.viewModelFactory)
    val state by shellViewModel.uiState.collectAsStateWithLifecycle()
    val snackbarHostState = remember { androidx.compose.material3.SnackbarHostState() }

    LaunchedEffect(shellViewModel) {
        shellViewModel.effects.collectLatest { effect ->
            when (effect) {
                is AppShellContract.Effect.ShowSnackbar -> snackbarHostState.showSnackbar(effect.message)
            }
        }
    }

    AppShellScreen(
        state = state,
        snackbarHostState = snackbarHostState,
        onIntent = shellViewModel::onIntent,
        modifier = modifier,
    ) { innerPadding ->
        content(
            state.currentRoute,
            innerPadding,
            { message -> shellViewModel.onIntent(AppShellContract.Intent.ShowSnackbar(message)) },
            { route -> shellViewModel.onIntent(AppShellContract.Intent.NavigateTo(route)) },
        )
    }
}
