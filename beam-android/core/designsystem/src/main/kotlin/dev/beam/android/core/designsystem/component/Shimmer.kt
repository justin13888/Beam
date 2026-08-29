package dev.beam.android.core.designsystem.component

import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Brush

/**
 * A sweeping highlight for placeholder surfaces.
 *
 * A skeleton that shimmers reads as "this is arriving"; a static grey block
 * reads as "this is broken". The distinction matters most on the home screen,
 * which is the first thing shown on a cold start.
 */
@Composable
public fun Modifier.shimmer(enabled: Boolean = true): Modifier {
    if (!enabled) return this

    val transition = rememberInfiniteTransition(label = "shimmer")
    val progress by transition.animateFloat(
        initialValue = 0f,
        targetValue = 1f,
        animationSpec = infiniteRepeatable(
            animation = tween(durationMillis = 1_400),
            repeatMode = RepeatMode.Restart,
        ),
        label = "shimmerProgress",
    )

    val base = MaterialTheme.colorScheme.surfaceContainerHighest
    val highlight = MaterialTheme.colorScheme.surfaceContainerHigh

    return drawWithContent {
        drawContent()
        // The sweep runs from one full width before the surface to one full
        // width after it, so the highlight enters and leaves rather than
        // appearing in place.
        val travel = size.width * 2f
        val start = -size.width + travel * progress
        drawRect(
            brush = Brush.linearGradient(
                colors = listOf(base, highlight, base),
                start = Offset(start, 0f),
                end = Offset(start + size.width, size.height),
            ),
        )
    }
}
