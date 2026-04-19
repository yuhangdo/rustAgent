package com.yuhangdo.rustagent.feature.sessions

import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import kotlinx.coroutines.flow.collectLatest

@Composable
fun SessionsRoute(
    factory: ViewModelProvider.Factory,
    modifier: Modifier = Modifier,
    onOpenChat: () -> Unit,
    onGlobalMessage: (String) -> Unit = {},
) {
    val viewModel: SessionsViewModel = viewModel(factory = factory)
    val state by viewModel.uiState.collectAsStateWithLifecycle()

    LaunchedEffect(viewModel) {
        viewModel.effects.collectLatest { effect ->
            when (effect) {
                is SessionsContract.Effect.OpenChat -> onOpenChat()
                is SessionsContract.Effect.ShowSnackbar -> onGlobalMessage(effect.message)
            }
        }
    }

    SessionsScreen(
        state = state,
        onIntent = viewModel::onIntent,
        modifier = modifier,
    )
}
