package dev.beam.android.core.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import dev.beam.android.core.designsystem.BeamSizes
import dev.beam.android.core.designsystem.BeamSpacing
import dev.beam.android.core.designsystem.component.Artwork
import dev.beam.android.core.designsystem.component.WatchProgressBar
import uniffi.beam_client_core.MediaKind
import uniffi.beam_client_core.MediaSummary

/**
 * A catalog tile: poster, title, and one line of context.
 *
 * The whole tile carries a single content description rather than letting the
 * poster, title and year each be announced separately -- a screen reader
 * moving through a grid should hear one thing per title, not three.
 *
 * @param progress fraction watched, shown as a bar across the poster, or null
 *   for a title that has not been started.
 */
@Composable
public fun MediaCard(
    media: MediaSummary,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    progress: Float? = null,
) {
    val subtitle = when (media.kind) {
        MediaKind.MOVIE -> listOfNotNull(
            media.year?.toString(),
            Format.runtime(media.runtimeMinutes).takeIf { it.isNotEmpty() },
        ).joinToString(" · ")

        MediaKind.SHOW -> {
            val seasons = media.seasonCount.toInt()
            if (seasons == 1) "1 season" else "$seasons seasons"
        }
    }

    Column(
        // Width is the caller's decision: a row wants a fixed poster width, a
        // grid wants to fill its cell.
        modifier = modifier
            .clickable(onClick = onClick)
            .semantics(mergeDescendants = true) {
                contentDescription = listOf(media.title, subtitle)
                    .filter { it.isNotBlank() }
                    .joinToString(", ")
            }
            .padding(bottom = BeamSpacing.Small),
    ) {
        Box(contentAlignment = Alignment.BottomStart) {
            Artwork(url = media.posterUrl, aspectRatio = BeamSizes.PosterAspectRatio)
            if (progress != null) {
                WatchProgressBar(progress = progress, modifier = Modifier.fillMaxWidth())
            }
        }
        Text(
            text = media.title,
            style = MaterialTheme.typography.titleSmall,
            maxLines = 2,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.padding(top = BeamSpacing.Small),
        )
        if (subtitle.isNotBlank()) {
            Text(
                text = subtitle,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}
