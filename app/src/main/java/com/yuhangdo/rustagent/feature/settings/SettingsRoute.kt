package com.yuhangdo.rustagent.feature.settings

import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import kotlinx.coroutines.flow.collectLatest

@Composable
fun SettingsRoute(
    factory: ViewModelProvider.Factory,
    modifier: Modifier = Modifier,
    onGlobalMessage: (String) -> Unit = {},
) {
    val viewModel: SettingsViewModel = viewModel(factory = factory)
    val state by viewModel.uiState.collectAsStateWithLifecycle()

    LaunchedEffect(viewModel) {
        viewModel.effects.collectLatest { effect ->
            when (effect) {
                is SettingsContract.Effect.ShowSnackbar -> onGlobalMessage(effect.message)
            }
        }
    }

    SettingsScreen(
        state = state,
        onIntent = viewModel::onIntent,
        modifier = modifier,
    )
}
