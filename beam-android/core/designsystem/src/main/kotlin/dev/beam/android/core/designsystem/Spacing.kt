package dev.beam.android.core.designsystem

import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp

/**
 * The spacing scale, so padding is chosen from a set rather than invented per
 * screen. Every value is a multiple of 4dp, which is what keeps unrelated
 * screens looking like one app.
 */
public object BeamSpacing {
    /** 4dp -- between a label and the thing it labels. */
    public val Tiny: Dp = 4.dp

    /** 8dp -- inside a badge, between chips. */
    public val Small: Dp = 8.dp

    /** 12dp -- between cards in a row. */
    public val Compact: Dp = 12.dp

    /** 16dp -- the default screen gutter. */
    public val Medium: Dp = 16.dp

    /** 24dp -- between sections. */
    public val Large: Dp = 24.dp

    /** 32dp -- above a screen's first heading. */
    public val ExtraLarge: Dp = 32.dp
}

/** Fixed sizes shared across screens. */
public object BeamSizes {
    /** Poster width in a horizontal row. */
    public val PosterWidth: Dp = 132.dp

    /** Poster width in the catalog grid's smallest column. */
    public val GridPosterMinWidth: Dp = 116.dp

    /** Width of a landscape episode or continue-watching thumbnail. */
    public val ThumbnailWidth: Dp = 208.dp

    /** Height of the backdrop behind a detail screen's header. */
    public val BackdropHeight: Dp = 260.dp

    /** The 2:3 ratio posters are authored at. */
    public const val PosterAspectRatio: Float = 2f / 3f

    /** The 16:9 ratio thumbnails and backdrops are authored at. */
    public const val ThumbnailAspectRatio: Float = 16f / 9f
}
