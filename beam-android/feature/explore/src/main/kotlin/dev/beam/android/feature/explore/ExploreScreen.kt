package dev.beam.android.feature.explore

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Close
import androidx.compose.material.icons.rounded.Search
import androidx.compose.material.icons.rounded.SwapVert
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.beam.android.core.designsystem.BeamSpacing
import dev.beam.android.core.designsystem.component.BeamEmptyState
import dev.beam.android.core.designsystem.component.BeamErrorState
import dev.beam.android.core.designsystem.component.BeamLoading
import dev.beam.android.core.ui.MediaGrid
import uniffi.beam_client_core.MediaSortField
import uniffi.beam_client_core.MediaTypeFilter
import uniffi.beam_client_core.SortOrder

/** Explore, wired to its view model. */
@Composable
public fun ExploreRoute(
    onOpenMedia: (String) -> Unit,
    modifier: Modifier = Modifier,
    viewModel: ExploreViewModel = hiltViewModel(),
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    ExploreScreen(
        state = state,
        onQueryChange = viewModel::onQueryChange,
        onGenreChange = viewModel::onGenreChange,
        onMediaTypeChange = viewModel::onMediaTypeChange,
        onSortChange = viewModel::onSortChange,
        onClearFilters = viewModel::clearFilters,
        onLoadMore = viewModel::loadMore,
        onRetry = viewModel::reload,
        onOpenMedia = onOpenMedia,
        modifier = modifier,
    )
}

/** Explore, as a function of its state. */
@Composable
internal fun ExploreScreen(
    state: ExploreUiState,
    onQueryChange: (String) -> Unit,
    onGenreChange: (String?) -> Unit,
    onMediaTypeChange: (MediaTypeFilter?) -> Unit,
    onSortChange: (MediaSortField, SortOrder) -> Unit,
    onClearFilters: () -> Unit,
    onLoadMore: () -> Unit,
    onRetry: () -> Unit,
    onOpenMedia: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize()) {
        OutlinedTextField(
            value = state.query,
            onValueChange = onQueryChange,
            singleLine = true,
            placeholder = { Text("Search titles") },
            leadingIcon = { Icon(Icons.Rounded.Search, contentDescription = null) },
            trailingIcon = {
                if (state.query.isNotEmpty()) {
                    IconButton(onClick = { onQueryChange("") }) {
                        Icon(Icons.Rounded.Close, contentDescription = "Clear search")
                    }
                }
            },
            modifier =
                Modifier
                    .fillMaxWidth()
                    .padding(horizontal = BeamSpacing.Medium, vertical = BeamSpacing.Small),
        )

        FilterBar(
            state = state,
            onGenreChange = onGenreChange,
            onMediaTypeChange = onMediaTypeChange,
            onSortChange = onSortChange,
            onClearFilters = onClearFilters,
        )

        // A thin bar rather than replacing the grid: a filter change should
        // not blank out results the viewer is still reading, and the new
        // results replace them the moment they arrive.
        if (state.isLoading && state.items.isNotEmpty()) {
            LinearProgressIndicator(modifier = Modifier.fillMaxWidth())
        }

        Box(modifier = Modifier.fillMaxSize()) {
            when {
                state.items.isNotEmpty() -> {
                    MediaGrid(
                        items = state.items,
                        onItemClick = { onOpenMedia(it.id) },
                        onLoadMore = onLoadMore.takeIf { state.hasMore },
                    )
                }

                state.isLoading -> {
                    BeamLoading()
                }

                state.error != null -> {
                    BeamErrorState(
                        message = state.error,
                        onRetry = onRetry,
                    )
                }

                state.hasFilters -> {
                    BeamEmptyState(
                        title = "No matches",
                        description = "Nothing in your libraries matches these filters.",
                        actionLabel = "Clear filters",
                        onAction = onClearFilters,
                    )
                }

                else -> {
                    BeamEmptyState(
                        title = "Nothing to explore yet",
                        description =
                            "Once your libraries have been scanned, everything " +
                                "you can watch will be here.",
                    )
                }
            }

            if (state.isLoadingMore) {
                LinearProgressIndicator(
                    modifier =
                        Modifier
                            .align(Alignment.BottomCenter)
                            .fillMaxWidth(),
                )
            }
        }
    }
}

@Composable
private fun FilterBar(
    state: ExploreUiState,
    onGenreChange: (String?) -> Unit,
    onMediaTypeChange: (MediaTypeFilter?) -> Unit,
    onSortChange: (MediaSortField, SortOrder) -> Unit,
    onClearFilters: () -> Unit,
) {
    LazyRow(
        contentPadding = PaddingValues(horizontal = BeamSpacing.Medium),
        horizontalArrangement = Arrangement.spacedBy(BeamSpacing.Small),
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier.fillMaxWidth(),
    ) {
        item(key = "sort") {
            SortChip(state = state, onSortChange = onSortChange)
        }
        item(key = "type-movie") {
            FilterChip(
                selected = state.mediaType == MediaTypeFilter.MOVIE,
                onClick = {
                    onMediaTypeChange(
                        MediaTypeFilter.MOVIE.takeIf { state.mediaType != it },
                    )
                },
                label = { Text("Films") },
            )
        }
        item(key = "type-show") {
            FilterChip(
                selected = state.mediaType == MediaTypeFilter.SHOW,
                onClick = {
                    onMediaTypeChange(
                        MediaTypeFilter.SHOW.takeIf { state.mediaType != it },
                    )
                },
                label = { Text("Series") },
            )
        }
        items(state.genres, key = { "genre-$it" }) { genre ->
            FilterChip(
                selected = state.genre == genre,
                onClick = { onGenreChange(genre.takeIf { state.genre != it }) },
                label = { Text(genre) },
            )
        }
        if (state.hasFilters) {
            item(key = "clear") {
                FilterChip(
                    selected = false,
                    onClick = onClearFilters,
                    label = { Text("Clear") },
                    leadingIcon = { Icon(Icons.Rounded.Close, contentDescription = null) },
                )
            }
        }
    }
}

@Composable
private fun SortChip(
    state: ExploreUiState,
    onSortChange: (MediaSortField, SortOrder) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }

    Box {
        FilterChip(
            selected = false,
            onClick = { expanded = true },
            label = { Text(state.sortBy.label()) },
            leadingIcon = { Icon(Icons.Rounded.SwapVert, contentDescription = null) },
        )
        DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            MediaSortField.entries.forEach { field ->
                DropdownMenuItem(
                    text = { Text(field.label()) },
                    onClick = {
                        // Selecting the field already in use reverses it,
                        // which is what a viewer expects from a sort control
                        // and saves a separate direction toggle.
                        val order = if (field == state.sortBy) state.sortOrder.flipped() else field.defaultOrder()
                        onSortChange(field, order)
                        expanded = false
                    },
                )
            }
        }
    }
}

internal fun MediaSortField.label(): String =
    when (this) {
        MediaSortField.TITLE -> "Title"
        MediaSortField.YEAR -> "Year"
        MediaSortField.RATING -> "Rating"
        MediaSortField.DATE_ADDED -> "Recently added"
        MediaSortField.RUNTIME -> "Runtime"
    }

/**
 * The direction a field is most useful in first.
 *
 * Alphabetical A-Z, but newest and highest-rated first: sorting by rating and
 * being shown the worst titles is never what was meant.
 */
internal fun MediaSortField.defaultOrder(): SortOrder =
    when (this) {
        MediaSortField.TITLE -> SortOrder.ASCENDING
        MediaSortField.RUNTIME -> SortOrder.ASCENDING
        MediaSortField.YEAR -> SortOrder.DESCENDING
        MediaSortField.RATING -> SortOrder.DESCENDING
        MediaSortField.DATE_ADDED -> SortOrder.DESCENDING
    }

internal fun SortOrder.flipped(): SortOrder =
    if (this ==
        SortOrder.ASCENDING
    ) {
        SortOrder.DESCENDING
    } else {
        SortOrder.ASCENDING
    }
