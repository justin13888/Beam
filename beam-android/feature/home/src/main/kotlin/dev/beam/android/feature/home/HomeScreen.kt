package dev.beam.android.feature.home

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.beam.android.core.designsystem.BeamSizes
import dev.beam.android.core.designsystem.BeamSpacing
import dev.beam.android.core.designsystem.component.BeamEmptyState
import dev.beam.android.core.designsystem.component.BeamErrorState
import dev.beam.android.core.designsystem.component.BeamLoading
import dev.beam.android.core.designsystem.component.SectionHeader
import dev.beam.android.core.model.LoadState
import dev.beam.android.core.model.valueOrNull
import dev.beam.android.core.ui.ContinueWatchingCard
import dev.beam.android.core.ui.MediaRow
import uniffi.beam_client_core.ContinueWatchingEntry
import uniffi.beam_client_core.MediaSummary

/** Home, wired to its view model. */
@Composable
public fun HomeRoute(
    onOpenMedia: (String) -> Unit,
    onResume: (ContinueWatchingEntry) -> Unit,
    modifier: Modifier = Modifier,
    viewModel: HomeViewModel = hiltViewModel(),
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    HomeScreen(
        state = state,
        onOpenMedia = onOpenMedia,
        onResume = onResume,
        onRetry = viewModel::refresh,
        modifier = modifier,
    )
}

/** Home, as a function of its state. */
@Composable
internal fun HomeScreen(
    state: LoadState<HomeUiState>,
    onOpenMedia: (String) -> Unit,
    onResume: (ContinueWatchingEntry) -> Unit,
    onRetry: () -> Unit,
    modifier: Modifier = Modifier,
) {
    // A load that has previous content to show keeps showing it. Replacing a
    // populated screen with a spinner on every refresh is a worse experience
    // than a moment of slightly stale content.
    val content = state.valueOrNull

    when {
        content != null && !content.isEmpty -> {
            HomeContent(
                state = content,
                onOpenMedia = onOpenMedia,
                onResume = onResume,
                modifier = modifier,
            )
        }

        state is LoadState.Failure -> {
            BeamErrorState(
                message = state.message,
                onRetry = onRetry.takeIf { state.retryable },
                modifier = modifier,
            )
        }

        state is LoadState.Loading || state is LoadState.Idle -> {
            BeamLoading(modifier)
        }

        else -> {
            BeamEmptyState(
                title = "Nothing here yet",
                description =
                    "Once your libraries have been scanned, what you can watch " +
                        "will appear here.",
                modifier = modifier,
            )
        }
    }
}

@Composable
private fun HomeContent(
    state: HomeUiState,
    onOpenMedia: (String) -> Unit,
    onResume: (ContinueWatchingEntry) -> Unit,
    modifier: Modifier = Modifier,
) {
    LazyColumn(
        modifier = modifier.fillMaxSize(),
        contentPadding = PaddingValues(vertical = BeamSpacing.Medium),
        verticalArrangement = Arrangement.spacedBy(BeamSpacing.Large),
    ) {
        if (state.continueWatching.isNotEmpty()) {
            item(key = "continue-header") {
                SectionHeader(title = "Continue watching")
            }
            item(key = "continue-row") {
                LazyRow(
                    contentPadding = PaddingValues(horizontal = BeamSpacing.Medium),
                    horizontalArrangement = Arrangement.spacedBy(BeamSpacing.Compact),
                ) {
                    items(
                        items = state.continueWatching,
                        key = { it.fileId },
                    ) { entry ->
                        ContinueWatchingCard(
                            entry = entry,
                            onClick = { onResume(entry) },
                            modifier = Modifier.width(BeamSizes.ThumbnailWidth),
                        )
                    }
                }
            }
        }

        mediaRow("Recently added", state.recentlyAdded, onOpenMedia)
        mediaRow("Top rated", state.topRated, onOpenMedia)
    }
}

private fun androidx.compose.foundation.lazy.LazyListScope.mediaRow(
    title: String,
    items: List<MediaSummary>,
    onOpenMedia: (String) -> Unit,
) {
    if (items.isEmpty()) return
    item(key = "$title-header") { SectionHeader(title = title) }
    item(key = "$title-row") {
        MediaRow(items = items, onItemClick = { onOpenMedia(it.id) })
    }
}
