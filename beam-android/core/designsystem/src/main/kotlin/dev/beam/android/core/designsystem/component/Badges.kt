package dev.beam.android.core.designsystem.component

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.unit.dp
import dev.beam.android.core.designsystem.BeamShapeDefaults
import dev.beam.android.core.designsystem.BeamSpacing
import dev.beam.android.core.designsystem.BeamTextStyles

/**
 * A small caption over artwork or beside a title: a codec, a container, a
 * resolution, a year.
 */
@Composable
public fun MetaBadge(
    text: String,
    modifier: Modifier = Modifier,
    container: Color = MaterialTheme.colorScheme.surfaceContainerHighest,
    content: Color = MaterialTheme.colorScheme.onSurfaceVariant,
) {
    Box(
        modifier = modifier
            .clip(BeamShapeDefaults.Badge)
            .background(container)
            .padding(horizontal = BeamSpacing.Small, vertical = BeamSpacing.Tiny),
    ) {
        Text(text = text, style = BeamTextStyles.Badge, color = content)
    }
}

/** A row of [MetaBadge]s that keeps its spacing consistent between screens. */
@Composable
public fun MetaBadgeRow(
    labels: List<String>,
    modifier: Modifier = Modifier,
) {
    Row(
        modifier = modifier,
        horizontalArrangement = Arrangement.spacedBy(BeamSpacing.Small),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        labels.forEach { label -> MetaBadge(text = label) }
    }
}

/**
 * The resume bar along the bottom of a partially-watched tile.
 *
 * Hidden from accessibility because the caller states the position in words --
 * "24 minutes left" is useful, "progress bar, 62 percent" is not.
 *
 * @param progress fraction watched, from 0 to 1.
 */
@Composable
public fun WatchProgressBar(
    progress: Float,
    modifier: Modifier = Modifier,
) {
    Box(
        modifier = modifier
            .fillMaxWidth()
            .height(4.dp)
            .background(MaterialTheme.colorScheme.surfaceContainerHighest)
            .clearAndSetSemantics {},
    ) {
        Box(
            Modifier
                .fillMaxWidth(fraction = progress.coerceIn(0f, 1f))
                .height(4.dp)
                .background(MaterialTheme.colorScheme.primary),
        )
    }
}
