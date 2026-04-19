package com.yuhangdo.rustagent.feature.settings

import com.yuhangdo.rustagent.core.ui.mvi.Reducer

class SettingsReducer : Reducer<SettingsContract.State, SettingsContract.Mutation> {
    override fun reduce(
        previous: SettingsContract.State,
        mutation: SettingsContract.Mutation,
    ): SettingsContract.State = when (mutation) {
        is SettingsContract.Mutation.SettingsLoaded -> previous.copy(settings = mutation.settings)
        is SettingsContract.Mutation.SavingChanged -> previous.copy(isSaving = mutation.value)
    }
}
