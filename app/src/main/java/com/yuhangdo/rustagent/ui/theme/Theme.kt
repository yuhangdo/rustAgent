package com.yuhangdo.rustagent.ui.theme

import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable

private val LightColorScheme = lightColorScheme(
    primary = Clay,
    secondary = Moss,
    tertiary = Cinder,
    background = Cloud,
    surface = Cloud,
    surfaceVariant = Sand,
    onPrimary = Cloud,
    onSecondary = Cloud,
    onBackground = Cinder,
    onSurface = Cinder,
)

@Composable
fun RustAgentTheme(
    content: @Composable () -> Unit,
) {
    MaterialTheme(
        colorScheme = LightColorScheme,
        typography = AppTypography,
        content = content,
    )
}

