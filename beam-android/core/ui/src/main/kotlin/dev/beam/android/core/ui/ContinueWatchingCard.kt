package dev.beam.android.core.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Tv
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
import uniffi.beam_client_core.ContinueWatchingEntry

/**
 * A resume tile: a landscape still with a progress bar and time remaining.
 *
 * Landscape rather than a poster because these are episodes as often as films,
 * and because the point of the row is "carry on from here", which an
 * episode still communicates better than cover art.
 *
 * The entry may not have hydrated -- the playback endpoints return bare
 * identifiers -- so this renders sensibly with `media` unset rather than
 * dropping the row. The user's place is valid whether or not its poster
 * loaded.
 */
@Composable
public fun ContinueWatchingCard(
    entry: ContinueWatchingEntry,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val title = entry.media?.title ?: "Continue watching"
    val subtitle =
        entry.episode?.let { episode ->
            listOfNotNull(
                episode.title.takeIf { it.isNotBlank() },
                Format.remaining(entry.positionSecs, entry.durationSecs).takeIf { it.isNotEmpty() },
            ).joinToString(" · ")
        } ?: Format.remaining(entry.positionSecs, entry.durationSecs)

    Column(
        modifier =
            modifier
                .width(BeamSizes.ThumbnailWidth)
                .clickable(onClick = onClick)
                .semantics(mergeDescendants = true) {
                    contentDescription = "Resume $title, $subtitle"
                }.padding(bottom = BeamSpacing.Small),
    ) {
        Box(contentAlignment = Alignment.BottomStart) {
            Artwork(
                url =
                    entry.episode?.thumbnailUrl ?: entry.media?.backdropUrl
                        ?: entry.media?.posterUrl,
                aspectRatio = BeamSizes.ThumbnailAspectRatio,
                fallbackIcon = Icons.Rounded.Tv,
            )
            WatchProgressBar(
                progress = entry.progressFraction?.toFloat() ?: 0f,
                modifier = Modifier.fillMaxWidth(),
            )
        }
        Text(
            text = title,
            style = MaterialTheme.typography.titleSmall,
            maxLines = 1,
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
