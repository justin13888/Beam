package dev.beam.android.feature.settings

import dev.beam.android.core.model.PaletteSource
import dev.beam.android.core.model.QualityPreference
import dev.beam.android.core.model.ThemeMode

internal fun ThemeMode.label(): String =
    when (this) {
        ThemeMode.System -> "Follow the system"
        ThemeMode.Light -> "Light"
        ThemeMode.Dark -> "Dark"
    }

internal fun PaletteSource.label(): String =
    when (this) {
        PaletteSource.Dynamic -> "From your wallpaper"
        PaletteSource.Brand -> "Beam's own colours"
    }

internal fun QualityPreference.label(): String =
    when (this) {
        QualityPreference.Best -> "Best this device can play"
        QualityPreference.MatchScreen -> "Match this screen"
        QualityPreference.Smallest -> "Smallest file"
    }
