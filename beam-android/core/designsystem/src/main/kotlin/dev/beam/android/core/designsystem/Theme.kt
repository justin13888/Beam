package dev.beam.android.core.designsystem

import android.os.Build
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.ColorScheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.platform.LocalContext

/**
 * The app's theme.
 *
 * Material 3 1.4.0 ships the expressive *tokens* and several expressive
 * components, but `MaterialExpressiveTheme` itself is still `internal` in the
 * stable artifact, so the theme is assembled through `MaterialTheme` with the
 * expressive shapes and type below. Nothing is lost: the motion scheme the
 * expressive theme would install is the one those components already default
 * to. Swap the entry point when it is made public.
 *
 * @param darkTheme whether to render the dark scheme.
 * @param dynamicColor whether to derive the palette from the wallpaper. Only
 *   honoured on Android 12 and later, where the platform can supply one.
 */
@Composable
public fun BeamTheme(
    darkTheme: Boolean = isSystemInDarkTheme(),
    dynamicColor: Boolean = true,
    content: @Composable () -> Unit,
) {
    val context = LocalContext.current
    val colorScheme =
        when {
            dynamicColor && supportsDynamicColor && darkTheme -> dynamicDarkColorScheme(context)
            dynamicColor && supportsDynamicColor -> dynamicLightColorScheme(context)
            darkTheme -> BeamDarkColors
            else -> BeamLightColors
        }

    MaterialTheme(
        colorScheme = colorScheme,
        shapes = BeamShapes,
        typography = BeamTypography,
        content = content,
    )
}

/** Whether this device can supply a wallpaper-derived palette. */
public val supportsDynamicColor: Boolean
    get() = Build.VERSION.SDK_INT >= Build.VERSION_CODES.S

/** Beam's own scheme, for previews and for the brand-palette preference. */
public fun beamColorScheme(darkTheme: Boolean): ColorScheme = if (darkTheme) BeamDarkColors else BeamLightColors
