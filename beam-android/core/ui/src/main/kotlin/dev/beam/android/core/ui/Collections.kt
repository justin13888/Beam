package dev.beam.android.core.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyGridState
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.rememberLazyGridState
import androidx.compose.foundation.lazy.items
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Modifier
import dev.beam.android.core.designsystem.BeamSizes
import dev.beam.android.core.designsystem.BeamSpacing
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.filter
import uniffi.beam_client_core.MediaSummary
import androidx.compose.foundation.lazy.grid.items as gridItems

/** A horizontally-scrolling row of catalog tiles. */
@Composable
public fun MediaRow(
    items: List<MediaSummary>,
    onItemClick: (MediaSummary) -> Unit,
    modifier: Modifier = Modifier,
) {
    LazyRow(
        modifier = modifier,
        contentPadding = PaddingValues(horizontal = BeamSpacing.Medium),
        horizontalArrangement = Arrangement.spacedBy(BeamSpacing.Compact),
    ) {
        items(items = items, key = { it.id }) { media ->
            MediaCard(
                media = media,
                onClick = { onItemClick(media) },
                modifier = Modifier.width(BeamSizes.PosterWidth),
            )
        }
    }
}

/**
 * The catalog grid.
 *
 * Columns are sized rather than counted, so a phone, an unfolded foldable and
 * a tablet each get as many as fit at a legible poster width instead of the
 * same number stretched.
 *
 * @param onLoadMore called once when the end comes into view. Paging is driven
 *   from scroll position rather than from an "end of list" item so the next
 *   page is already arriving by the time the user reaches it.
 */
@Composable
public fun MediaGrid(
    items: List<MediaSummary>,
    onItemClick: (MediaSummary) -> Unit,
    modifier: Modifier = Modifier,
    state: LazyGridState = rememberLazyGridState(),
    onLoadMore: (() -> Unit)? = null,
    contentPadding: PaddingValues = PaddingValues(BeamSpacing.Medium),
) {
    if (onLoadMore != null) {
        val shouldLoadMore by remember(state, items.size) {
            derivedStateOf {
                val last =
                    state.layoutInfo.visibleItemsInfo
                        .lastOrNull()
                        ?.index ?: return@derivedStateOf false
                // Two rows of slack, so the request is in flight before the
                // user can see that there is nothing below.
                last >= items.size - PrefetchDistance
            }
        }
        LaunchedEffect(state, items.size) {
            snapshotFlow { shouldLoadMore }
                .distinctUntilChanged()
                .filter { it }
                .collect { onLoadMore() }
        }
    }

    LazyVerticalGrid(
        columns = GridCells.Adaptive(minSize = BeamSizes.GridPosterMinWidth),
        modifier = modifier,
        state = state,
        contentPadding = contentPadding,
        horizontalArrangement = Arrangement.spacedBy(BeamSpacing.Compact),
        verticalArrangement = Arrangement.spacedBy(BeamSpacing.Medium),
    ) {
        gridItems(items = items, key = { it.id }) { media ->
            MediaCard(
                media = media,
                onClick = { onItemClick(media) },
                modifier = Modifier.fillMaxWidth(),
            )
        }
    }
}

private const val PrefetchDistance = 8
