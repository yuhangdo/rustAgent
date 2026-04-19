package com.yuhangdo.rustagent.core.ui.mvi

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

abstract class MviViewModel<Intent : UiIntent, State : UiState, Effect : UiEffect, Mutation>(
    initialState: State,
    private val reducer: Reducer<State, Mutation>,
) : ViewModel() {
    private val _uiState = MutableStateFlow(initialState)
    val uiState: StateFlow<State> = _uiState.asStateFlow()

    private val _effects = MutableSharedFlow<Effect>(extraBufferCapacity = 8)
    val effects: SharedFlow<Effect> = _effects.asSharedFlow()

    fun onIntent(intent: Intent) {
        handleIntent(intent)
    }

    protected abstract fun handleIntent(intent: Intent)

    protected fun mutate(mutation: Mutation) {
        _uiState.update { previous -> reducer.reduce(previous, mutation) }
    }

    protected fun launchMutation(block: suspend () -> Mutation) {
        viewModelScope.launch {
            mutate(block())
        }
    }

    protected fun launchMutationCollection(collector: suspend (emit: (Mutation) -> Unit) -> Unit) {
        viewModelScope.launch {
            collector(::mutate)
        }
    }

    protected fun launchEffect(effect: Effect) {
        viewModelScope.launch {
            _effects.emit(effect)
        }
    }
}
