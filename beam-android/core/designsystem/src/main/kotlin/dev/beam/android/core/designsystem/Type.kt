package dev.beam.android.core.designsystem

import androidx.compose.material3.Typography
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.sp

// The platform font, deliberately: it is the one already loaded, it carries
// the user's own font-scale and weight settings, and it covers scripts a
// bundled font would not. Shipping a display face would cost startup time and
// break exactly the accessibility settings a media app is judged on.
private val Family = FontFamily.Default

/**
 * Tightened display and headline styles.
 *
 * The Material defaults are set for text-heavy products. A catalog is mostly
 * titles over artwork, where the default tracking reads as loose, so the
 * large styles are pulled in and weighted up while body and label text keeps
 * the baseline metrics that make long synopses readable.
 */
internal val BeamTypography = Typography().let { base ->
    Typography(
        displayLarge = base.displayLarge.copy(
            fontFamily = Family,
            fontWeight = FontWeight.Bold,
            letterSpacing = (-0.5).sp,
        ),
        displayMedium = base.displayMedium.copy(
            fontFamily = Family,
            fontWeight = FontWeight.Bold,
            letterSpacing = (-0.25).sp,
        ),
        displaySmall = base.displaySmall.copy(fontFamily = Family, fontWeight = FontWeight.Bold),
        headlineLarge = base.headlineLarge.copy(
            fontFamily = Family,
            fontWeight = FontWeight.SemiBold,
            letterSpacing = (-0.25).sp,
        ),
        headlineMedium = base.headlineMedium.copy(
            fontFamily = Family,
            fontWeight = FontWeight.SemiBold,
        ),
        headlineSmall = base.headlineSmall.copy(
            fontFamily = Family,
            fontWeight = FontWeight.SemiBold,
        ),
        titleLarge = base.titleLarge.copy(fontFamily = Family, fontWeight = FontWeight.SemiBold),
        titleMedium = base.titleMedium.copy(fontFamily = Family, fontWeight = FontWeight.SemiBold),
        titleSmall = base.titleSmall.copy(fontFamily = Family, fontWeight = FontWeight.Medium),
        bodyLarge = base.bodyLarge.copy(fontFamily = Family),
        bodyMedium = base.bodyMedium.copy(fontFamily = Family),
        bodySmall = base.bodySmall.copy(fontFamily = Family),
        labelLarge = base.labelLarge.copy(fontFamily = Family, fontWeight = FontWeight.Medium),
        labelMedium = base.labelMedium.copy(fontFamily = Family, fontWeight = FontWeight.Medium),
        labelSmall = base.labelSmall.copy(fontFamily = Family, fontWeight = FontWeight.Medium),
    )
}

/** Styles with no Material role, kept here so they are not redefined per screen. */
public object BeamTextStyles {
    /** The tiny uppercase caption on codec and container badges. */
    public val Badge: TextStyle = TextStyle(
        fontFamily = Family,
        fontWeight = FontWeight.SemiBold,
        fontSize = 11.sp,
        letterSpacing = 0.6.sp,
    )

    /** Centred supporting copy inside an empty or error state. */
    public val EmptyStateBody: TextStyle = TextStyle(
        fontFamily = Family,
        fontSize = 14.sp,
        lineHeight = 20.sp,
        textAlign = TextAlign.Center,
    )
}
