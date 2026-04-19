package com.yuhangdo.rustagent.ui.appshell

import com.google.common.truth.Truth.assertThat
import org.junit.Test

class AppShellReducerTest {
    private val reducer = AppShellReducer()

    @Test
    fun reduce_routeSelected_updatesCurrentRoute() {
        val initial = AppShellContract.State()

        val reduced = reducer.reduce(
            previous = initial,
            mutation = AppShellContract.Mutation.RouteChanged(AppRoute.Settings),
        )

        assertThat(reduced.currentRoute).isEqualTo(AppRoute.Settings)
    }
}
