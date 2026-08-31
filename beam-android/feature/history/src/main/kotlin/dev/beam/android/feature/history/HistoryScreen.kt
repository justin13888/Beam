package dev.beam.android.feature.history

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material3.ListItem
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Modifier
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.beam.android.core.designsystem.BeamSizes
import dev.beam.android.core.designsystem.BeamSpacing
import dev.beam.android.core.designsystem.component.Artwork
import dev.beam.android.core.designsystem.component.BeamEmptyState
import dev.beam.android.core.designsystem.component.BeamErrorState
import dev.beam.android.core.designsystem.component.BeamLoading
import dev.beam.android.core.designsystem.component.WatchProgressBar
import dev.beam.android.core.ui.Format
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.filter
import uniffi.beam_client_core.HistoryEntry
import java.text.DateFormat
import java.util.Date

/** History, wired to its view model. */
@Composable
public fun HistoryRoute(
    onOpenMedia: (String) -> Unit,
    modifier: Modifier = Modifier,
    viewModel: HistoryViewModel = hiltViewModel(),
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    HistoryScreen(
        state = state,
        onOpenMedia = onOpenMedia,
        onLoadMore = viewModel::loadMore,
        onRetry = viewModel::refresh,
        modifier = modifier,
    )
}

/** History, as a function of its state. */
@Composable
internal fun HistoryScreen(
    state: HistoryUiState,
    onOpenMedia: (String) -> Unit,
    onLoadMore: () -> Unit,
    onRetry: () -> Unit,
    modifier: Modifier = Modifier,
) {
    when {
        state.entries.isNotEmpty() -> {
            val listState = rememberLazyListState()
            val shouldLoadMore by remember(listState, state.entries.size) {
                derivedStateOf {
                    val last =
                        listState.layoutInfo.visibleItemsInfo
                            .lastOrNull()
                            ?.index
                            ?: return@derivedStateOf false
                    last >= state.entries.size - PREFETCH_DISTANCE
                }
            }
            LaunchedEffect(listState, state.entries.size) {
                snapshotFlow { shouldLoadMore }
                    .distinctUntilChanged()
                    .filter { it && state.hasMore }
                    .collect { onLoadMore() }
            }

            LazyColumn(
                state = listState,
                modifier = modifier.fillMaxSize(),
                contentPadding = PaddingValues(vertical = BeamSpacing.Small),
            ) {
                items(state.entries, key = { "${it.fileId}-${it.updatedAtUnix}" }) { entry ->
                    HistoryRow(entry = entry, onClick = { onOpenMedia(entry.mediaId) })
                }
            }
        }

        state.isLoading -> {
            BeamLoading(modifier)
        }

        state.error != null -> {
            BeamErrorState(
                message = state.error,
                onRetry = onRetry,
                modifier = modifier,
            )
        }

        else -> {
            BeamEmptyState(
                title = "Nothing watched yet",
                description = "What you watch will be listed here so you can pick it back up.",
                modifier = modifier,
            )
        }
    }
}

@Composable
private fun HistoryRow(
    entry: HistoryEntry,
    onClick: () -> Unit,
) {
    ListItem(
        headlineContent = {
            Text(entry.media?.title ?: entry.episode?.title ?: "Unknown title")
        },
        supportingContent = {
            Text(entry.statusLine())
            entry.progressFraction?.takeIf { !entry.completed }?.let { fraction ->
                WatchProgressBar(
                    progress = fraction.toFloat(),
                    modifier = Modifier.fillMaxWidth(),
                )
            }
        },
        leadingContent = {
            Box(modifier = Modifier.width(BeamSizes.PosterWidth / 2)) {
                Artwork(
                    url = entry.media?.posterUrl ?: entry.episode?.thumbnailUrl,
                    aspectRatio = BeamSizes.PosterAspectRatio,
                )
            }
        },
        modifier =
            Modifier
                .fillMaxWidth()
                .clickable(onClick = onClick),
    )
}

/** "Finished · 3 March", or "22 minutes left · 3 March". */
internal fun HistoryEntry.statusLine(): String {
    val watched = DateFormat.getDateInstance().format(Date(updatedAtUnix * MILLIS_PER_SECOND))
    val progress =
        if (completed) {
            "Finished"
        } else {
            Format.remaining(positionSecs, durationSecs).ifEmpty { "In progress" }
        }
    return "$progress · $watched"
}

private const val MILLIS_PER_SECOND = 1_000L
private const val PREFETCH_DISTANCE = 8
