package dev.beam.android.core.designsystem

import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.ui.graphics.Color

// Beam's own palette, used when dynamic colour is unavailable or the user has
// asked for the brand look. The seed is the cyan `beam-web` already uses for
// its accent, so the two clients read as the same product rather than as two
// apps that happen to share a server.
//
// Tones follow the Material 3 tonal scale: a role's light and dark values are
// the same hue at different tones, which is what keeps contrast ratios correct
// when the scheme flips.

private val BrandPrimaryLight = Color(0xFF006878)
private val BrandPrimaryDark = Color(0xFF56D6F2)

internal val BeamLightColors =
    lightColorScheme(
        primary = BrandPrimaryLight,
        onPrimary = Color(0xFFFFFFFF),
        primaryContainer = Color(0xFFA8EEFF),
        onPrimaryContainer = Color(0xFF001F26),
        inversePrimary = BrandPrimaryDark,
        secondary = Color(0xFF4A6269),
        onSecondary = Color(0xFFFFFFFF),
        secondaryContainer = Color(0xFFCCE7EF),
        onSecondaryContainer = Color(0xFF051F25),
        // A violet third accent, well away from cyan on the wheel, so "watched",
        // "downloaded" and "unplayable" never have to be told apart by shade.
        tertiary = Color(0xFF5B5B7E),
        onTertiary = Color(0xFFFFFFFF),
        tertiaryContainer = Color(0xFFE0E0FF),
        onTertiaryContainer = Color(0xFF181837),
        error = Color(0xFFBA1A1A),
        onError = Color(0xFFFFFFFF),
        errorContainer = Color(0xFFFFDAD6),
        onErrorContainer = Color(0xFF410002),
        background = Color(0xFFF5FAFC),
        onBackground = Color(0xFF171D1F),
        surface = Color(0xFFF5FAFC),
        onSurface = Color(0xFF171D1F),
        surfaceVariant = Color(0xFFDBE4E7),
        onSurfaceVariant = Color(0xFF3F484B),
        surfaceTint = BrandPrimaryLight,
        inverseSurface = Color(0xFF2B3133),
        inverseOnSurface = Color(0xFFECF2F4),
        surfaceContainerLowest = Color(0xFFFFFFFF),
        surfaceContainerLow = Color(0xFFEFF5F7),
        surfaceContainer = Color(0xFFE9EFF1),
        surfaceContainerHigh = Color(0xFFE4EAEC),
        surfaceContainerHighest = Color(0xFFDEE3E5),
        outline = Color(0xFF6F797C),
        outlineVariant = Color(0xFFBFC8CB),
        scrim = Color(0xFF000000),
    )

internal val BeamDarkColors =
    darkColorScheme(
        primary = BrandPrimaryDark,
        onPrimary = Color(0xFF003640),
        primaryContainer = Color(0xFF004E5C),
        onPrimaryContainer = Color(0xFFA8EEFF),
        inversePrimary = BrandPrimaryLight,
        secondary = Color(0xFFB0CBD3),
        onSecondary = Color(0xFF1B343B),
        secondaryContainer = Color(0xFF324B52),
        onSecondaryContainer = Color(0xFFCCE7EF),
        tertiary = Color(0xFFC3C3EB),
        onTertiary = Color(0xFF2C2D4D),
        tertiaryContainer = Color(0xFF434465),
        onTertiaryContainer = Color(0xFFE0E0FF),
        error = Color(0xFFFFB4AB),
        onError = Color(0xFF690005),
        errorContainer = Color(0xFF93000A),
        onErrorContainer = Color(0xFFFFDAD6),
        // Near-black rather than pure black: an OLED-black background makes the
        // elevation overlays Material relies on invisible, and posters bleed into
        // it with no edge.
        background = Color(0xFF0F1416),
        onBackground = Color(0xFFDEE3E5),
        surface = Color(0xFF0F1416),
        onSurface = Color(0xFFDEE3E5),
        surfaceVariant = Color(0xFF3F484B),
        onSurfaceVariant = Color(0xFFBFC8CB),
        surfaceTint = BrandPrimaryDark,
        inverseSurface = Color(0xFFDEE3E5),
        inverseOnSurface = Color(0xFF2B3133),
        surfaceContainerLowest = Color(0xFF0A0F11),
        surfaceContainerLow = Color(0xFF171D1F),
        surfaceContainer = Color(0xFF1B2123),
        surfaceContainerHigh = Color(0xFF262B2E),
        surfaceContainerHighest = Color(0xFF303539),
        outline = Color(0xFF899295),
        outlineVariant = Color(0xFF3F484B),
        scrim = Color(0xFF000000),
    )
