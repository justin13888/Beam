package dev.beam.android.core.designsystem

import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Shapes
import androidx.compose.ui.unit.dp

/**
 * Rounder than the Material baseline, which is the most legible part of the
 * expressive direction: the shapes carry the personality, so the colour does
 * not have to shout.
 */
internal val BeamShapes = Shapes(
    extraSmall = RoundedCornerShape(6.dp),
    small = RoundedCornerShape(10.dp),
    medium = RoundedCornerShape(16.dp),
    large = RoundedCornerShape(24.dp),
    extraLarge = RoundedCornerShape(32.dp),
)

/** Shapes for parts of the UI that are not Material components. */
public object BeamShapeDefaults {
    /** Posters and backdrops. */
    public val Artwork: RoundedCornerShape = RoundedCornerShape(14.dp)

    /** Small pills: ratings, codec badges, download state. */
    public val Badge: RoundedCornerShape = RoundedCornerShape(8.dp)

    /** The sheet the source picker and filters rise in. */
    public val Sheet: RoundedCornerShape = RoundedCornerShape(topStart = 28.dp, topEnd = 28.dp)
}
