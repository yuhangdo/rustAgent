package com.yuhangdo.rustagent.core.ui.mvi

fun interface Reducer<State : UiState, Mutation> {
    fun reduce(previous: State, mutation: Mutation): State
}
